#!/usr/bin/env python3
"""Install the reusable pmman_network BIRD integration transactionally."""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import tempfile
from datetime import datetime, timezone
from pathlib import Path


MAIN = Path("/etc/bird/bird.conf")
TARGET = Path("/etc/bird/pmman_network.conf")
INCLUDE = 'include "/etc/bird/pmman_network.conf";'


def atomic_write(path: Path, data: bytes, mode: int | None = None) -> None:
    fd, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temp = Path(name)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        if mode is not None:
            os.chmod(temp, mode)
        os.replace(temp, path)
    finally:
        temp.unlink(missing_ok=True)


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if new in text:
        return text
    if text.count(old) != 1:
        raise RuntimeError(f"expected exactly one {label} block")
    return text.replace(old, new, 1)


def replace_any_once(text: str, olds: tuple[str, ...], new: str, label: str) -> str:
    if new in text:
        return text
    matches = [old for old in olds if old in text]
    if len(matches) != 1:
        raise RuntimeError(f"expected exactly one {label} block")
    return text.replace(matches[0], new, 1)


def add_filter_rule(text: str, name: str, rule: str, label: str) -> str:
    """Add a rule before a named filter's final reject, tolerating local comments."""
    pattern = re.compile(
        rf"(^\s*filter\s+{re.escape(name)}\s*\{{)(.*?)(^\s*reject;\s*$)",
        re.MULTILINE | re.DOTALL,
    )
    matches = list(pattern.finditer(text))
    if len(matches) != 1:
        raise RuntimeError(f"expected exactly one {label} block")
    match = matches[0]
    block = match.group(0)
    if rule in block:
        return text
    insertion = f"    {rule}\n"
    reject_start = match.start(3)
    return text[:reject_start] + insertion + text[reject_start:]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("config", type=Path)
    parser.add_argument("--check-only", action="store_true")
    parser.add_argument(
        "--restart-on-configure-failure",
        action="store_true",
        help="restart BIRD when a live reconfigure cannot start a new kernel protocol",
    )
    args = parser.parse_args()

    if os.geteuid() != 0:
        raise SystemExit("run this installer as root")
    if not MAIN.is_file() or not args.config.is_file():
        raise SystemExit("missing BIRD configuration input")

    main_before = MAIN.read_text(encoding="utf-8")
    resource_before = TARGET.read_bytes() if TARGET.exists() else None
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%d%H%M%S")
    backup = MAIN.with_name(f"bird.conf.pre-pmman-network-{timestamp}")

    main_after = main_before
    main_after = replace_any_once(
        main_after,
        (
            """        export filter {\n            if source != RTS_STATIC && net ~ OWNNETSETv6 then {""",
            """        export filter {\n            if source = RTS_PIPE then reject;\n            if source != RTS_STATIC && net ~ OWNNETSETv6 then {""",
        ),
        """        export filter {\n            if proto = \"pmman_network_direct\" || proto = \"pmman_network_kernel_v6\" then reject;\n            if source != RTS_STATIC && net ~ OWNNETSETv6 then {""",
        "IPv6 kernel export filter",
    )
    main_after = replace_any_once(
        main_after,
        (
            """        export filter {\n            if source = RTS_STATIC then reject;\n            krt_prefsrc = OWNIP;""",
            """        export filter {\n            # if source = RTS_STATIC then reject;\n            krt_prefsrc = OWNIP;""",
            """        export filter {\n            if source = RTS_PIPE then reject;\n            if source = RTS_STATIC then reject;\n            krt_prefsrc = OWNIP;""",
        ),
        """        export filter {\n            if proto = \"pmman_network_direct\" || proto = \"pmman_network_kernel_v4\" then reject;\n            if source = RTS_STATIC then reject;\n            krt_prefsrc = OWNIP;""",
        "IPv4 kernel export filter",
    )
    main_after = add_filter_rule(
        main_after,
        "ospf_export",
        'if proto = "pmman_network_direct" || proto = "pmman_network_kernel_v4" then accept;',
        "IPv4 OSPF export filter",
    )
    main_after = add_filter_rule(
        main_after,
        "ospf_export_v6",
        'if proto = "pmman_network_direct" || proto = "pmman_network_kernel_v6" then accept;',
        "IPv6 OSPF export filter",
    )
    if INCLUDE not in main_after:
        main_after = main_after.rstrip() + f"\n\n{INCLUDE}\n"

    if args.check_only:
        atomic_write(TARGET, args.config.read_bytes(), TARGET.stat().st_mode & 0o777)
        atomic_write(MAIN, main_after.encode(), MAIN.stat().st_mode & 0o777)
        return 0

    shutil.copyfile(MAIN, backup)
    try:
        atomic_write(TARGET, args.config.read_bytes(), 0o644)
        atomic_write(MAIN, main_after.encode(), MAIN.stat().st_mode & 0o777)
        checked = subprocess.run(
            ["bird", "-p", "-c", str(MAIN)], capture_output=True, text=True
        )
        if checked.returncode != 0:
            raise RuntimeError(checked.stderr.strip() or checked.stdout.strip())
        applied = subprocess.run(
            ["birdc", "configure"], capture_output=True, text=True
        )
        if applied.returncode != 0:
            if not args.restart_on_configure_failure:
                raise RuntimeError(applied.stderr.strip() or applied.stdout.strip())
            restarted = subprocess.run(
                ["systemctl", "restart", "bird"], capture_output=True, text=True
            )
            if restarted.returncode != 0:
                raise RuntimeError(
                    restarted.stderr.strip()
                    or restarted.stdout.strip()
                    or applied.stderr.strip()
                    or applied.stdout.strip()
                )
            healthy = subprocess.run(
                ["systemctl", "is-active", "--quiet", "bird"], check=False
            )
            if healthy.returncode != 0:
                raise RuntimeError("BIRD restart did not become active")
    except Exception:
        atomic_write(MAIN, backup.read_bytes(), backup.stat().st_mode & 0o777)
        if resource_before is None:
            TARGET.unlink(missing_ok=True)
        else:
            atomic_write(TARGET, resource_before, 0o644)
        subprocess.run(["birdc", "configure"], check=False)
        raise

    print(f"backup={backup}")
    print(f"installed={TARGET}")
    print("bird_configure=passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
