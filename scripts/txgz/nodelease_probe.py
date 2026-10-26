"""Bounded pause of only the project Compose master; called by
verify-node-auth-lease.ps1.

Input credentials travel on SSH stdin, never argv, disk, or output. No wg dump.
"""
import ipaddress
import json
import os
import re
import select
import signal
import subprocess
import sys
import time
import urllib.request

MASTER = "https://network.pmman.tech"
ROOT = "/opt/pmman-network-dev"
COMPOSE = [
    "docker", "compose", "--project-name", "pmman-network",
    "--env-file", ROOT + "/.env", "-f", ROOT + "/docker-compose.yml",
]
PAUSE_SECONDS = 40


def command(args, deadline=None):
    remaining = 2 if deadline is None else min(2, deadline - time.monotonic())
    if remaining <= 0:
        raise TimeoutError("observation deadline exceeded")
    return subprocess.check_output(args, text=True, timeout=remaining).strip()


def request(path, token, method="GET"):
    req = urllib.request.Request(
        MASTER + path, data=b"" if method == "POST" else None,
        headers={"Authorization": "Bearer " + token}, method=method,
    )
    with urllib.request.urlopen(req, timeout=5) as response:
        body = response.read()
        return json.loads(body) if body else None


def validate(spec):
    if spec["master"] != MASTER or spec["node_id"] not in ("txgz-a", "txgz-b"):
        raise ValueError("unexpected public master/node")
    nodes = spec["nodes"]
    if len(nodes) != 2 or {n["id"] for n in nodes} != {"txgz-a", "txgz-b"}:
        raise ValueError("expected exactly the two development nodes")
    for node in nodes:
        suffix = node["id"].removeprefix("txgz-")
        if node["interface"] != "pmman-dev-" + suffix or not re.fullmatch(r"[0-9]+", str(node["route_table"])):
            raise ValueError("unexpected dedicated interface/route table")
    if not re.fullmatch(r"[0-9a-f-]{36}", spec["device_id"]):
        raise ValueError("invalid fixture device id")
    ipaddress.IPv4Interface(spec["ipv4"])
    ipaddress.IPv6Interface(spec["ipv6"])


def snapshot(nodes, deadline=None):
    output = []
    for node in nodes:
        interface = node["interface"]
        links = json.loads(command(["ip", "-j", "link", "show", "dev", interface], deadline))
        output.append({
            "id": node["id"], "interface": interface,
            "up": "UP" in links[0]["flags"],
            "peers": command(["wg", "show", interface, "peers"], deadline).splitlines(),
            "routes4": json.loads(command(["ip", "-j", "-4", "route", "show", "table", str(node["route_table"])], deadline)),
            "routes6": json.loads(command(["ip", "-j", "-6", "route", "show", "table", str(node["route_table"])], deadline)),
        })
    return output


def owns_route(route, address, interface):
    destination = route.get("dst", "default")
    if destination == "default" or route.get("dev") != interface:
        return False
    network = ipaddress.ip_network(destination, strict=False)
    target = ipaddress.ip_interface(address).ip
    return network.prefixlen == network.max_prefixlen and network.network_address == target


def fixture_present(observation, spec):
    node = next(n for n in observation if n["id"] == spec["node_id"])
    return (spec["public_key"] in node["peers"]
            and any(owns_route(r, spec["ipv4"], node["interface"]) for r in node["routes4"])
            and any(owns_route(r, spec["ipv6"], node["interface"]) for r in node["routes6"]))


def failclosed(observation, spec):
    return (all(not n["up"] for n in observation)
            and all(spec["public_key"] not in n["peers"] for n in observation)
            and all(not any(owns_route(r, spec[family], n["interface"]) for r in n[routes])
                    for n in observation for family, routes in (("ipv4", "routes4"), ("ipv6", "routes6"))))


class MasterProcess:
    def __init__(self):
        if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
            raise RuntimeError("pidfd support required before any pause")
        if "master" not in command(COMPOSE + ["ps", "--services", "--status", "running"]).splitlines():
            raise RuntimeError("Compose master is not active")
        container = command(COMPOSE + ["ps", "-q", "master"])
        if not container:
            raise RuntimeError("Compose master container is missing")
        self.pid = int(command(["docker", "inspect", "--format", "{{.State.Pid}}", container]))
        if self.pid <= 1:
            raise RuntimeError("invalid development master PID")
        self.fd = os.pidfd_open(self.pid)
        if int(command(["docker", "inspect", "--format", "{{.State.Pid}}", container])) != self.pid:
            os.close(self.fd)
            raise RuntimeError("Compose master changed during preflight")

    def stop(self):
        signal.pidfd_send_signal(self.fd, signal.SIGSTOP)

    def resume(self):
        try:
            signal.pidfd_send_signal(self.fd, signal.SIGCONT)
        except ProcessLookupError:
            pass  # Never signal a replacement PID.

    def close(self):
        os.close(self.fd)


