#!/usr/bin/env python3
"""Small deterministic HTTP/1.1 fixture for Mettle examples and acceptance tests."""

from __future__ import annotations

import argparse
import json
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
        self._json(404, {"error": "not found"})

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if not self._authorized():
            return
        if self.path != "/users":
            self._json(404, {"error": "not found"})
            return
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
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

    def log_message(self, format: str, *args: object) -> None:
        return

    def _authorized(self) -> bool:
        if self.headers.get("Authorization") == "Bearer local-test-token":
            return True
        self._json(401, {"error": "missing or invalid token"})
        return False

    def _json(self, status: int, value: object) -> None:
        body = json.dumps(value, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
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
