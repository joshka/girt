#!/usr/bin/env python3
"""Original disposable CONNECT relay; routes only to a fixed loopback TLS fixture."""
import argparse
import http.server
import select
import socket

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("port", type=int)
parser.add_argument("requests")
parser.add_argument("--authorization", default="")
args = parser.parse_args()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_CONNECT(self):
        with open(args.requests, "a") as log:
            log.write("CONNECT\n")
        if self.path != f"localhost:{args.port}":
            self.send_error(403)
            return
        if self.headers.get("Proxy-Authorization", "") != args.authorization:
            self.send_error(407)
            return
        with socket.create_connection(("127.0.0.1", args.port), timeout=10) as upstream:
            self.send_response(200)
            self.end_headers()
            self.connection.settimeout(10)
            while True:
                readable, _, _ = select.select([self.connection, upstream], [], [], 10)
                if not readable:
                    return
                for source in readable:
                    data = source.recv(65536)
                    if not data:
                        return
                    target = upstream if source is self.connection else self.connection
                    target.sendall(data)


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_port, flush=True)
server.serve_forever()