class ResumeGuard:
    """Detached safety guard: parent death/SSH EOF or 40s timeout resumes PID."""
    def __init__(self, pidfd):
        read_fd, self.write_fd = os.pipe()
        self.pid = os.fork()
        if self.pid == 0:
            os.close(self.write_fd)
            os.setsid()
            for sig in (signal.SIGHUP, signal.SIGTERM, signal.SIGINT):
                signal.signal(sig, signal.SIG_IGN)
            null = os.open(os.devnull, os.O_RDWR)
            for stream in (0, 1, 2):
                os.dup2(null, stream)
            try:
                ready, _, _ = select.select([read_fd], [], [], PAUSE_SECONDS)
                done = os.read(read_fd, 1) if ready else b""
                if done != b"D":
                    signal.pidfd_send_signal(pidfd, signal.SIGCONT)
            except ProcessLookupError:
                pass
            finally:
                os._exit(0)
        os.close(read_fd)

    def cancel(self):
        try:
            os.write(self.write_fd, b"D")
        except BrokenPipeError:
            pass
        finally:
            os.close(self.write_fd)
            os.waitpid(self.pid, os.WNOHANG)


def pause_and_observe(spec, process, guard):
    observations = []
    began = time.monotonic()
    deadline = began + PAUSE_SECONDS
    passed = False
    try:
        process.stop()
        time.sleep(31)  # 30s authorization lease, then bounded kernel cleanup.
        while time.monotonic() < deadline:
            current = snapshot(spec["nodes"], deadline)
            observations.append({"elapsed": time.monotonic() - began, "nodes": current})
            if failclosed(current, spec):
                passed = True
                break
            time.sleep(min(0.5, max(0, deadline - time.monotonic())))
        if not passed:
            raise RuntimeError("node authorization did not fail closed within pause window")
        time.sleep(max(0, deadline - time.monotonic()))
    finally:
        # Even assertion failures, handled termination, and command timeouts
        # resume the original PID. Keep the master process and its in-memory
        # demo sessions intact: restarting it here would invalidate the
        # fixture's client token and turn node recovery into an auth test.
        try:
            process.resume()
        except Exception:
            os.close(guard.write_fd)
            raise
        else:
            guard.cancel()
        finally:
            process.close()
    return observations


def run(spec):
    validate(spec)
    if spec.get("mode") == "observe":
        return {"passed": True, "nodes": snapshot(spec["nodes"])}
    if spec.get("mode", "pause") != "pause":
        raise ValueError("unexpected probe mode")
    devices = request("/v1/admin/devices", spec["admin_token"])
    active = [d for d in devices if d["status"] == "active"]
    if len(active) != 1 or active[0]["device_id"] != spec["device_id"] or active[0]["public_key"] != spec["public_key"]:
        raise RuntimeError("other active leases present or fixture not active; no pause performed")
    before = snapshot(spec["nodes"])
    if not all(n["up"] for n in before) or not fixture_present(before, spec):
        raise RuntimeError("fixture peer/routes not installed on UP nodes; no pause performed")
    if any(key != spec["public_key"] for n in before for key in n["peers"]):
        raise RuntimeError("other kernel peers present; no pause performed")
    # Start the 60s client lease immediately before the 40s master pause.
    request("/v1/devices/" + spec["device_id"] + "/heartbeat", spec["client_token"], "POST")
    active = [d for d in request("/v1/admin/devices", spec["admin_token"]) if d["status"] == "active"]
    if len(active) != 1 or active[0]["device_id"] != spec["device_id"]:
        raise RuntimeError("active lease set changed; no pause performed")
    process = MasterProcess()
    try:
        guard = ResumeGuard(process.fd)
    except Exception:
        process.close()
        raise
    observations = pause_and_observe(spec, process, guard)
    # Renew without re-registering: a revoked/expired client must fail here,
    # never get silently replaced by a new connection to make recovery pass.
    request("/v1/devices/" + spec["device_id"] + "/heartbeat", spec["client_token"], "POST")
    return {"passed": True, "pauseSeconds": PAUSE_SECONDS, "before": before,
            "observations": observations, "masterResumed": True, "clientLeaseRenewed": True}


if __name__ == "__main__":
    def interrupted(signum, frame):
        raise RuntimeError("remote lease probe interrupted")
    for sig in (signal.SIGHUP, signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, interrupted)
    try:
        result = run(json.load(sys.stdin))
    except Exception as exc:
        result = {"passed": False, "error": str(exc)}
    print(json.dumps(result))
    sys.exit(0 if result["passed"] else 1)
