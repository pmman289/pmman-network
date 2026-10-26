#!/usr/bin/env python3
"""Real dual-stack nftables/ip-rule regression in disposable WSL network namespaces.

Build the pmman-node musl unit-test executable first, then pass --test-binary.
No production network, credentials, interfaces or services are touched.
"""
import argparse
import json
import os
import secrets
import subprocess
import tempfile


SERVER = r'''
import socket,threading
def serve(family,udp):
 s=socket.socket(family,socket.SOCK_DGRAM if udp else socket.SOCK_STREAM)
 s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
 if family==socket.AF_INET6:s.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_V6ONLY,1)
 s.bind(('::' if family==socket.AF_INET6 else '0.0.0.0',3000))
 if udp:
  while True:
   data,peer=s.recvfrom(1024);s.sendto(data,peer)
 else:
  s.listen()
  def echo(c):
   with c:
    while True:
     data=c.recv(1024)
     if not data:return
     c.sendall(data)
  while True:
   c,_=s.accept();threading.Thread(target=echo,args=(c,),daemon=True).start()
for family in (socket.AF_INET,socket.AF_INET6):
 for udp in (False,True):threading.Thread(target=serve,args=(family,udp),daemon=True).start()
# Synchronize actual listeners, not a fixed sleep.
import time
for attempt in range(100):
 try:
  for address in ('162.141.136.136','2001:db8:2::2'):
   with socket.create_connection((address,3000),timeout=.2):pass
  print('READY',flush=True);break
 except OSError:time.sleep(.02)
else:raise RuntimeError('echo listeners not ready')
threading.Event().wait()
'''

PROBE = r'''
import socket,sys
address,protocol=sys.argv[1:]
family=socket.AF_INET6 if ':' in address else socket.AF_INET
with socket.socket(family,socket.SOCK_DGRAM if protocol=='udp' else socket.SOCK_STREAM) as s:
 s.settimeout(1.5);s.connect((address,50510))
 s.sendall(b'pmman-forward-routing')
 assert s.recv(128)==b'pmman-forward-routing'
print('PASS')
'''

