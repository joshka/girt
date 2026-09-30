#!/usr/bin/env python3
"""Serves Git repositories over smart HTTP for interoperability tests.

Bridges requests to `git http-backend` (the Git server, used here only as the remote side).
Usage: http_server.py <project-root> <port-file> [user:password]

Listens on an ephemeral loopback port and writes it to <port-file>. With credentials, every
request requires HTTP Basic authentication.
"""

import base64
import http.server
import os
import subprocess
import sys

root = sys.argv[1]
port_file = sys.argv[2]
credentials = sys.argv[3] if len(sys.argv) > 3 else None


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.serve()

    def do_POST(self):
        self.serve()

    def authorized(self):
        if credentials is None:
            return True
        header = self.headers.get("Authorization", "")
        expected = "Basic " + base64.b64encode(credentials.encode()).decode()
        return header == expected

    def serve(self):
        if not self.authorized():
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="test"')
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        path, _, query = self.path.partition("?")
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        env = {
            "PATH": os.environ["PATH"],
            "GIT_PROJECT_ROOT": root,
            "GIT_HTTP_EXPORT_ALL": "1",
            "REMOTE_USER": "tester",
            "REMOTE_ADDR": "127.0.0.1",
            "REQUEST_METHOD": self.command,
            "PATH_INFO": path,
            "QUERY_STRING": query,
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "CONTENT_LENGTH": str(len(body)),
            "GIT_PROTOCOL": self.headers.get("Git-Protocol", ""),
        }
        result = subprocess.run(
            ["git", "http-backend"], input=body, env=env, capture_output=True, check=False
        )
        head, _, payload = result.stdout.partition(b"\r\n\r\n")
        status = 200
        headers = []
        for line in head.split(b"\r\n"):
            if not line:
                continue
            name, _, value = line.decode("latin-1").partition(":")
            value = value.strip()
            if name.lower() == "status":
                status = int(value.split()[0])
            else:
                headers.append((name, value))
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w") as f:
    f.write(str(server.server_address[1]))
server.serve_forever()
