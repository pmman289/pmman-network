#!/usr/bin/env python3
"""Build the client-owned IPv4/IPv6 overseas route snapshot.

The default invocation validates the checked-in snapshot. `--refresh` is the
only mode that performs network I/O and replaces it after all checks pass.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import ipaddress
import json
import os
import shutil
import subprocess
import tempfile
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

GENERATOR_VERSION = "client_builtin_v1"
ROOT = Path(__file__).resolve().parents[2]
RESOURCE_DIR = ROOT / "service" / "resources" / "cnipgen"
SNAPSHOT_DIR = Path(__file__).resolve().parent / "snapshots"

URLS = {
    "produce.py": "https://raw.githubusercontent.com/dndx/nchnroutes/master/produce.py",
    "ipv4-address-space.csv": "https://raw.githubusercontent.com/dndx/nchnroutes/master/ipv4-address-space.csv",
    "china_ip_list.txt": "https://raw.githubusercontent.com/17mon/china_ip_list/master/china_ip_list.txt",
    "delegated-apnic-latest": "https://ftp.apnic.net/stats/apnic/delegated-apnic-latest",
}

SPECIAL_V4 = tuple(
    ipaddress.ip_network(value)
    for value in (
        "0.0.0.0/8",
        "10.0.0.0/8",
        "100.64.0.0/10",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "172.16.0.0/12",
        "192.0.0.0/24",
        "192.0.2.0/24",
        "192.168.0.0/16",
        "198.18.0.0/15",
        "198.51.100.0/24",
        "203.0.113.0/24",
        "224.0.0.0/4",
        "240.0.0.0/4",
    )
)
SPECIAL_V6 = tuple(
    ipaddress.ip_network(value)
    for value in (
        "::/128",
        "::1/128",
        "::ffff:0:0/96",
        "100::/64",
        "2001:db8::/32",
        "2001::/32",
        "2002::/16",
        "fc00::/7",
        "fe80::/10",
        "ff00::/8",
    )
)


def download(url: str, destination: Path) -> None:
    request = urllib.request.Request(url, headers={"User-Agent": "pmman-cnipgen/1"})
    with urllib.request.urlopen(request, timeout=180) as response:
        data = response.read()
    if not data:
        raise RuntimeError(f"empty download: {url}")
    destination.write_bytes(data)


def subtract(network: ipaddress._BaseNetwork, excluded: list[ipaddress._BaseNetwork]):
    parts = [network]
    for block in excluded:
        next_parts = []
        for part in parts:
            if not part.overlaps(block):
                next_parts.append(part)
            elif block.supernet_of(part) or block == part:
                continue
            else:
                next_parts.extend(part.address_exclude(block))
        parts = next_parts
    return parts


def canonical(values: list[ipaddress._BaseNetwork], family: int):
    result = list(ipaddress.collapse_addresses(values))
    result = [value for value in result if value.version == family]
    result.sort(key=lambda value: (int(value.network_address), value.prefixlen))
    return result


def write_lines(path: Path, values: list[ipaddress._BaseNetwork]) -> None:
    path.write_text("".join(f"{value}\n" for value in values), encoding="ascii")


def read_nchnroutes(path: Path) -> list[ipaddress.IPv4Network]:
    values = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.startswith("route "):
            continue
        raw = line.split()[1]
        value = ipaddress.ip_network(raw, strict=True)
        if value.version == 4:
            values.append(value)
    return values


def read_china_ipv6(path: Path) -> list[ipaddress.IPv6Network]:
    values = []
    with path.open(encoding="utf-8", errors="replace") as source:
        for line in source:
            if not line.startswith("apnic|CN|ipv6|"):
                continue
            fields = line.rstrip("\n").split("|")
            if len(fields) < 5:
                continue
            start = ipaddress.IPv6Address(fields[3])
            prefix = int(fields[4])
            if prefix < 0 or prefix > 128:
                raise ValueError(f"invalid APNIC IPv6 prefix length: {line.strip()}")
            values.append(ipaddress.ip_network(f"{start}/{prefix}", strict=False))
    return values


def build(work: Path) -> tuple[list[ipaddress.IPv4Network], list[ipaddress.IPv6Network], dict]:
    routes4 = read_nchnroutes(work / "routes4.conf")
    v4 = canonical(
        [part for route in routes4 for part in subtract(route, list(SPECIAL_V4))],
        4,
    )
    global_v6 = ipaddress.ip_network("2000::/3")
    china_v6 = canonical(read_china_ipv6(work / "delegated-apnic-latest"), 6)
    v6 = canonical(subtract(global_v6, list(china_v6) + list(SPECIAL_V6)), 6)
    if not v4 or not v6:
        raise ValueError("generated route set is empty")
    if any(value.prefixlen == 0 for value in [*v4, *v6]):
        raise ValueError("default route is forbidden in split snapshot")
    metadata = {
        "generator_version": GENERATOR_VERSION,
        "generated_at": datetime.now(timezone.utc).replace(microsecond=0).isoformat(),
        "sources": URLS,
        "source_files": {
            "routes4.conf": hashlib.sha256((work / "routes4.conf").read_bytes()).hexdigest(),
            "delegated-apnic-latest": hashlib.sha256((work / "delegated-apnic-latest").read_bytes()).hexdigest(),
        },
        "ipv4_count": len(v4),
        "ipv6_count": len(v6),
    }
    return v4, v6, metadata


def validate(directory: Path) -> dict:
    metadata = json.loads((directory / "metadata.json").read_text(encoding="utf-8"))
    for family, maximum in (("ipv4", 32), ("ipv6", 128)):
        path = directory / f"overseas-{family[-2:]}.txt"
        values = [ipaddress.ip_network(line.strip(), strict=True) for line in path.read_text(encoding="ascii").splitlines() if line.strip()]
        if not values or any(value.version != (4 if family == "ipv4" else 6) for value in values):
            raise ValueError(f"invalid {family} snapshot")
        if any(value.prefixlen == 0 or value.prefixlen > maximum for value in values):
            raise ValueError(f"default or invalid {family} route")
        if values != sorted(values, key=lambda value: (int(value.network_address), value.prefixlen)):
            raise ValueError(f"{family} snapshot is not sorted")
        if len(values) != len(set(values)):
            raise ValueError(f"{family} snapshot contains duplicates")
        expected = metadata[f"{family}_count"]
        if len(values) != expected:
            raise ValueError(f"{family} metadata count mismatch")
        digest = hashlib.sha256((directory / f"overseas-{family[-2:]}.txt").read_bytes()).hexdigest()
        if digest != metadata[f"{family}_sha256"]:
            raise ValueError(f"{family} metadata hash mismatch")
    return metadata


def refresh() -> None:
    with tempfile.TemporaryDirectory(prefix="pmman-cnipgen-") as temporary:
        work = Path(temporary)
        for name, url in URLS.items():
            download(url, work / name)
        subprocess.run(
            ["python3", str(work / "produce.py"), "--next", "0.0.0.0", "--ipv4-list", "apnic", "ipip"],
            cwd=work,
            check=True,
            timeout=180,
        )
        v4, v6, metadata = build(work)
        candidate = Path(tempfile.mkdtemp(prefix="pmman-cnipgen-candidate-"))
        try:
            write_lines(candidate / "overseas-v4.txt", v4)
            write_lines(candidate / "overseas-v6.txt", v6)
            metadata["ipv4_sha256"] = hashlib.sha256((candidate / "overseas-v4.txt").read_bytes()).hexdigest()
            metadata["ipv6_sha256"] = hashlib.sha256((candidate / "overseas-v6.txt").read_bytes()).hexdigest()
            (candidate / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
            validate(candidate)
            RESOURCE_DIR.mkdir(parents=True, exist_ok=True)
            for name in ("overseas-v4.txt", "overseas-v6.txt", "metadata.json"):
                target = RESOURCE_DIR / name
                temporary_target = RESOURCE_DIR / f".{name}.tmp"
                shutil.copy2(candidate / name, temporary_target)
                os.replace(temporary_target, target)
        finally:
            shutil.rmtree(candidate, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--refresh", action="store_true")
    args = parser.parse_args()
    if args.refresh:
        refresh()
    metadata = validate(RESOURCE_DIR)
    print(json.dumps(metadata, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
