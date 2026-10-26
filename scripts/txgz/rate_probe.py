"""Temporary dual-stack HTTP traffic source for the real rate-limit check.

This process is started only for a bounded acceptance run. It binds the
project's test gateway addresses, serves a fixed response for downloads, and
drains uploads before replying. It deliberately exposes no files or control
data.
"""

from __future__ import annotations

import argparse
import http.server
import os
import signal
import socket
import socketserver
import threading
from pathlib import Path


class Handler(http.server.BaseHTTPRequestHandler):
    server_version = "PMManRateProbe/1"

    def do_GET(self) -> None:  # noqa: N802 - stdlib handler API
        size = self.server.payload_size  # type: ignore[attr-defined]
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(size))
        self.send_header("Connection", "close")
        self.end_headers()
        chunk = b"\0" * (64 * 1024)
        remaining = size
        while remaining:
            part = chunk if remaining >= len(chunk) else chunk[:remaining]
            self.wfile.write(part)
            remaining -= len(part)

    def do_POST(self) -> None:  # noqa: N802 - stdlib handler API
        try:
            remaining = int(self.headers.get("Content-Length", "-1"))
        except ValueError:
            remaining = -1
        if remaining < 0 or remaining > self.server.payload_size:  # type: ignore[attr-defined]
            self.send_error(400)
            return
        while remaining:
            data = self.rfile.read(min(64 * 1024, remaining))
            if not data:
                return
            remaining -= len(data)
        body = b"received\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args: object) -> None:
        return


class IPv6Server(socketserver.ThreadingTCPServer):
    address_family = socket.AF_INET6
    allow_reuse_address = True


class IPv4Server(socketserver.ThreadingTCPServer):
    address_family = socket.AF_INET
    allow_reuse_address = True


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=18081)
    parser.add_argument("--size", type=int, default=8 * 1024 * 1024)
    parser.add_argument("--pid-file", default="/tmp/pmman-rate-probe.pid")
    args = parser.parse_args()

    servers: list[socketserver.ThreadingTCPServer] = []
    for server_type, address in (
        (IPv4Server, ("10.88.0.1", args.port)),
        (IPv6Server, ("fd88:1000::1", args.port)),
    ):
        server = server_type(address, Handler)
        server.payload_size = args.size  # type: ignore[attr-defined]
        servers.append(server)

    pid_file = Path(args.pid_file)
    pid_file.write_text(str(os.getpid()), encoding="ascii")
    stop = threading.Event()

    def shutdown(_signum: int, _frame: object) -> None:
        if not stop.is_set():
            stop.set()
            for server in servers:
                threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, shutdown)
    signal.signal(signal.SIGINT, shutdown)
    threads = [
        threading.Thread(target=server.serve_forever, daemon=True) for server in servers
    ]
    for thread in threads:
        thread.start()
    stop.wait()
    for server in servers:
        server.server_close()
    try:
        pid_file.unlink()
    except FileNotFoundError:
        pass


if __name__ == "__main__":
    main()
