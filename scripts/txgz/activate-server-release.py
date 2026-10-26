"""Atomically activate one already-validated PMMan server release on txgz.

This program deliberately does not build anything. It is called by the local
publisher after the release manifest and every payload have been uploaded to
a private staging directory. It stops only the project-owned units, keeps a
rollback snapshot, and never touches the host's unrelated WireGuard, routing,
DNS, BIRD, or web-site services.
"""
import hashlib
import fcntl
import json
import os
import pathlib
import shutil
import subprocess
import sys
import time
import urllib.request
import uuid

ROOT = pathlib.Path('/opt/pmman-network-dev')
BIN_DIR = ROOT / 'bin'
RESOURCE_DIR = ROOT / 'node-resources'
LEGACY_PORT_FORWARD_DB = ROOT / 'port-forward.db'
BACKUP_ROOT = ROOT / 'release-backups'
COMPOSE_FILE = ROOT / 'docker-compose.yml'
COMPOSE_ENV = ROOT / '.env'
COMPOSE_PROJECT = 'pmman-network'


def fail(message):
    raise RuntimeError(message)


def run(*args, check=True):
    result = subprocess.run(args, text=True, capture_output=True)
    if check and result.returncode:
        detail = (result.stderr or result.stdout).strip()
        fail(f"{' '.join(args)} failed: {detail}")
    return result


def safe_name(value):
    path = pathlib.PurePosixPath(value)
    if not value or path.name != value or value in ('.', '..'):
        fail(f'invalid release file name: {value!r}')
    return value


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def verify_payload(stage, entry):
    name = safe_name(entry['fileName'])
    path = stage / name
    if not path.is_file():
        fail(f'missing staged payload: {name}')
    actual_size = path.stat().st_size
    actual_hash = digest(path)
    if actual_size != int(entry['size']) or actual_hash.lower() != entry['sha256'].lower():
        fail(f'payload integrity mismatch: {name}')
    return path


def copy_if_exists(source, destination):
    if source.is_file():
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)


def snapshot(previous):
    previous.mkdir(parents=True, exist_ok=True)
    for source, name in (
        (BIN_DIR / 'pmman-control-plane', 'pmman-control-plane'),
        (BIN_DIR / 'pmman-migrate', 'pmman-migrate'),
        (BIN_DIR / 'pmman-node', 'pmman-node'),
        (RESOURCE_DIR / 'manifest.json', 'node-manifest.json'),
        (ROOT / 'server-release.json', 'server-release.json'),
        (ROOT / 'node-registry.json', 'node-registry.json'),
        (ROOT / 'user-registry.json', 'user-registry.json'),
        # Snapshot for manual recovery only. Neither promotion nor rollback
        # overwrites live administrator settings with an artifact/default.
        (ROOT / 'network-settings.json', 'network-settings.json'),
        (ROOT / 'master.env', 'master.env'),
        (LEGACY_PORT_FORWARD_DB, 'port-forward.db'),
        (COMPOSE_FILE, 'docker-compose.yml'),
        (COMPOSE_ENV, '.env'),
    ):
        copy_if_exists(source, previous / name)
    manifest_path = RESOURCE_DIR / 'manifest.json'
    if manifest_path.is_file():
        try:
            old = json.loads(manifest_path.read_text(encoding='utf-8'))
            old_name = old.get('fileName')
            if isinstance(old_name, str) and old_name == pathlib.PurePosixPath(old_name).name:
                copy_if_exists(RESOURCE_DIR / old_name, previous / 'old-node-resource')
                (previous / 'old-node-resource.name').write_text(old_name, encoding='utf-8')
        except (OSError, ValueError):
            # A malformed old manifest will be replaced by the new validated
            # one; the executable backups are still enough to recover service.
            pass


def restore(previous):
    for name, destination in (
        ('pmman-control-plane', BIN_DIR / 'pmman-control-plane'),
        ('pmman-migrate', BIN_DIR / 'pmman-migrate'),
        ('pmman-node', BIN_DIR / 'pmman-node'),
        ('node-manifest.json', RESOURCE_DIR / 'manifest.json'),
        ('server-release.json', ROOT / 'server-release.json'),
        ('docker-compose.yml', COMPOSE_FILE),
        ('.env', COMPOSE_ENV),
        ('master.env', ROOT / 'master.env'),
    ):
        source = previous / name
        if source.is_file():
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
        elif destination in (COMPOSE_FILE, COMPOSE_ENV):
            # A legacy host-service deployment has no Compose files. Remove
            # files generated during a failed migration instead of leaving a
            # half-created stack behind.
            destination.unlink(missing_ok=True)
    old_name_file = previous / 'old-node-resource.name'
    old_resource = previous / 'old-node-resource'
    if old_name_file.is_file() and old_resource.is_file():
        old_name = safe_name(old_name_file.read_text(encoding='utf-8').strip())
        shutil.copy2(old_resource, RESOURCE_DIR / old_name)


