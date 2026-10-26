#!/usr/bin/env python3
"""Isolated WSL Docker acceptance; no production credentials or networking.

Run on the local WSL host after building the musl master and Docker image:
    python3 scripts/verify-compose.py --image pmman-network/master:compose-test
Only this run's project, network, volume and temporary mounts are removed.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="pmman-network/master:compose-test")
    parser.add_argument("--browser-state", type=Path, help="ignored artifacts directory; pause for browser checks")
    parser.add_argument("--browser-origin", default="http://127.0.0.1:18087")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    project = "pmman-compose-test-" + secrets.token_hex(5)
    env = os.environ.copy()
    # Ignore any production Compose defaults or .env loaded by the developer.
    admin_password = secrets.token_hex(24)
    changed_password = secrets.token_hex(24)
    user_password = secrets.token_hex(24)
    port = free_port()
    db_port = free_port()
    while db_port == port:
        db_port = free_port()
    env.update({
        "PMMAN_POSTGRES_DB": "pmman", "PMMAN_POSTGRES_USER": "pmman",
        "PMMAN_POSTGRES_PASSWORD": secrets.token_hex(24),
        "PMMAN_ADMIN_PASSWORD": admin_password,
        "PMMAN_MASTER_IMAGE": args.image,
        "PMMAN_MASTER_PORT": str(port), "PMMAN_POSTGRES_PORT": str(db_port),
        "PMMAN_COMPOSE_NETWORK_NAME": project + "-control",
        "PMMAN_POSTGRES_VOLUME_NAME": project + "-postgres-data",
        "PMMAN_CLIENT_RELEASE_KEY_FILE": "/dev/null",
        "PMMAN_TRUSTED_PROXY_IPS": "",
    })
    base_url = "http://127.0.0.1:" + str(port)
    passed = []
    token = ""

    def check(condition, label):
        if not condition:
            raise RuntimeError(label)
        passed.append(label)
        print("PASS " + label, flush=True)

    with tempfile.TemporaryDirectory(prefix=project + "-") as temporary:
        env["PMMAN_DATA_ROOT"] = temporary
        # Non-root master UID 1000 must be able to publish resource files.
        for directory in ("node-resources", "client-releases", "geosite", "diagnostics"):
            path = Path(temporary) / directory
            path.mkdir()
            if os.geteuid() == 0:
                os.chown(path, 1000, 1000)
        os.chmod(temporary, 0o755)
        compose = ["docker", "compose", "--env-file", "/dev/null", "-p", project,
                   "-f", str(repo / "docker-compose.yml")]

        def command(argv, *, required=True):
            result = subprocess.run(argv, cwd=repo, env=env, capture_output=True,
                                    text=True, timeout=120)
            if required and result.returncode:
                # Do not print subprocess output containing generated secrets.
                raise RuntimeError("command failed: " + " ".join(argv[:6]))
            return result

        def dc(*argv, required=True):
            return command(compose + list(argv), required=required)

        def api(path, data=None, *, auth=True, expected=200, session=None, method=None):
            headers = {}
            if auth and (session or token):
                headers["Authorization"] = "Bearer " + (session or token)
            if data is not None:
                headers["Content-Type"] = "application/json"
            request = urllib.request.Request(base_url + path, headers=headers,
                data=json.dumps(data).encode() if data is not None else None, method=method)
            try:
                with urllib.request.urlopen(request, timeout=15) as response:
                    status, body = response.status, response.read()
            except urllib.error.HTTPError as error:
                status, body = error.code, error.read()
            if status != expected:
                raise RuntimeError(f"{path}: expected HTTP {expected}, got {status}")
            if body and body[:1] in (b"[", b"{"):
                return json.loads(body)
            return body.decode()

        def healthy():
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                try:
                    ids = dc("ps", "-q", "master", "postgres").stdout.split()
                    states = [command(["docker", "inspect", "--format",
                        "{{.State.Health.Status}}", cid]).stdout.strip() for cid in ids]
                    if len(states) == 2 and all(s == "healthy" for s in states):
                        return
                except RuntimeError:
                    pass
                time.sleep(1)
            raise RuntimeError("Compose containers did not become healthy")

        def login(password):
            return api("/v1/auth/login", {"username": "admin", "password": password},
                       auth=False)["token"]

        def sql(query):
            return dc("exec", "-T", "postgres", "psql", "-U", "pmman", "-d", "pmman",
                      "-At", "-v", "ON_ERROR_STOP=1", "-c", query).stdout.strip()

        def snapshot():
            values = sql("SELECT document_key || ':' || md5(payload) FROM pmman_documents "
                         "ORDER BY document_key")
            for table in ("invitations", "invitation_policies", "invitation_registrations", "user_forward_policies",
                          "address_pools", "address_pool_segments", "address_pool_exclusions",
                          "address_pool_defaults", "user_address_policies", "address_leases", "address_forward_cleanup"):
                values += sql(f"SELECT md5(COALESCE(string_agg(row_to_json(t)::text, ',' ORDER BY row_to_json(t)::text),'')) FROM {table} t")
            return hashlib.sha256(values.encode()).hexdigest()

        def verify_persisted():
            users = api("/v1/admin/users")
            check(any(u["username"] == "compose_acceptance" for u in users),
                  "test user restored")
            check(api("/v1/admin/settings/address-pool")["ipv4_pool"] == "10.219.77.0/27",
                  "custom pool restored")
            check(api("/v1/admin/settings/address-pool")["internal_cidrs"] ==
                  ["10.219.0.0/16", "fd77:219::/48"], "internal routes restored")
            check(len(api("/v1/admin/nodes")) == 1, "enrolled node restored")
            check(len(api("/v1/admin/port-forward/entries")) == 1,
                  "PostgreSQL forward entry restored")
            check(api("/v1/admin/users/compose_acceptance/port-forward-policy")
                  ["max_active_forwards"] == 2, "PostgreSQL forward policy restored")
            check(api("/v1/admin/invitation-registrations")["total"] >= 2,
                  "invitation registrations restored")
            check(api("/v1/admin/users/invitation_child/invitation-policy")["policy"]["enabled"] is False,
                  "invited account never inherits invitation privilege")

        try:
            dc("up", "-d", "--wait", "--wait-timeout", "90")
            healthy()
            check(api("/healthz", auth=False) == "ok", "empty Compose boot and /healthz")
            check(api("/admin", auth=False).startswith("<!doctype html>"), "embedded admin HTML")
            page = api("/admin", auth=False)
            assets = re.findall(r'(?:src|href)="(/admin/assets/[^\"]+)"', page)
            check(bool(assets), "admin asset references")
            for asset in assets:
                check(bool(api(asset, auth=False)), "embedded admin asset")
            uid = dc("exec", "-T", "master", "id", "-u").stdout.strip()
            check(uid == "1000", "master runs as non-root")
            dc("exec", "-T", "master", "sh", "-c",
               "test -w /var/lib/pmman/client-releases && test -w /var/lib/pmman/geosite && "
               "test -w /var/lib/pmman/node-resources")
            check(True, "resource mounts writable by master")
            token = login(admin_password)
            api("/v1/admin/nodes", expected=403)
            check(True, "bootstrap administrator must change password")
            api("/v1/auth/password", {"current_password": admin_password,
                                     "new_password": changed_password}, expected=204)
            check(api("/v1/admin/nodes") == [], "no demo nodes on first boot")
            check(len(api("/v1/admin/users")) == 1, "only bootstrap administrator")
            api("/v1/admin/users", {"username": "compose_acceptance", "password": user_password})
            pools = api("/v1/admin/address-pools")
            primary = pools["ipv4_pool_id"]
            api(f"/v1/admin/address-pools/{primary}", {"revision": pools["revision"],
                "name": "Compose primary", "segments": ["10.219.77.0/27", "fd77:219:77::/64"],
                "exclusions": []})
            api("/v1/admin/settings/internal-routes", {"internal_cidrs":
                ["10.219.0.0/16", "fd77:219::/48"]})
            api("/v1/admin/settings/address-pool", {"ipv4_pool": "10.0.0.0/24",
                "ipv6_pool": "fd00::/64"}, expected=409)
            api("/v1/admin/nodes", {"node_id": "compose-test-node", "name": "Compose acceptance",
                                    "route_table": "52001"})
            api("/v1/admin/port-forward/entries", {"entry_id": "compose-test-entry",
                "node_id": "compose-test-node", "name": "Compose acceptance",
                "public_address": "203.0.113.17", "family": "ipv4", "protocols": ["tcp", "udp"],
                "port_ranges": [{"start": 45000, "end": 45009}], "reserved_ports": [45001],
                "enabled": True})
            api("/v1/admin/users/compose_acceptance/port-forward-policy", {"enabled": True,
                "max_active_forwards": 2, "allowed_entry_ids": ["compose-test-entry"],
                "allowed_protocols": ["tcp", "udp"], "allowed_families": ["ipv4"]})
            check(sql("SELECT string_agg(version::text, ',' ORDER BY version) FROM schema_migrations")
                  == "1,2,3,4,5", "versioned database migrations")
            check(sql("SELECT count(*) FROM forward_entries") == "1", "entry persisted in PostgreSQL")
            register_page = api("/register", auth=False)
            check(register_page.startswith("<!doctype html>"), "public registration entry")
            for asset in re.findall(r'(?:src|href)="(/admin/assets/[^\"]+)"', register_page):
                check(bool(api(asset, auth=False)), "embedded public registration asset")
            grant = {"allow_ipv4_global": False, "allow_ipv6_global": False,
                "allow_ipv4_split": True, "allow_ipv6_split": False,
                "port_forward": {"enabled": True, "max_active_forwards": 2,
                    "allowed_entry_ids": ["compose-test-entry"], "allowed_protocols": ["tcp", "udp"],
                    "allowed_target_ports": [22, 443], "allowed_families": ["ipv4"]}}
            invitation = api("/v1/admin/invitations", {"kind": "unlimited", "note": "Compose acceptance",
                "expires_at": None, "grant": grant})
            api("/v1/auth/register", {"username": "invitation_owner", "password": user_password,
                "code": invitation["code"]}, auth=False, expected=201)
            owner = api("/v1/auth/login", {"username": "invitation_owner", "password": user_password}, auth=False)
            check(owner["must_change_password"] is False and owner["role"] == "user",
                  "invited user can log in without forced reset and is never admin")
            api("/v1/admin/invitations", session=owner["token"], expected=403)
            api("/v1/client/invitations", {}, session=owner["token"], expected=403)
            policy = {"enabled": True, "max_unused": 2, "daily_limit": 3, "valid_days": 7, "grant": grant}
            api("/v1/admin/users/invitation_owner/invitation-policy", policy)
            single = api("/v1/client/invitations", {}, session=owner["token"])
            policy["grant"]["allow_ipv6_split"] = True
            api("/v1/admin/users/invitation_owner/invitation-policy", policy)
            api("/v1/auth/register", {"username": "invitation_child", "password": user_password,
                "code": single["code"]}, auth=False, expected=201)
            api("/v1/auth/register", {"username": "invitation_child_duplicate", "password": user_password,
                "code": single["code"]}, auth=False, expected=400)
            child = api("/v1/auth/login", {"username": "invitation_child", "password": user_password}, auth=False)
            check(child["allow_ipv6_split"] is True, "registration uses current inviter grant")
            check(api("/v1/admin/users/invitation_child/port-forward-policy")["allowed_target_ports"] == [22, 443],
                  "registration atomically persists complete forwarding grant")
            records = api("/v1/client/invitation-registrations", session=owner["token"])
            check(records["total"] == 1 and records["items"][0]["username"] == "invitation_child",
                  "inviter records are ownership-filtered")
            api("/v1/admin/invitations/" + invitation["invitation"]["id"] + "/revoke", {}, expected=204)
            api("/v1/auth/register", {"username": "invitation_revoked", "password": user_password,
                "code": invitation["code"]}, auth=False, expected=400)
            check("code_hash" not in json.dumps(api("/v1/admin/invitations")),
                  "invitation list never exposes code digests or full codes")
            pending = api("/v1/client/invitations", {}, session=owner["token"])
            if args.browser_state:
                location = args.browser_state.resolve()
                if not location.is_relative_to(repo / "artifacts"):
                    raise RuntimeError("browser state must remain under ignored artifacts/")
                location.mkdir(parents=True, exist_ok=True)
                session = api("/v1/auth/login", {"username": "admin", "password": changed_password}, auth=False)
                # No password is persisted. Token expires with this isolated lab.
                script = ("if(location.origin === " + json.dumps(args.browser_origin) + ") {"
                    "sessionStorage.setItem('pmman-admin-session-v1', " + json.dumps(json.dumps(session)) + ");}")
                (location / "auth-init.js").write_text(script)
                print(f"BROWSER_UPSTREAM_PORT={port}", flush=True)
                print("BROWSER_READY=true", flush=True)
                deadline = time.monotonic() + 900
                while not (location / "done").exists():
                    if time.monotonic() > deadline:
                        raise RuntimeError("browser acceptance timed out")
                    time.sleep(1)
                (location / "auth-init.js").unlink()
                check(True, "browser acceptance completed")
            before = snapshot()
            dc("restart", "master")
            healthy()
            api("/v1/admin/users", expected=401)
            check(True, "old sessions invalid after master restart")
            token = login(changed_password)
            api("/v1/auth/login", {"username": "admin", "password": admin_password},
                auth=False, expected=401)
            check(True, "changed password not reset by bootstrap environment")
            verify_persisted()
            check(snapshot() == before, "master restart preserves durable document content")
            dc("stop", "postgres")
            api("/healthz", auth=False, expected=503)
            check(True, "database outage produces HTTP 503")
            api("/v1/auth/register", {"username": "registration_outage", "password": user_password,
                "code": pending["code"]}, auth=False, expected=503)
            check(True, "registration fails closed while PostgreSQL is unavailable")
            api("/v1/admin/users", {"username": "must_not_persist", "password": user_password},
                expected=500)
            check(not any(u["username"] == "must_not_persist" for u in api("/v1/admin/users")),
                  "failed database write does not mutate live user registry")
            dc("restart", "master")
            time.sleep(2)
            result = dc("ps", "-a", "-q", "master").stdout.strip()
            state = command(["docker", "inspect", "--format", "{{.State.Status}}", result]).stdout.strip()
            health = command(["docker", "inspect", "--format", "{{if .State.Health}}{{.State.Health.Status}}{{end}}", result]).stdout.strip()
            check(state in ("restarting", "exited") or health in ("starting", "unhealthy"),
                  "master exposes database outage without fallback")
            dc("start", "postgres")
            healthy()
            token = login(changed_password)
            verify_persisted()
            check(snapshot() == before, "database restart preserves durable documents")
            # Also verify recovery without restarting the master process.
            dc("restart", "postgres")
            healthy()
            check(api("/healthz", auth=False) == "ok", "master reconnects after database restart")
            verify_persisted()
            dc("up", "-d", "--force-recreate", "master")
            healthy()
            token = login(changed_password)
            verify_persisted()
            check(snapshot() == before, "container replacement preserves PostgreSQL data")
            # Exercise actual master HTTP + PostgreSQL after all restarts.
            # The legacy fixture has no WireGuard interface and is not a
            # substitute for real client/node traffic acceptance.
            client = api("/v1/auth/login", {"username": "compose_acceptance",
                "password": user_password}, auth=False)["token"]
            assigned = api("/v1/devices/register", {"public_key": "compose-route-fixture"}, session=client)
            expected_routes = {"10.219.0.0/16", "fd77:219::/48",
                               "10.219.77.0/27", "fd77:219:77::/64"}
            check(set(assigned["routes"]) == expected_routes,
                  "restarted master merges all pool routes with manual routes")
            check(len(assigned["routes"]) == len(set(assigned["routes"])),
                  "effective internal routes contain no exact duplicates")
            pools = api("/v1/admin/address-pools")
            extra = api("/v1/admin/address-pools", {"revision": pools["revision"],
                "name": "Automatic route fixture", "segments": ["192.0.2.255/32", "2001:db8:9::/128"]})
            extra_id = next(p["pool_id"] for p in extra["pools"] if p["name"] == "Automatic route fixture")
            config_path = f"/v1/devices/{assigned['device_id']}/config"
            check(set(api(config_path, session=client)["routes"]) ==
                  expected_routes | {"192.0.2.255/32", "2001:db8:9::/128"},
                  "configuration refresh adds non-primary host-prefix pool routes")
            extra = api(f"/v1/admin/address-pools/{extra_id}", {"revision": extra["revision"],
                "name": "Automatic route fixture", "segments": ["198.51.100.0/31"]})
            check(set(api(config_path, session=client)["routes"]) == expected_routes | {"198.51.100.0/31"},
                  "pool edit replaces derived routes without stale prefixes")
            api(f"/v1/admin/address-pools/{extra_id}?revision={extra['revision']}", expected=204, method="DELETE")
            check(set(api(config_path, session=client)["routes"]) == expected_routes,
                  "pool deletion removes only its derived routes")
            check(api("/v1/admin/settings/address-pool")["internal_cidrs"] ==
                  ["10.219.0.0/16", "fd77:219::/48"], "automatic routes never rewrite manual settings")
            api(f"/v1/devices/{assigned['device_id']}/revoke", {"public_key": "compose-route-fixture"},
                session=client, expected=204)
            logs = dc("logs", "--no-color", "master").stdout
            check(all(secret not in logs for secret in (admin_password, changed_password, user_password,
                invitation["code"], single["code"], pending["code"])), "master logs contain no invitation codes or passwords")
            check("Cannot start a runtime" not in logs, "no nested-runtime panic in final container")
            print(f"RESULT {len(passed)} checks passed", flush=True)
        finally:
            dc("down", "-v", "--remove-orphans")
            check(not command(["docker", "ps", "-a", "-q", "--filter",
                "label=com.docker.compose.project=" + project]).stdout.strip(), "test containers removed")
            check(not command(["docker", "volume", "ls", "-q", "--filter",
                "label=com.docker.compose.project=" + project]).stdout.strip(), "test volumes removed")


if __name__ == "__main__":
    main()
