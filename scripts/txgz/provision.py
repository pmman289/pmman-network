"""Project-scoped production deployment bootstrap; run as root on txgz.

This script provisions only the Pmman Network master.  It deliberately does
not create, seed, overwrite, or start any node.  Nodes are created later from
the admin panel and receive their own one-click deployment script.
"""

import json
import ipaddress
import os
import pathlib
import secrets
import subprocess


ROOT = pathlib.Path(os.environ.get('PMMAN_DEPLOY_ROOT', '/opt/pmman-network-dev'))
DOMAIN = os.environ.get('PMMAN_PUBLIC_DOMAIN', 'network.pmman.tech')
MASTER_LISTEN = os.environ.get('PMMAN_MASTER_LISTEN', '127.0.0.1:8787')
COMPOSE_FILE = ROOT / 'docker-compose.yml'
COMPOSE_ENV = ROOT / '.env'
DATA_ROOT = ROOT
RESOURCE_DIR = ROOT / 'node-resources'
CLIENT_RELEASE_DIR = ROOT / 'client-releases'
CLIENT_RELEASE_KEY = ROOT / 'client-release.key'
GEOSITE_DIR = ROOT / 'geosite'


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def write(path, value, mode=0o600):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value, encoding='utf-8')
    path.chmod(mode)


def prepare_container_directory(path):
    path = pathlib.Path(path)
    path.mkdir(parents=True, exist_ok=True)
    for item in [path, *path.rglob('*')]:
        try:
            os.chown(item, 1000, 1000)
        except PermissionError:
            pass
    path.chmod(0o700)


credential_path = ROOT / 'credentials.json'
if credential_path.exists():
    credentials = json.loads(credential_path.read_text())
else:
    credentials = {'admin_password': secrets.token_urlsafe(32), 'admin_user': 'admin'}

# Never retain demo-user passwords or pre-seeded node tokens in a production
# deployment credential file.
credentials = {
    'admin_user': credentials.get('admin_user', 'admin'),
    'admin_password': credentials['admin_password'],
}
write(credential_path, json.dumps(credentials, indent=2))

def existing_env_value(name):
    if not COMPOSE_ENV.exists():
        return None
    for line in COMPOSE_ENV.read_text(encoding='utf-8').splitlines():
        if line.startswith(name + '='):
            return line.split('=', 1)[1]
    return None


postgres_password = existing_env_value('PMMAN_POSTGRES_PASSWORD') or secrets.token_urlsafe(32)
master_image = os.environ.get('PMMAN_MASTER_IMAGE', existing_env_value('PMMAN_MASTER_IMAGE') or 'pmman-network/master:local')
trusted_proxy_ips = os.environ.get('PMMAN_TRUSTED_PROXY_IPS', existing_env_value('PMMAN_TRUSTED_PROXY_IPS') or '')
trusted_proxy_ips = ','.join(str(ipaddress.ip_address(value.strip()))
                             for value in trusted_proxy_ips.split(',') if value.strip())

RESOURCE_DIR.mkdir(parents=True, exist_ok=True)
RESOURCE_DIR.chmod(0o700)
CLIENT_RELEASE_DIR.mkdir(parents=True, exist_ok=True)
CLIENT_RELEASE_DIR.chmod(0o700)
GEOSITE_DIR.mkdir(parents=True, exist_ok=True)
GEOSITE_DIR.chmod(0o700)
DATA_ROOT.mkdir(parents=True, exist_ok=True)
for directory in (DATA_ROOT / 'node-resources', DATA_ROOT / 'client-releases', DATA_ROOT / 'geosite', DATA_ROOT / 'diagnostics', ROOT / 'backups'):
    prepare_container_directory(directory)
if CLIENT_RELEASE_KEY.exists():
    try:
        os.chown(CLIENT_RELEASE_KEY, 1000, 1000)
    except PermissionError:
        pass

client_release_secret_env = (
    '      PMMAN_CLIENT_RELEASE_PRIVATE_KEY_FILE: /run/secrets/client_release_key\n'
    if CLIENT_RELEASE_KEY.is_file() else ''
)
client_release_secret_mount = (
    f'      - {CLIENT_RELEASE_KEY}:/run/secrets/client_release_key:ro\n'
    if CLIENT_RELEASE_KEY.is_file() else ''
)

