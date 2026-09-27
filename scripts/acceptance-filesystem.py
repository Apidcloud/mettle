#!/usr/bin/env python3
"""Portable filesystem, execution-directory, and streamed-upload acceptance."""

from __future__ import annotations

import hashlib
import http.client
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import tempfile
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "target" / "debug" / ("mettle.exe" if os.name == "nt" else "mettle")
FLOW = ROOT / "tests" / "fixtures" / "filesystem.mettle"


def execute(directory: Path, environment: dict[str, str], name: str,
            *, source: Path = FLOW, success: bool = True,
            arguments: tuple[str, ...] = ()) -> object:
    result = subprocess.run([str(BINARY), "run", str(source), name, "--raw", *arguments],
                            cwd=directory, env=environment, capture_output=True,
                            text=True, timeout=20)
    if success:
        assert result.returncode == 0, (name, result.stdout, result.stderr)
        return json.loads(result.stdout) if result.stdout.strip().startswith(("{", "[")) else result.stdout.strip()
    assert result.returncode != 0, (name, result.stdout)
    return result.stderr


def main() -> None:
    subprocess.run(["cargo", "build", "--locked", "--quiet", "-p", "mettle-cli"], cwd=ROOT, check=True)
    spec = importlib.util.spec_from_file_location("fixture", ROOT / "util" / "test-server" / "http_fixture.py")
    assert spec is not None and spec.loader is not None
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    server = fixture.FixtureServer(("127.0.0.1", 0))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    environment = os.environ.copy()
    environment["METTLE_BASE_URL"] = f"http://127.0.0.1:{server.server_address[1]}"
    try:
        # Pending inbound bytes must not reset and truncate the early 413.
        # Deliberately leave the declared upload unfinished, with no sleeps.
        with socket.create_connection(server.server_address, timeout=5) as connection:
            headers = (
                "POST /reject-upload HTTP/1.1\r\n"
                "Host: localhost\r\n"
                "Authorization: Bearer local-test-token\r\n"
                "Content-Length: 1048576\r\n\r\n"
            ).encode("ascii")
            connection.sendall(headers + b"x" * 262144)
            response = http.client.HTTPResponse(connection)
            response.begin()
            assert response.status == 413
            assert json.loads(response.read()) == {"error": "upload rejected before consumption"}
            connection.shutdown(socket.SHUT_WR)
        with tempfile.TemporaryDirectory(prefix="mettle-fs-") as temporary:
            directory = Path(temporary)
            data = bytes(range(256)) * 1024 + b"last chunk"
            (directory / "payload.bin").write_bytes(data)
            (directory / "payload.txt").write_text("Hello filesystem", encoding="utf-8")
            (directory / "invalid.bin").write_bytes(b"\xff")
            # Entry file is outside cwd: relative I/O must use the invocation directory.
            assert execute(directory, environment, "complete") == "Hello filesystem"
            assert execute(directory, environment, "copy") == {"bytesWritten": len(data)}
            assert (directory / "copy.bin").read_bytes() == data
            assert "already exists" in execute(directory, environment, "copy", success=False)
            uploaded = execute(directory, environment, "upload")
            assert uploaded == {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(),
                                "contentType": "application/octet-stream", "chunked": True}, uploaded
            buffered = execute(directory, environment, "bufferedUpload")
            assert buffered["sha256"] == uploaded["sha256"] and not buffered["chunked"], buffered
            assert execute(directory, environment, "rejectedUpload") == "413"
            assert "already consumed" in execute(directory, environment, "reused", success=False)
            assert "execution result" in execute(directory, environment, "escaped", success=False)
            assert "maxBytes" in execute(directory, environment, "boundedRead", success=False)
            assert "UTF-8" in execute(directory, environment, "invalidText", success=False)
            assert "maxBytes" in execute(directory, environment, "boundedWrite", success=False)
            assert not (directory / "bounded.bin").exists()
            assert execute(directory, environment, "helperCopy") == {"bytesWritten": len(data)}
            assert "already consumed" in execute(directory, environment, "concurrentReaders", success=False)
            assert not list(directory.glob(".mettle-*.tmp"))
            if os.name != "nt":
                os.mkfifo(directory / "pipe")
                (directory / "special.mettle").write_text('flow main = fs.read("pipe")\n', encoding="utf-8")
                assert "regular file" in execute(directory, environment, "main", source=directory / "special.mettle", success=False)

            project = directory / "project"
            (project / "data").mkdir(parents=True)
            (project / "sub").mkdir()
            (project / "payload.txt").write_text("project root", encoding="utf-8")
            (project / "data" / "payload.txt").write_text("configured directory", encoding="utf-8")
            (project / "sub" / "main.mettle").write_text('flow main = helper()\n', encoding="utf-8")
            (project / "helper.mettle").write_text('flow helper = fs.readText("payload.txt")\n', encoding="utf-8")
            manifest = project / "mettle.toml"
            manifest.write_text('name = "filesystem"\n', encoding="utf-8")
            assert execute(directory, environment, "main", source=project / "sub" / "main.mettle") == "project root"
            manifest.write_text('name = "filesystem"\nworkingDir = "./data"\n', encoding="utf-8")
            assert execute(directory, environment, "main", source=project / "sub" / "main.mettle") == "configured directory"
            manifest.write_text('workingDir = "./missing"\n', encoding="utf-8")
            assert "workingDir" in execute(directory, environment, "main", source=project / "sub" / "main.mettle", success=False)

            # Execute checked-in examples, not just parser-check them. Keep generated
            # project output isolated from the repository and any existing copy.
            assert execute(directory, environment, "inspectFile",
                           source=ROOT / "examples/language/files.mettle",
                           arguments=("--arg", "path=payload.txt")) == {
                               "text": "Hello filesystem", "reusableBytes": True}
            example = directory / "example"
            shutil.copytree(ROOT / "examples/language/filesystem", example,
                            ignore=shutil.ignore_patterns("copy.txt"))
            expected = (example / "data/payload.txt").read_bytes()
            for _ in range(2):
                assert execute(directory, environment, "main", source=example / "main.mettle") == {
                    "bytesWritten": len(expected)}
                assert (example / "data/copy.txt").read_bytes() == expected
            checks = subprocess.run([str(BINARY), "test", str(example / "main.mettle"),
                                     "--output", "json"],
                                    cwd=directory, env=environment, capture_output=True,
                                    text=True, timeout=20)
            assert checks.returncode == 0, (checks.stdout, checks.stderr)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    print("Portable filesystem, project paths, source ownership, and streamed upload checks passed.")


if __name__ == "__main__":
    main()
