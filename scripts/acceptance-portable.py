#!/usr/bin/env python3
"""Cross-platform smoke test for the installed Mettle runtime surface."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
EXECUTABLE = ROOT / "target" / "debug" / ("mettle.exe" if os.name == "nt" else "mettle")


def run(*arguments: str, environment: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(EXECUTABLE), *arguments],
        cwd=ROOT,
        env=environment,
        check=True,
        capture_output=True,
        text=True,
    )


def wait_for_port(process: subprocess.Popen[str], port_file: Path) -> str:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if port_file.exists() and (port := port_file.read_text(encoding="utf-8").strip()):
            return port
        if process.poll() is not None:
            output, _ = process.communicate()
            raise RuntimeError(f"HTTP fixture exited before startup:\n{output}")
        time.sleep(0.05)
    raise TimeoutError("HTTP fixture did not publish its port within 10 seconds")


def main() -> None:
    subprocess.run(
        ["cargo", "build", "--quiet", "--package", "mettle-cli"],
        cwd=ROOT,
        check=True,
    )
    with tempfile.TemporaryDirectory(prefix="mettle-portable-") as state:
        state_path = Path(state)
        port_file = state_path / "port"
        fixture = subprocess.Popen(
            [
                sys.executable,
                str(ROOT / "util" / "test-server" / "http_fixture.py"),
                "--port",
                "0",
                "--port-file",
                str(port_file),
            ],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            port = wait_for_port(fixture, port_file)
            environment = os.environ.copy()
            environment.update(
                {
                    "METTLE_BASE_URL": f"http://127.0.0.1:{port}",
                    "METTLE_API_TOKEN": "local-test-token",
                }
            )

            response = json.loads(
                run(
                    "run",
                    "tests/fixtures/http.mettle",
                    "--raw",
                    environment=environment,
                ).stdout
            )
            assert response["id"] == "created-seed-42", response
            assert response["seedConnection"] == response["connectionId"], response

            workload = json.loads(
                run(
                    "run",
                    "tests/fixtures/load.mettle",
                    "steady",
                    "--raw",
                    environment=environment,
                ).stdout
            )
            assert workload["scheduled"] == 20, workload
            assert workload["success"] == 20, workload
            assert workload["failed"] == 0, workload
            assert workload["dropped"] == 0, workload
        finally:
            fixture.terminate()
            try:
                fixture.wait(timeout=5)
            except subprocess.TimeoutExpired:
                fixture.kill()
                fixture.wait(timeout=5)

    print(f"Portable HTTP and load smoke tests passed on {sys.platform}.")


if __name__ == "__main__":
    main()