def wait_for_postgres(user, database, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = run('docker', 'compose', '--project-name', COMPOSE_PROJECT,
                     '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
                     'exec', '-T', 'postgres', 'pg_isready', '-U', user,
                     '-d', database, '-h', '127.0.0.1', '-p', '5432', check=False)
        if result.returncode == 0:
            return
        time.sleep(1)
    fail('PostgreSQL did not become ready before migration')


def legacy_migration_completed(user, database):
    """Return whether the one-time pre-PostgreSQL import already ran.

    Server releases carry the migration binary for first-time deployments, but
    normal upgrades must not replay the legacy JSON/SQLite snapshot over the
    live PostgreSQL documents. The migration writes this audit event in the
    same transaction as the imported data, so it is a durable completion mark.
    """
    result = run(
        'docker', 'compose', '--project-name', COMPOSE_PROJECT,
        '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
        'exec', '-T', 'postgres', 'psql', '-At', '-U', user, '-d', database,
        '-h', '127.0.0.1', '-p', '5432', '-c',
        "SELECT 1 FROM pmman_audit_events "
        "WHERE event_type = 'legacy-import.completed' LIMIT 1",
        check=False,
    )
    return result.returncode == 0 and result.stdout.strip() == '1'


def backup_postgres(previous, user, database):
    # pg_dump --clean only drops objects present in the OLD dump. Remember
    # that schema so rollback also removes tables introduced by an upgrade.
    tables = public_tables(user, database)
    write_atomic(previous / 'postgres-tables.json', json.dumps(tables) + '\n', 0o600)
    dump = previous / 'postgres.sql'
    temporary = dump.with_suffix('.sql.tmp')
    try:
        with temporary.open('wb') as stream:
            result = subprocess.run(
                ['docker', 'compose', '--project-name', COMPOSE_PROJECT,
                 '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
                 'exec', '-T', 'postgres', 'pg_dump', '--clean', '--if-exists',
                 '--no-owner', '--no-privileges', '-U', user, '-d', database,
                 '-h', '127.0.0.1', '-p', '5432'],
                stdout=stream, stderr=subprocess.PIPE,
            )
    except Exception:
        temporary.unlink(missing_ok=True)
        raise
    if result.returncode:
        temporary.unlink(missing_ok=True)
        detail = result.stderr.decode(errors='replace').strip()
        fail(f'PostgreSQL backup failed: {detail}')
    os.replace(temporary, dump)
    os.chmod(dump, 0o600)


def public_tables(user, database):
    result = run('docker', 'compose', '--project-name', COMPOSE_PROJECT,
                 '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
                 'exec', '-T', 'postgres', 'psql', '-At', '-U', user,
                 '-d', database, '-h', '127.0.0.1', '-p', '5432', '-c',
                 "SELECT COALESCE(json_agg(tablename ORDER BY tablename), '[]'::json) "
                 "FROM pg_tables WHERE schemaname='public'")
    return json.loads(result.stdout)


def restore_postgres(previous, user, database):
    dump = previous / 'postgres.sql'
    if not dump.is_file():
        return
    run('docker', 'compose', '--project-name', COMPOSE_PROJECT,
        '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
        'stop', 'master')
    tables_path = previous / 'postgres-tables.json'
    if tables_path.is_file():
        baseline = set(json.loads(tables_path.read_text(encoding='utf-8')))
        added = set(public_tables(user, database)) - baseline
        if added:
            # This is the project's dedicated database, not host networking
            # or another application's schema. Quote identifiers defensively.
            names = ','.join('public."' + name.replace('"', '""') + '"'
                             for name in sorted(added))
            run('docker', 'compose', '--project-name', COMPOSE_PROJECT,
                '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
                'exec', '-T', 'postgres', 'psql', '-v', 'ON_ERROR_STOP=1',
                '-U', user, '-d', database, '-h', '127.0.0.1', '-p', '5432',
                '-c', f'DROP TABLE {names} CASCADE')
    with dump.open('rb') as stream:
        result = subprocess.run(
            ['docker', 'compose', '--project-name', COMPOSE_PROJECT,
             '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
             'exec', '-T', 'postgres', 'psql', '-v', 'ON_ERROR_STOP=1',
             '-U', user, '-d', database, '-h', '127.0.0.1', '-p', '5432'],
            stdin=stream, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
    if result.returncode:
        detail = result.stderr.decode(errors='replace').strip()
        fail(f'PostgreSQL rollback failed: {detail}')


def write_atomic(path, content, mode=0o644):
    temporary = path.with_name(f'.{path.name}.tmp.{os.getpid()}')
    temporary.write_text(content, encoding='utf-8')
    os.chmod(temporary, mode)
    os.replace(temporary, path)


def env_value(name):
    if not COMPOSE_ENV.is_file():
        return None
    for line in COMPOSE_ENV.read_text(encoding='utf-8').splitlines():
        if line.startswith(name + '='):
            return line.split('=', 1)[1]
    return None


def api(path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    headers = {'Content-Type': 'application/json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    request = urllib.request.Request('http://127.0.0.1:8787' + path, data=data, headers=headers)
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.load(response)


def admin_token():
    credentials = json.loads((ROOT / 'credentials.json').read_text())
    return api('/v1/auth/login', body={
        'username': credentials['admin_user'], 'password': credentials['admin_password'],
    })['token']


def healthy(version=None):
    if not COMPOSE_FILE.is_file() or not COMPOSE_ENV.is_file():
        return False
    try:
        with urllib.request.urlopen('http://127.0.0.1:8787/healthz', timeout=3) as response:
            if response.read().strip() != b'ok':
                return False
        if version is None:
            return True
        if api('/v1/version')['version'] != version:
            return False
        return True
    except Exception:
        return False


def restart_units():
    run('docker', 'compose', '--project-name', COMPOSE_PROJECT,
        '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE), 'up', '-d')


def main():
    if len(sys.argv) not in (3, 4):
        fail('usage: activate-server-release.py STAGING_DIR RELEASE_VERSION [--bootstrap]')
    stage = pathlib.Path(sys.argv[1]).resolve()
    if stage.parent != (ROOT / '.staging').resolve() or not stage.name.startswith('server-'):
        fail('staging directory is outside the project staging root')
    lock = (ROOT / 'release.lock').open('a')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        fail('another server publication is running')
    version = sys.argv[2]
    bootstrap = len(sys.argv) == 4 and sys.argv[3] == '--bootstrap'
    manifest_path = stage / 'release.json'
    if not manifest_path.is_file():
        fail('staged release.json is missing')
    manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
    if manifest.get('product') != 'pmman-network-server':
        fail('release product mismatch')
    if manifest.get('releaseVersion') != version:
        fail('release version mismatch')
    if manifest.get('target') != 'x86_64-unknown-linux-musl':
        fail('release target mismatch')
    master = verify_payload(stage, manifest['master'])
    master_image = verify_payload(stage, manifest['masterImage'])
    migrator = verify_payload(stage, manifest['migrator'])
    node = verify_payload(stage, manifest['node'])
    resource = verify_payload(stage, manifest['nodeResource'])
    resource_name = safe_name(manifest['nodeResource']['fileName'])
    for helper in manifest['helpers']:
        verify_payload(stage, helper)

    now = time.strftime('%Y%m%d-%H%M%S', time.gmtime())
    previous = BACKUP_ROOT / f'{now}-{version}-{uuid.uuid4().hex[:8]}'
    had_compose = COMPOSE_FILE.is_file() and COMPOSE_ENV.is_file()
    legacy_master_active = subprocess.run(
        ['systemctl', 'is-active', '--quiet', 'pmman-dev-master.service'],
        check=False,
    ).returncode == 0
    snapshot(previous)
    stopped = False
    try:
        run('docker', 'load', '--input', str(master_image))
        stopped = True
        if had_compose:
            # Quiesce the old writer before taking the database backup. A
            # schema-changing upgrade must be able to roll back to exactly
            # that state, not discard registrations/configuration accepted
            # while pg_dump or the migration was running. Nodes stay running.
            run('docker', 'compose', '--project-name', COMPOSE_PROJECT,
                '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
                'stop', 'master')
        BIN_DIR.mkdir(parents=True, exist_ok=True)
        RESOURCE_DIR.mkdir(parents=True, exist_ok=True)
        # Keep the binary in the rollback snapshot for operators, but the
        # running master is now always the locally loaded container image.
        os.replace(master, BIN_DIR / 'pmman-control-plane')
        os.replace(migrator, BIN_DIR / 'pmman-migrate')
        os.replace(node, BIN_DIR / 'pmman-node')
        os.replace(resource, RESOURCE_DIR / resource_name)
        os.chmod(BIN_DIR / 'pmman-control-plane', 0o755)
        os.chmod(BIN_DIR / 'pmman-migrate', 0o755)
        os.chmod(BIN_DIR / 'pmman-node', 0o755)
        os.chmod(RESOURCE_DIR / resource_name, 0o755)
        release = {
            'version': version,
            'fileName': resource_name,
            'sha256': manifest['nodeResource']['sha256'].lower(),
            'size': int(manifest['nodeResource']['size']),
            'publishedAt': manifest['builtAt'],
        }
        write_atomic(RESOURCE_DIR / 'manifest.json', json.dumps(release, indent=2) + '\n')
        # Add the resource directory without regenerating existing credentials
        # or administrator-owned node/network settings on normal upgrades.
        env_path = ROOT / 'master.env'
        if env_path.is_file():
            env_text = env_path.read_text(encoding='utf-8')
            if not any(line.startswith('PMMAN_GEOSITE_DIR=') for line in env_text.splitlines()):
                geosite_dir = ROOT / 'geosite'
                geosite_dir.mkdir(mode=0o700, exist_ok=True)
                write_atomic(env_path, env_text.rstrip() + f'\nPMMAN_GEOSITE_DIR={geosite_dir}\n', 0o600)
            env_text = env_path.read_text(encoding='utf-8')
            if not any(line.startswith('PMMAN_DIAGNOSTIC_DIR=') for line in env_text.splitlines()):
                diagnostic_dir = ROOT / 'diagnostics'
                diagnostic_dir.mkdir(mode=0o700, exist_ok=True)
                write_atomic(env_path, env_text.rstrip() + f'\nPMMAN_DIAGNOSTIC_DIR={diagnostic_dir}\n', 0o600)
        helper_dir = ROOT / 'scripts' / 'txgz'
        helper_dir.mkdir(parents=True, exist_ok=True)
        for name in ('provision.py', 'probe.py'):
            shutil.copy2(stage / name, helper_dir / name)
        run('env', f'PMMAN_MASTER_IMAGE=pmman-network/master:{version}',
            'PMMAN_SKIP_MASTER=1',
            'python3', str(helper_dir / 'provision.py'))
        run('docker', 'compose', '--project-name', COMPOSE_PROJECT,
            '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
            'up', '-d', 'postgres')
        db_user = env_value('PMMAN_POSTGRES_USER') or 'pmman'
        db_name = env_value('PMMAN_POSTGRES_DB') or 'pmman'
        db_password = env_value('PMMAN_POSTGRES_PASSWORD')
        if not db_password:
            fail('PMMAN_POSTGRES_PASSWORD missing after provisioning')
        wait_for_postgres(db_user, db_name)
        backup_postgres(previous, db_user, db_name)
        if not legacy_migration_completed(db_user, db_name):
            database_url = f'postgres://{db_user}:{db_password}@127.0.0.1:55432/{db_name}'
            run(str(BIN_DIR / 'pmman-migrate'), database_url, str(ROOT))
        restart_units()
        deadline = time.monotonic() + 35
        updates_requested = False
        while time.monotonic() < deadline:
            if not updates_requested and healthy():
                # Production publishing owns the master and its resource store;
                # node installation/update is an explicit administrator action.
                updates_requested = True
            if updates_requested and healthy(version):
                write_atomic(ROOT / 'server-release.json', json.dumps(manifest, indent=2) + '\n')
                print(json.dumps({'activated': True, 'version': version, 'backup': str(previous)}))
                shutil.rmtree(stage, ignore_errors=True)
                return 0
            time.sleep(1)
        fail('new master/node release did not become healthy')
    except Exception:
        if stopped:
            try:
                db_user = env_value('PMMAN_POSTGRES_USER') or 'pmman'
                db_name = env_value('PMMAN_POSTGRES_DB') or 'pmman'
                restore_postgres(previous, db_user, db_name)
                if not had_compose and COMPOSE_FILE.is_file():
                    run('docker', 'compose', '--project-name', COMPOSE_PROJECT,
                        '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
                        'down', check=False)
                restore(previous)
                if had_compose:
                    restart_units()
                else:
                    # provision.py stops the legacy service before generating
                    # the new Compose stack. On rollback, remove that stack
                    # and put the original host service back exactly as it was.
                    if COMPOSE_FILE.is_file():
                        run('docker', 'compose', '--project-name', COMPOSE_PROJECT,
                            '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE),
                            'down', check=False)
                    if legacy_master_active:
                        run('systemctl', 'start', 'pmman-dev-master.service')
                deadline = time.monotonic() + 35
                while not healthy() and time.monotonic() < deadline:
                    time.sleep(1)
                if not healthy():
                    fail('previous release did not recover')
                print(json.dumps({'rolledBack': True, 'backup': str(previous)}), file=sys.stderr)
            except Exception as rollback_error:
                print(f'rollback failed: {rollback_error}', file=sys.stderr)
        raise


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f'activation failed: {error}', file=sys.stderr)
        raise SystemExit(1)