write(COMPOSE_ENV, f'''PMMAN_POSTGRES_DB=pmman
PMMAN_POSTGRES_USER=pmman
PMMAN_POSTGRES_PASSWORD={postgres_password}
PMMAN_ADMIN_PASSWORD={credentials['admin_password']}
PMMAN_MASTER_IMAGE={master_image}
PMMAN_DATA_ROOT={DATA_ROOT}
PMMAN_CLIENT_RELEASE_KEY_FILE={CLIENT_RELEASE_KEY}
PMMAN_TRUSTED_PROXY_IPS={trusted_proxy_ips}
''')
write(COMPOSE_FILE, f'''name: pmman-network
services:
  postgres:
    image: postgres:16.4-alpine
    restart: unless-stopped
    environment:
      POSTGRES_DB: ${{PMMAN_POSTGRES_DB}}
      POSTGRES_USER: ${{PMMAN_POSTGRES_USER}}
      POSTGRES_PASSWORD: ${{PMMAN_POSTGRES_PASSWORD}}
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -h 127.0.0.1 -p 5432 -U $${{POSTGRES_USER}} -d $${{POSTGRES_DB}}"]
      interval: 5s
      timeout: 5s
      retries: 12
    ports:
      - 127.0.0.1:55432:5432
    volumes:
      - pmman-postgres-data:/var/lib/postgresql/data
  master:
    image: ${{PMMAN_MASTER_IMAGE}}
    restart: unless-stopped
    depends_on:
      postgres:
        condition: service_healthy
    environment:
      PMMAN_DATABASE_URL: postgres://${{PMMAN_POSTGRES_USER}}:${{PMMAN_POSTGRES_PASSWORD}}@postgres:5432/${{PMMAN_POSTGRES_DB}}
      PMMAN_MASTER_LISTEN: 0.0.0.0:8787
      PMMAN_TRUSTED_PROXY_IPS: ${{PMMAN_TRUSTED_PROXY_IPS:-}}
      PMMAN_ADMIN_PASSWORD: ${{PMMAN_ADMIN_PASSWORD}}
      PMMAN_NODE_RESOURCE_DIR: /var/lib/pmman/node-resources
      PMMAN_CLIENT_RELEASE_DIR: /var/lib/pmman/client-releases
      PMMAN_GEOSITE_DIR: /var/lib/pmman/geosite
      PMMAN_DIAGNOSTIC_DIR: /var/lib/pmman/diagnostics
{client_release_secret_env.rstrip()}
    healthcheck:
      test: ["CMD-SHELL", "wget -q -O - http://127.0.0.1:8787/healthz | grep -qx ok"]
      interval: 10s
      timeout: 3s
      start_period: 10s
      retries: 6
    ports:
      - 127.0.0.1:8787:8787
    volumes:
      - ${{PMMAN_DATA_ROOT}}/node-resources:/var/lib/pmman/node-resources
      - ${{PMMAN_DATA_ROOT}}/client-releases:/var/lib/pmman/client-releases
      - ${{PMMAN_DATA_ROOT}}/geosite:/var/lib/pmman/geosite
      - ${{PMMAN_DATA_ROOT}}/diagnostics:/var/lib/pmman/diagnostics
{client_release_secret_mount.rstrip()}
volumes:
  pmman-postgres-data:
    name: pmman-network-postgres-data
''')

# A production master deployment must not create or overwrite node records.
# Stop only obsolete project-owned development units; unrelated host services
# and all administrator-created node registrations remain untouched.
for service in (
    'pmman-dev-master.service',
    'pmman-dev-node-a.service',
    'pmman-dev-node-b.service',
    'pmman-dev-probe.service',
    'pmman-dev-network.service',
):
    subprocess.run(
        ['systemctl', 'stop', service],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    subprocess.run(
        ['systemctl', 'disable', service],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

if os.environ.get('PMMAN_SKIP_MASTER') != '1':
    run('docker', 'compose', '--env-file', str(COMPOSE_ENV), '-f', str(COMPOSE_FILE), 'up', '-d')
write(
    ROOT / 'deployment.json',
    json.dumps(
        {
            'master': f'https://{DOMAIN}',
            'backend': f'http://{MASTER_LISTEN}',
            'nodes': [],
            'profile': 'production',
        },
        indent=2,
    ),
    0o644,
)
print('deployment_ready=true')
