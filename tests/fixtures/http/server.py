#!/usr/bin/env python3
"""Disposable loopback Git CGI bridge; original fixture, never a production server."""
import argparse
import http.server
import os
import ssl
import subprocess
import time
import urllib.parse

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("repository")
parser.add_argument("--fault", default="")
parser.add_argument("--authorization", default="")
parser.add_argument("--certificate")
parser.add_argument("--key")
parser.add_argument("--requests")
args = parser.parse_args()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.exchange()

    def do_POST(self):
        self.exchange()

    def exchange(self):
        if args.requests:
            with open(args.requests, "a") as log:
                log.write(self.command + "\n")
        if args.fault == "stall" or (args.fault == "post-stall" and self.command == "POST"):
            time.sleep(10)
            return
        if args.authorization and self.headers.get("Authorization") != args.authorization:
            self.send_error(401)
            return
        fault = args.fault
        if fault in ("redirect", "failure") or (fault == "post-failure" and self.command == "POST"):
            self.send_response(302 if fault == "redirect" else 503)
            self.send_header("Location", "http://127.0.0.1:1/credential-trap")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        size = int(self.headers.get("Content-Length", "0"))
        if size > 512 * 1024 * 1024:
            self.send_error(413)
            return
        request = self.rfile.read(size)
        path = urllib.parse.urlsplit(self.path)
        if not path.path.startswith("/repo/"):
            self.send_error(404)
            return
        env = {
            "PATH": os.environ["PATH"],
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_HTTP_EXPORT_ALL": "1",
            "PATH_TRANSLATED": "." + path.path[len("/repo"):],
            "REQUEST_METHOD": self.command,
            "QUERY_STRING": path.query,
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "CONTENT_LENGTH": str(size),
            "REMOTE_USER": "fixture",
        }
        # Windows process startup needs SystemRoot; retain only these OS locations, not Git overrides.
        for key in ("SYSTEMROOT", "WINDIR", "TEMP", "TMP"):
            if key in os.environ:
                env[key] = os.environ[key]
        # Keep Git's CGI path relative to the process cwd; Rust's Windows verbatim path
        # is valid for process startup but is not a Git CGI path spelling.
        result = subprocess.run(["git", "http-backend"], input=request, capture_output=True,
                                cwd=args.repository, env=env, check=True, timeout=15)
        headers, body = result.stdout.split(b"\r\n\r\n", 1)
        status = 200
        fields = []
        for line in headers.split(b"\r\n"):
            name, value = line.decode("ascii").split(":", 1)
            if name.lower() == "status":
                status = int(value.split()[0])
            else:
                fields.append((name, value.strip()))
        if fault == "v2":
            body = b"001e# service=git-upload-pack\n0000000eversion 2\n0000"
        if fault == "prelude":
            body = body.replace(b"# service=git-upload-pack", b"# service=git-broken-pack", 1)
        if fault in ("partial", "partial-stall") and self.command == "POST":
            # Retain unpack + first complete ref acknowledgement, then truncate HTTP framing.
            first = int(body[:4], 16)
            second = int(body[first:first + 4], 16)
            body = body[:first + second]
        self.send_response(status)
        for name, value in fields:
            if name.lower() == "content-type" and (fault == "media" or (fault == "post-media" and self.command == "POST")):
                value = "text/plain"
            self.send_header(name, value)
        if fault == "duplicate-type":
            self.send_header("Content-Type", "text/plain")
        if fault == "encoding":
            self.send_header("Content-Encoding", "gzip")
        if fault == "headers":
            self.send_header("X-Large", "x" * 20000)
        if fault == "malformed-header":
            self.wfile.write(b"HTTP/1.1 200 OK\r\nbad header\r\n\r\n")
            return
        truncated = fault in ("truncate", "body-stall") or (fault == "post-truncate" and self.command == "POST") or (fault in ("partial", "partial-stall") and self.command == "POST")
        self.send_header("Content-Length", str(len(body) + (100 if truncated else 0)))
        self.end_headers()
        try:
            self.wfile.write(body)
            self.wfile.flush()
            if fault == "body-stall" or (fault == "partial-stall" and self.command == "POST"):
                time.sleep(10)
        except (BrokenPipeError, ConnectionResetError):
            pass
        self.close_connection = True


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
server.daemon_threads = True
if args.certificate:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.certificate, args.key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_port, flush=True)
server.serve_forever()
