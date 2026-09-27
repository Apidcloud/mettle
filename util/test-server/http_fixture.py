#!/usr/bin/env python3
"""Small deterministic HTTP/1.1 fixture for Mettle examples and acceptance tests."""

from __future__ import annotations

import argparse
import hashlib
import json
import socket
import ssl
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address: tuple[str, int]) -> None:
        super().__init__(address, FixtureHandler)
        self._connection_ids: dict[int, int] = {}
        self._connection_lock = threading.Lock()
        self._policy_lock = threading.Lock()
        self._flaky_attempts = 0
        self._active_work = 0
        self._maximum_work = 0

    def connection_id(self, socket_id: int) -> int:
        with self._connection_lock:
            if socket_id not in self._connection_ids:
                self._connection_ids[socket_id] = len(self._connection_ids) + 1
            return self._connection_ids[socket_id]

    def flaky_attempt(self) -> int:
        with self._policy_lock:
            self._flaky_attempts += 1
            return self._flaky_attempts

    def work_started(self) -> None:
        with self._policy_lock:
            self._active_work += 1
            self._maximum_work = max(self._maximum_work, self._active_work)

    def work_finished(self) -> None:
        with self._policy_lock:
            self._active_work -= 1

    def work_stats(self) -> dict[str, int]:
        with self._policy_lock:
            return {"active": self._active_work, "maximum": self._maximum_work}


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server: FixtureServer

    def handle_one_request(self) -> None:
        try:
            super().handle_one_request()
        except (ValueError, BrokenPipeError, ConnectionResetError):
            # A bounded/failed upload can close its request before the fixture
            # receives the final chunk. Do not emit unrelated server tracebacks.
            self.close_connection = True

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if not self._authorized():
            return
        if self.path == "/seed":
            self._json(
                200,
                {
                    "id": "seed-42",
                    "name": "Ada",
                    "connectionId": self.server.connection_id(self.connection.fileno()),
                },
            )
            return
        if self.path == "/slow":
            time.sleep(0.2)
            self._json(200, {"completed": True})
            return
        if self.path == "/hang":
            time.sleep(10)
            self._json(200, {"completed": True})
            return
        if self.path == "/flaky":
            attempt = self.server.flaky_attempt()
            if attempt < 3:
                self.close_connection = True
                self.connection.shutdown(2)
                self.connection.close()
                return
            self._json(200, {"attempt": attempt})
            return
        if self.path.startswith("/work/"):
            self.server.work_started()
            try:
                time.sleep(0.05)
                self._json(200, {"path": self.path})
            finally:
                self.server.work_finished()
            return
        if self.path == "/parallel-stats":
            self._json(200, self.server.work_stats())
            return
        if self.path == "/slow-body":
            body = b'{"completed":true}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body[:1])
                self.wfile.flush()
                time.sleep(0.2)
                self.wfile.write(body[1:])
            except (BrokenPipeError, ConnectionResetError):
                pass
            return
        if self.path == "/large":
            body = b"x" * 1024
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path == "/invalid-json":
            body = b'{"incomplete":'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self._json(404, {"error": "not found"})

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if not self._authorized():
            return
        if self.path == "/reject-upload":
            self.close_connection = True
            self._json(413, {"error": "upload rejected before consumption"})
            # Closing with unread upload bytes can reset TCP and discard the
            # response. Send a FIN, then discard a bounded amount of transport
            # data without parsing/processing the rejected application body.
            self.wfile.flush()
            deadline = time.monotonic() + 1.0
            remaining = 10 * 1024 * 1024
            try:
                self.connection.shutdown(socket.SHUT_WR)
                while remaining > 0:
                    timeout = deadline - time.monotonic()
                    if timeout <= 0:
                        break
                    self.connection.settimeout(timeout)
                    chunk = self.connection.recv(min(65536, remaining))
                    if not chunk:
                        break
                    remaining -= len(chunk)
            except OSError:
                pass  # EOF, peer reset, or the bounded drain deadline.
            return
        if self.path == "/upload":
            body = self._read_body()
            self._json(201, {
                "bytes": len(body),
                "sha256": hashlib.sha256(body).hexdigest(),
                "contentType": self.headers.get("Content-Type", ""),
                "chunked": self.headers.get("Transfer-Encoding", "").lower() == "chunked",
            })
            return
        if self.path == "/method":
            self._method_response()
            return
        if self.path != "/users":
            self._json(404, {"error": "not found"})
            return
        body = self._read_body()
        try:
            request = json.loads(body)
        except json.JSONDecodeError:
            self._json(400, {"error": "invalid JSON"})
            return
        self._json(
            201,
            {
                "id": f"created-{request['sourceId']}",
                "name": request["name"],
                "active": request["active"],
                "roles": request["roles"],
                "seedConnection": request["seedConnection"],
                "connectionId": self.server.connection_id(self.connection.fileno()),
            },
        )

    def do_PUT(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        self._method_response()

    def do_PATCH(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        self._method_response()

    def do_DELETE(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        self._method_response()

    def do_HEAD(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if not self._authorized():
            return
        if self.path != "/method":
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("X-Mettle-Method", self.command)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_OPTIONS(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if not self._authorized():
            return
        if self.path != "/method":
            self._json(404, {"error": "not found"})
            return
        self.send_response(204)
        self.send_header("Allow", "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, format: str, *args: object) -> None:
        return

    def _authorized(self) -> bool:
        if self.headers.get("Authorization") == "Bearer local-test-token":
            return True
        self._json(401, {"error": "missing or invalid token"})
        return False

    def _method_response(self) -> None:
        if not self._authorized():
            return
        if self.path != "/method":
            self._json(404, {"error": "not found"})
            return
        body = self._read_body().decode("utf-8")
        content_type = self.headers.get("Content-Type", "")
        parsed_json = None
        if body and (content_type.split(";", 1)[0].lower().endswith("json")):
            parsed_json = json.loads(body)
        self._json(
            200,
            {
                "method": self.command,
                "body": body,
                "json": parsed_json,
                "contentType": content_type,
            },
        )

    def _read_body(self) -> bytes:
        limit = 10 * 1024 * 1024
        if self.headers.get("Transfer-Encoding", "").lower() != "chunked":
            length = int(self.headers.get("Content-Length", "0"))
            if length < 0 or length > limit:
                raise ValueError("fixture body limit exceeded")
            body = self.rfile.read(length)
            if len(body) != length:
                raise ValueError("truncated fixture body")
            return body
        body = bytearray()
        while True:
            line = self.rfile.readline(8192)
            size = int(line.split(b";", 1)[0].strip(), 16)
            if size == 0:
                for _ in range(100):
                    trailer = self.rfile.readline(8192)
                    if trailer == b"\r\n":
                        return bytes(body)
                    if not trailer or not trailer.endswith(b"\r\n"):
                        raise ValueError("invalid chunk trailers")
                raise ValueError("too many chunk trailers")
            if size < 0 or len(body) + size > limit:
                raise ValueError("fixture body limit exceeded")
            chunk = self.rfile.read(size)
            if len(chunk) != size:
                raise ValueError("truncated chunk")
            body.extend(chunk)
            if self.rfile.read(2) != b"\r\n":
                raise ValueError("invalid chunk framing")

    def _json(self, status: int, value: object) -> None:
        body = json.dumps(value, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        if self.close_connection:
            self.send_header("Connection", "close")
        self.end_headers()
        try:
            self.wfile.write(body)
        except BrokenPipeError:
            pass


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", default=8089, type=int)
    parser.add_argument("--port-file", type=Path)
    parser.add_argument("--tls-cert", type=Path)
    parser.add_argument("--tls-key", type=Path)
    arguments = parser.parse_args()

    server = FixtureServer((arguments.host, arguments.port))
    if bool(arguments.tls_cert) != bool(arguments.tls_key):
        parser.error("--tls-cert and --tls-key must be provided together")
    scheme = "http"
    if arguments.tls_cert and arguments.tls_key:
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(arguments.tls_cert, arguments.tls_key)
        server.socket = tls.wrap_socket(server.socket, server_side=True)
        scheme = "https"
    actual_port = server.server_address[1]
    if arguments.port_file:
        arguments.port_file.write_text(str(actual_port), encoding="utf-8")
    print(
        f"Mettle HTTP fixture listening on {scheme}://{arguments.host}:{actual_port}",
        flush=True,
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
