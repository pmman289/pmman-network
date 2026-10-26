"""HTTP probes bound only to the development gateway's dual-stack addresses."""
import http.server
import socket
import threading
import socketserver
import struct


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'PMMan txgz real gateway\n'
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class IPv6Server(http.server.ThreadingHTTPServer):
    address_family = socket.AF_INET6


servers = [http.server.ThreadingHTTPServer(('10.88.0.1', 8081), Handler),
           IPv6Server(('fd88:1000::1', 8081), Handler)]
for server in servers:
    threading.Thread(target=server.serve_forever).start()


def resolve(query):
    for address in ('183.60.83.19', '183.60.82.98', '223.5.5.5'):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as upstream:
            upstream.settimeout(2)
            try:
                upstream.connect((address, 53))
                upstream.send(query)
                return upstream.recv(65535)
            except OSError:
                continue
    raise OSError('all development DNS upstreams timed out')


class DNS(socketserver.BaseRequestHandler):
    def handle(self):
        try:
            query, listener = self.request
            listener.sendto(resolve(query), self.client_address)
        except OSError:
            pass


class DNS6(socketserver.ThreadingUDPServer):
    address_family = socket.AF_INET6


class DNSTCP(socketserver.BaseRequestHandler):
    def handle(self):
        try:
            self.request.settimeout(5)
            header = self.request.recv(2)
            if len(header) != 2:
                return
            remaining = struct.unpack('!H', header)[0]
            query = b''
            while remaining:
                chunk = self.request.recv(remaining)
                if not chunk:
                    return
                query += chunk
                remaining -= len(chunk)
            answer = resolve(query)
            self.request.sendall(struct.pack('!H', len(answer)) + answer)
        except OSError:
            pass


class DNSTCP6(socketserver.ThreadingTCPServer):
    address_family = socket.AF_INET6
    allow_reuse_address = True


socketserver.ThreadingTCPServer.allow_reuse_address = True
for server in (socketserver.ThreadingUDPServer(('10.88.0.1', 53), DNS),
               DNS6(('fd88:1000::1', 53), DNS),
               socketserver.ThreadingTCPServer(('10.88.0.1', 53), DNSTCP),
               DNSTCP6(('fd88:1000::1', 53), DNSTCP)):
    threading.Thread(target=server.serve_forever).start()
