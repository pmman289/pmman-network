#!/usr/bin/env python3
"""Explicit SSH maintenance promotion of ONE txgz-managed node; no compilation.

Master publication never calls this automatically. Does not alter node credentials,
configuration, WireGuard/BIRD settings or the desired versions of other nodes.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time

ROOT = Path('/opt/pmman-network-dev')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--node-id', required=True)
    parser.add_argument('--version', required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,63}', args.node_id):
        raise RuntimeError('invalid node ID')
    lock = (ROOT / 'release.lock').open('a')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    manifest = json.loads((ROOT / 'server-release.json').read_text())
    if manifest['releaseVersion'] != args.version:
        raise RuntimeError('publish and verify the matching master/resource first')
    source = ROOT / 'bin/pmman-node'
    if digest(source) != manifest['node']['sha256']:
        raise RuntimeError('node release checksum mismatch')
    directory = ROOT / 'nodes' / args.node_id
    if directory.resolve() != directory:
        raise RuntimeError('node directory must not be a symlink')
    config_file = directory / 'node.env'
    config = dict(line.split('=', 1) for line in config_file.read_text().splitlines()
                  if line and not line.startswith('#') and '=' in line)
    binary = directory / 'pmman-node'
    service = f'pmman-dev-node-{args.node_id}.service'
    if config.get('PMMAN_NODE_BINARY') != str(binary) or config.get('PMMAN_NODE_ID') != args.node_id:
        raise RuntimeError('node configuration does not match the exact target')
    if config.get('PMMAN_SERVICE_NAME', service.removesuffix('.service')) not in (service, service.removesuffix('.service')):
        raise RuntimeError('node service does not match the exact target')
    version = Path(config['PMMAN_NODE_VERSION_FILE'])
    allowed = Path(f'/var/lib/pmman-network-node-{args.node_id}')
    if not version.is_absolute() or not version.resolve().is_relative_to(allowed):
        raise RuntimeError('node version path outside its dedicated state directory')
    show = subprocess.check_output(['systemctl', 'show', service, '-p', 'ExecStart', '--value'], text=True)
    if f'path={binary} ' not in show and f'path={binary} ;' not in show:
        raise RuntimeError('systemd executable does not match the node binary')
    backup = ROOT / 'release-backups' / f'node-{args.node_id}-{time.strftime("%Y%m%d-%H%M%S", time.gmtime())}-{args.version}'
    backup.mkdir(mode=0o700, parents=True, exist_ok=False)
    shutil.copy2(binary, backup / 'pmman-node')
    shutil.copy2(config_file, backup / 'node.env')
    os.chmod(backup / 'node.env', 0o600)
    previous_version = version.read_bytes() if version.exists() else None
    if previous_version is not None:
        (backup / 'version').write_bytes(previous_version)
    env_hash = digest(config_file)
    temporary = binary.with_name('pmman-node.release-new')
    try:
        shutil.copyfile(source, temporary)
        os.chmod(temporary, 0o755)
        if digest(temporary) != manifest['node']['sha256']:
            raise RuntimeError('staged node checksum mismatch')
        os.replace(temporary, binary)
        version.parent.mkdir(parents=True, exist_ok=True)
        version_temp = version.with_name(version.name + '.release-new')
        version_temp.write_text(args.version + '\n')
        os.replace(version_temp, version)
        subprocess.run(['systemctl', 'restart', service], check=True, timeout=20)
        time.sleep(2)
        subprocess.run(['systemctl', 'is-active', '--quiet', service], check=True, timeout=5)
        pid = subprocess.check_output(['systemctl', 'show', service, '-p', 'MainPID', '--value'], text=True).strip()
        if not pid.isdigit() or int(pid) <= 1 or digest(Path('/proc') / pid / 'exe') != manifest['node']['sha256']:
            raise RuntimeError('running node executable checksum mismatch')
        if digest(config_file) != env_hash:
            raise RuntimeError('node environment changed during promotion')
        print(json.dumps({'node_id': args.node_id, 'version': args.version,
                          'pid': int(pid), 'sha256': manifest['node']['sha256'],
                          'backup': str(backup), 'environment_unchanged': True}))
    except Exception:
        shutil.copy2(backup / 'pmman-node', temporary)
        os.replace(temporary, binary)
        if previous_version is None:
            version.unlink(missing_ok=True)
        else:
            version.write_bytes(previous_version)
        subprocess.run(['systemctl', 'restart', service], check=False, timeout=20)
        raise


if __name__ == '__main__':
    main()
