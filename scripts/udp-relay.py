"""Small disposable UDP relay for the mirrored-WSL acceptance lab."""
import argparse
import os
import socket
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("listen_port", type=int)
    parser.add_argument("target_host")
    parser.add_argument("target_port", type=int)
    args = parser.parse_args()

    relay = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    relay.bind(("127.0.0.1", args.listen_port))
    Path(f"/tmp/pmman-udp-relay-{args.listen_port}.pid").write_text(str(os.getpid()))
    peer = None
    while True:
        data, source = relay.recvfrom(65535)
        print(f"packet={len(data)} source={source}", flush=True)
        if source[0] == "127.0.0.1" and source[1] == args.target_port:
            if peer is not None:
                print(f"reverse={len(data)} target={peer}", flush=True)
                relay.sendto(data, peer)
        else:
            peer = source
            print(f"forward={len(data)} target={(args.target_host, args.target_port)}", flush=True)
            relay.sendto(data, (args.target_host, args.target_port))


if __name__ == "__main__":
    main()
