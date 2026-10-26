"""Real node-agent onboarding/restart smoke test. Creates and removes its own fixtures only."""
import json
import os
import pathlib
import subprocess
import time
import urllib.error
import urllib.request
import uuid

ROOT = pathlib.Path('/opt/pmman-network-dev')
BASE = 'http://127.0.0.1:8787'
COMPOSE = [
    'docker', 'compose', '--project-name', 'pmman-network',
    '--env-file', str(ROOT / '.env'), '-f', str(ROOT / 'docker-compose.yml'),
]
credentials = json.loads((ROOT / 'credentials.json').read_text())
fixture_id = 'pmman-check-' + uuid.uuid4().hex[:12]
pending_id = fixture_id + '-pending'
token = None
agent = None
fixture_interface = 'pmmchk-' + uuid.uuid4().hex[:8]
interface_created = False


def request(path, body=None, node_token=None, expected=200):
    headers = {}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    if node_token:
        headers['X-PMMAN-NODE-TOKEN'] = node_token
    data = None if body is None else json.dumps(body).encode()
    if data is not None:
        headers['Content-Type'] = 'application/json'
    req = urllib.request.Request(BASE + path, data=data, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=12) as response:
            status = response.status
            value = response.read()
    except urllib.error.HTTPError as error:
        status, value = error.code, b''
    if status != expected:
        raise RuntimeError(f'{path}: expected {expected}, got {status}')
    return json.loads(value) if value else None


def login():
    global token
    token = request('/v1/auth/login', dict(username=credentials['admin_user'],
                                         password=credentials['admin_password']))['token']


def wait_for(predicate, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, RuntimeError):
            pass
        time.sleep(1)
    raise RuntimeError('onboarding readiness timed out')


def compose(*arguments, check=True):
    return subprocess.run(COMPOSE + list(arguments), check=check,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          text=True)


def env_value(name):
    for line in (ROOT / '.env').read_text().splitlines():
        if line.startswith(name + '='):
            return line.split('=', 1)[1]
    raise RuntimeError(f'missing {name} in Compose environment')


login()
if any(device['status'] == 'active' for device in request('/v1/admin/devices')):
    raise RuntimeError('Close active project client connections before running this restart smoke test')

try:
    enrolled = request('/v1/admin/nodes', dict(node_id=fixture_id, name='Onboarding smoke', route_table=None))
    request('/v1/admin/nodes', dict(node_id=pending_id, name='Pending smoke'))
    assert not enrolled['node']['registered'] and not enrolled['node']['healthy']
    request(f'/v1/admin/nodes/{fixture_id}', dict(route_table=None, enabled=False))
    assert fixture_id not in [n['nodeId'] for n in request('/v1/client/nodes')]
    subprocess.run(['ip', 'link', 'add', fixture_interface, 'type', 'wireguard'], check=True)
    interface_created = True
    private_key = subprocess.check_output(['wg', 'genkey'], text=True)
    subprocess.run(['wg', 'set', fixture_interface, 'private-key', '/dev/stdin', 'listen-port', '51832'],
                   input=private_key, text=True, check=True)
    subprocess.run(['ip', 'link', 'set', fixture_interface, 'up'], check=True)
    public_key = subprocess.check_output(['wg', 'show', fixture_interface, 'public-key'], text=True).strip()
    registration = dict(node_id=fixture_id, name='Agent report', probe_endpoint='1.14.226.49:51842',
                        wireguard_endpoint='1.14.226.49:51832', server_public_key=public_key)
    request('/v1/nodes/register', registration, node_token='invalid-token', expected=401)
    request('/v1/nodes/register', dict(registration, node_id=fixture_id + '-wrong'),
            node_token=enrolled['node_token'], expected=401)
    environment = dict(os.environ, PMMAN_NODE_ID=fixture_id, PMMAN_NODE_NAME='Agent report',
                       PMMAN_PROBE_ENDPOINT=registration['probe_endpoint'], PMMAN_PROBE_LISTEN='0.0.0.0:51842',
                       PMMAN_NODE_TOKEN=enrolled['node_token'], PMMAN_MASTER_URL=BASE,
                       PMMAN_WG_INTERFACE=fixture_interface, PMMAN_WG_SERVER_PUBLIC_KEY=public_key,
                       PMMAN_WG_ENDPOINT=registration['wireguard_endpoint'])
    # The disabled fixture owns a disposable interface, so authoritative peer
    # reconciliation cannot alter either real node. Do not log credentials.
    agent = subprocess.Popen([str(ROOT / 'bin/pmman-node')], env=environment,
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    def registered():
        nodes = request('/v1/admin/nodes')
        return next((n for n in nodes if n['nodeId'] == fixture_id and n['registered']
                     and n['lastSeenAt'] != '1970-01-01T00:00:00Z'), None)
    first = wait_for(registered)
    assert not first['enabled'] and first['routeTable'] is None
    print('real_agent_registration_and_heartbeat=passed', flush=True)
    assert enrolled['node_token'] not in json.dumps(request('/v1/admin/nodes'))
    compose('restart', 'master')
    wait_for(lambda: urllib.request.urlopen(BASE + '/healthz', timeout=3).read() == b'ok')
    login()
    recovered = wait_for(registered)
    assert not recovered['enabled']
    assert next(n for n in request('/v1/admin/nodes') if n['nodeId'] == pending_id)['registered'] is False
    request(f'/v1/admin/nodes/{pending_id}/cancel', {}, expected=204)
    request('/v1/nodes/register', dict(registration, node_id=pending_id),
            node_token=enrolled['node_token'], expected=401)
    print('pending_and_registered_restart_recovery=passed', flush=True)
    print('credential_binding_and_cancel=passed', flush=True)
finally:
    if agent is not None:
        agent.terminate()
        agent.wait(timeout=15)
    if interface_created:
        subprocess.run(['ip', 'link', 'delete', fixture_interface], check=True)
    # Registered fixture IDs cannot be cancelled through the product API. Stop
    # only the project master, remove the generated keys from the PostgreSQL
    # nodes document, and start the same Compose service again. Never edit the
    # retired JSON files or touch an unrelated host service.
    compose('stop', 'master')
    try:
        user = env_value('PMMAN_POSTGRES_USER')
        database = env_value('PMMAN_POSTGRES_DB')
        fixture_sql = (
            "UPDATE pmman_documents "
            "SET payload=(payload::jsonb - :'fixture_id' - :'pending_id')::text, "
            "revision=revision+1, updated_at=CURRENT_TIMESTAMP "
            "WHERE document_key='nodes' AND (payload::jsonb ? :'fixture_id' "
            "OR payload::jsonb ? :'pending_id')"
        )
        subprocess.run(
            COMPOSE + ['exec', '-T', 'postgres', 'psql', '-v', 'ON_ERROR_STOP=1',
                       '-U', user, '-d', database,
                       '-v', f'fixture_id={fixture_id}',
                       '-v', f'pending_id={pending_id}', '-c', fixture_sql],
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
    finally:
        compose('start', 'master')
        wait_for(lambda: urllib.request.urlopen(BASE + '/healthz', timeout=3).read() == b'ok')
    print('onboarding_fixture_cleanup=passed', flush=True)
