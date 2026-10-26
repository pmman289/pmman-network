"""Read-only UDP/TCP DNS probe; no packages or DNS configuration changes needed."""
import argparse
import ipaddress
import json
import secrets
import socket
import struct
import time


def receive_exact(stream, count):
    result = b""
    while len(result) < count:
        chunk = stream.recv(count - len(result))
        if not chunk:
            raise OSError("DNS TCP response closed early")
        result += chunk
    return result


def skip_name(data, offset):
    while data[offset]:
        size = data[offset]
        if size & 0xC0 == 0xC0:
            return offset + 2
        offset += size + 1
    return offset + 1


def query(server, name, qtype, tcp, timeout):
    identifier = secrets.randbelow(65536)
    question = b"".join(bytes([len(x)]) + x.encode("ascii") for x in name.split("."))
    packet = struct.pack("!6H", identifier, 256, 1, 0, 0, 0) + question + b"\0"
    packet += struct.pack("!2H", qtype, 1)
    family = socket.AF_INET6 if ipaddress.ip_address(server).version == 6 else socket.AF_INET
    started = time.monotonic()
    with socket.socket(family, socket.SOCK_STREAM if tcp else socket.SOCK_DGRAM) as stream:
        stream.settimeout(timeout)
        stream.connect((server, 53))
        stream.sendall((struct.pack("!H", len(packet)) if tcp else b"") + packet)
        data = receive_exact(stream, struct.unpack("!H", receive_exact(stream, 2))[0]) if tcp else stream.recv(65535)
    response_id, flags, questions, answers, _, _ = struct.unpack("!6H", data[:12])
    if response_id != identifier or not flags & 0x8000:
        raise ValueError("Invalid DNS response")
    offset = 12
    for _ in range(questions):
        offset = skip_name(data, offset) + 4
    addresses = []
    for _ in range(answers):
        offset = skip_name(data, offset)
        kind, _, _, length = struct.unpack("!HHIH", data[offset:offset + 10])
        offset += 10
        value = data[offset:offset + length]
        if kind in (1, 28):
            addresses.append(str(ipaddress.ip_address(value)))
        offset += length
    return {"rcode": flags & 15, "answer_count": answers, "addresses": addresses,
            "truncated": bool(flags & 0x200), "ms": round((time.monotonic() - started) * 1000, 1)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", action="append", required=True)
    parser.add_argument("--name", action="append")
    parser.add_argument("--timeout", type=float, default=3)
    args = parser.parse_args()
    failed = False
    for server in args.server:
        for name in args.name or ["baidu.com", "www.google.com"]:
            for qtype in (1, 28):
                for tcp in (False, True):
                    result = {"server": server, "name": name, "type": "A" if qtype == 1 else "AAAA",
                              "transport": "TCP" if tcp else "UDP"}
                    try:
                        result.update(query(server, name, qtype, tcp, args.timeout))
                        failed |= result["rcode"] != 0 or result["truncated"]
                    except (OSError, ValueError, IndexError, struct.error) as error:
                        result["error"] = str(error)
                        failed = True
                    print(json.dumps(result), flush=True)
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