PERSISTENT = r'''
import socket,sys
with socket.create_connection((sys.argv[1],50510),timeout=2) as s:
 print('READY',flush=True)
 for line in sys.stdin:
  s.sendall(line.encode());assert s.recv(128)==line.encode();print('PASS',flush=True)
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--test-binary', required=True)
    args = parser.parse_args()
    binary = os.path.realpath(args.test_binary)
    prefix = 'pmman-pfr-' + secrets.token_hex(4)
    namespaces = [prefix + suffix for suffix in ('-gw', '-pub', '-cli')]
    gateway, public, client = namespaces
    created, children, passed = [], [], []

    def run(*command, check=True, **kwargs):
        return subprocess.run(command, check=check, text=True, capture_output=True, timeout=45, **kwargs)

    def ns(namespace, *command, **kwargs):
        return run('ip', 'netns', 'exec', namespace, *command, **kwargs)

    def check(condition, label):
        if not condition:
            raise RuntimeError(label)
        passed.append(label)
        print('PASS ' + label, flush=True)

    def link(left, lname, right, rname):
        # Create directly inside the new namespace: no host-side veth management.
        ns(left, 'ip', 'link', 'add', lname, 'type', 'veth', 'peer', 'name', rname)
        ns(left, 'ip', 'link', 'set', rname, 'netns', right)
        ns(left, 'ip', 'link', 'set', lname, 'up')
        ns(right, 'ip', 'link', 'set', rname, 'up')

    def address(namespace, interface, value):
        ns(namespace, 'ip', 'address', 'add', value, 'dev', interface, 'nodad')

    def probe(address, protocol, success=True):
        result = ns(public, 'python3', '-c', PROBE, address, protocol, check=False)
        check((result.returncode == 0) == success, f'{address} {protocol} ' + ('forwarded' if success else 'blocked'))

    try:
        for namespace in namespaces:
            run('ip', 'netns', 'add', namespace)
            created.append(namespace)
            ns(namespace, 'ip', 'link', 'set', 'lo', 'up')
        link(gateway, 'wan0', public, 'pub0')
        link(gateway, 'wan1', public, 'pub1')
        link(gateway, 'client0', client, 'tun0')
        for index in (0, 1):
            address(gateway, f'wan{index}', f'198.18.{index}.2/24')
            address(public, f'pub{index}', f'198.18.{index}.1/24')
            subnet = 1 if index == 0 else 3
            address(gateway, f'wan{index}', f'2001:db8:{subnet}::2/64')
            address(public, f'pub{index}', f'2001:db8:{subnet}::1/64')
            for family, via in [('-4', f'198.18.{index}.1'), ('-6', f'2001:db8:{subnet}::1')]:
                ns(gateway, 'ip', family, 'route', 'add', 'default', 'via', via, 'dev', f'wan{index}', 'metric', str(100 + index))
        address(gateway, 'client0', '172.20.60.97/27')
        address(gateway, 'client0', '2001:db8:2::1/64')
        address(client, 'tun0', '162.141.136.136/32')
        address(client, 'tun0', '2001:db8:2::2/64')
        ns(gateway, 'ip', '-4', 'route', 'add', '162.141.136.136/32', 'dev', 'client0')
        ns(client, 'ip', '-4', 'route', 'add', 'default', 'via', '172.20.60.97', 'dev', 'tun0', 'onlink')
        ns(client, 'ip', '-6', 'route', 'add', 'default', 'via', '2001:db8:2::1', 'dev', 'tun0')
        ns(gateway, 'sysctl', '-qw', 'net.ipv4.ip_forward=1', 'net.ipv6.conf.all.forwarding=1')
        ns(gateway, 'ip', 'link', 'add', 'wrong0', 'type', 'dummy')
        ns(gateway, 'ip', 'link', 'set', 'wrong0', 'up')
        for family, source in [('-4', '162.141.136.136'), ('-6', '2001:db8:2::2')]:
            ns(gateway, 'ip', family, 'route', 'add', 'default', 'dev', 'wrong0', 'table', '236')
            ns(gateway, 'ip', family, 'rule', 'add', 'priority', '25362', 'from', source, 'lookup', '236')
        server = subprocess.Popen(['ip', 'netns', 'exec', client, 'python3', '-u', '-c', SERVER], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        children.append(server)
        check(server.stdout.readline().strip() == 'READY', 'dual-stack TCP/UDP listeners ready')

        # Reproduce the old DNAT+MASQUERADE implementation with the custom source rule.
        legacy = '''table ip pmman_network_port_forward {
 chain prerouting { type nat hook prerouting priority dstnat; policy accept;
 ip daddr 198.18.0.2 tcp dport 50510 ct mark set 14 dnat to 162.141.136.136:3000
 }
 chain postrouting { type nat hook postrouting priority srcnat; policy accept; ct mark 14 masquerade; }
}
'''
        ns(gateway, 'nft', '-f', '-', input=legacy)
        probe('198.18.0.2', 'tcp', success=False)
        # Existing firewall/QoS bits must survive both conntrack and packet marking.
        ns(gateway, 'nft', '-f', '-', input='''table inet pmman_fixture_marks {
 chain before { type filter hook prerouting priority -160; policy accept;
 meta mark set 0x1234
 ct direction original ct state new ct mark set 0x5678
 }
 counter preserved {}
 chain after { type filter hook prerouting priority -140; policy accept;
 ct direction reply ct status dnat ct mark & 0xffff == 0x5678 meta mark & 0xffff == 0x1234 counter name preserved
 }
}
''')

        with tempfile.TemporaryDirectory(prefix=prefix + '-') as temp:
            state = os.path.join(temp, 'version')
            def reconcile(stage='install'):
                env = os.environ.copy()
                env.update(PMMAN_PF_KERNEL_FIXTURE='1', PMMAN_PF_KERNEL_STATE=state, PMMAN_PF_KERNEL_STAGE=stage)
                result = ns(gateway, binary, 'tests::real_kernel_port_forward_return_routing', '--exact', '--ignored', '--nocapture', env=env, check=False)
                if result.returncode:
                    raise RuntimeError(result.stdout + result.stderr)
            reconcile('multi')
            journal = open(state + '.port-forward-routing.json').read()
            for endpoint in ('198.18.0.2', '2001:db8:1::2', '198.18.1.2', '2001:db8:3::2'):
                for protocol in ('tcp', 'udp'):
                    probe(endpoint, protocol)
            counters = json.loads(ns(gateway, 'nft', '-j', 'list', 'counter', 'inet', 'pmman_fixture_marks', 'preserved').stdout)
            check(any(item.get('counter', {}).get('packets', 0) > 0 for item in counters['nftables']), 'real packets preserve foreign conntrack and fwmark bits')
            reconcile('multi')
            check(open(state + '.port-forward-routing.json').read() == journal, 'restart/reconcile retains stable ownership and marks')
            rules = json.loads(ns(gateway, 'ip', '-j', '-4', 'rule', 'show').stdout)
            check(sum(rule.get('priority') == 9000 for rule in rules) == 2, 'no duplicate routing rules after repeat reconciliation')
            check(any(rule.get('priority') == 25362 and str(rule.get('table')) == '236' for rule in rules), 'custom source policy preserved')
            route = ns(gateway, 'ip', '-4', 'route', 'get', '198.18.0.1', 'from', '162.141.136.136', 'iif', 'client0', 'mark', '0x504d1234').stdout
            check('dev wan0' in route and 'table 52100' in route, 'fwmark mask tolerates foreign low bits')
            route = ns(gateway, 'ip', '-4', 'route', 'get', '198.18.0.1', 'from', '162.141.136.136', 'iif', 'client0').stdout
            check('dev wrong0' in route and 'table 236' in route, 'unmarked proxy traffic still follows source policy')
            established = []
            for endpoint in ('198.18.0.2', '2001:db8:1::2'):
                process = subprocess.Popen(['ip', 'netns', 'exec', public, 'python3', '-u', '-c', PERSISTENT, endpoint], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                children.append(process)
                check(process.stdout.readline().strip() == 'READY', 'established connection ' + endpoint)
                established.append(process)
            reconcile('release')
            for process in established:
                process.stdin.write('retained\n'); process.stdin.flush()
                check(process.stdout.readline().strip() == 'PASS', 'existing connection retains return path after stopping new mappings')
            probe('198.18.0.2', 'tcp', success=False)
            probe('2001:db8:1::2', 'udp', success=False)
            ns(gateway, 'ip', 'link', 'set', 'wan0', 'down')
            route = ns(gateway, 'ip', '-4', 'route', 'get', '198.18.0.1', 'from', '162.141.136.136', 'iif', 'client0', 'mark', '0x504d0000', check=False)
            check(route.returncode != 0, 'egress down fails closed instead of falling through source policy')
        print(f'PASS {len(passed)} real-kernel checks', flush=True)
    finally:
        for child in children:
            if child.poll() is None:
                child.terminate()
            try: child.wait(timeout=3)
            except subprocess.TimeoutExpired: child.kill(); child.wait()
        for namespace in reversed(created):
            run('ip', 'netns', 'del', namespace, check=False)


if __name__ == '__main__':
    main()
