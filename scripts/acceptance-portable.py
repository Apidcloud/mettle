#!/usr/bin/env python3
"""Cross-platform smoke test for the installed Mettle runtime surface."""

from __future__ import annotations

import json
import importlib.util
import os
import subprocess
import sys
import threading
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
EXECUTABLE = ROOT / "target" / "debug" / ("mettle.exe" if os.name == "nt" else "mettle")


def run(*arguments: str, environment: dict[str, str]) -> subprocess.CompletedProcess[str]:
    command = [str(EXECUTABLE), *arguments]
    result = subprocess.run(
        command,
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(f"Command failed with exit code {result.returncode}: {' '.join(command)}", file=sys.stderr)
        if result.stdout:
            print("stdout:", file=sys.stderr)
            print(result.stdout, file=sys.stderr)
        if result.stderr:
            print("stderr:", file=sys.stderr)
            print(result.stderr, file=sys.stderr)
        result.check_returncode()
    return result


def fixture_server() -> object:
    fixture_path = ROOT / "util" / "test-server" / "http_fixture.py"
    specification = importlib.util.spec_from_file_location("mettle_http_fixture", fixture_path)
    if specification is None or specification.loader is None:
        raise RuntimeError(f"could not load HTTP fixture from {fixture_path}")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module.FixtureServer(("127.0.0.1", 0))


def main() -> None:
    subprocess.run(
        ["cargo", "build", "--quiet", "--package", "mettle-cli"],
        cwd=ROOT,
        check=True,
    )
    fixture = fixture_server()
    fixture_thread = threading.Thread(target=fixture.serve_forever, daemon=True)
    fixture_thread.start()
    try:
        port = fixture.server_address[1]
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

        methods = json.loads(
            run(
                "run",
                "tests/fixtures/http-methods.mettle",
                "--raw",
                environment=environment,
            ).stdout
        )
        assert methods["posted"]["method"] == "POST", methods
        assert methods["put"]["method"] == "PUT", methods
        assert methods["patched"]["method"] == "PATCH", methods
        assert methods["posted"]["json"] == {"action": "create"}, methods
        assert methods["posted"]["contentType"] == "application/json", methods
        assert methods["put"]["body"] == "replacement", methods
        assert methods["put"]["contentType"].startswith("text/plain"), methods
        assert methods["patched"]["json"] == {"active": True}, methods
        assert methods["patched"]["contentType"] == "application/merge-patch+json", methods
        assert methods["jsonString"]["json"] == "Ada", methods
        assert methods["namedUrl"]["json"] == [1, 2], methods
        assert methods["rawCopy"]["contentType"] == "application/octet-stream", methods
        assert methods["rawCopy"]["body"] == methods["postedResponseBody"], methods
        assert methods["deleted"]["method"] == "DELETE", methods
        assert methods["headBody"] == "", methods
        assert "OPTIONS" in methods["allowed"], methods
        assert methods["responseContentType"] == "application/json", methods

        workload = json.loads(
            run(
                "run",
                "tests/fixtures/portable-load.mettle",
                "steady",
                "--raw",
                environment=environment,
            ).stdout
        )
        assert workload["scheduled"] == 10, workload
        assert workload["success"] == 10, workload
        assert workload["failed"] == 0, workload
        assert workload["dropped"] == 0, workload
    finally:
        fixture.shutdown()
        fixture.server_close()
        fixture_thread.join(timeout=5)

    # Examples are user-facing documentation: keep every file compilable, then
    # execute the network-free language demos without relying on public APIs.
    example_environment = os.environ.copy()
    for example in sorted((ROOT / "examples").rglob("*.mettle")):
        relative = example.relative_to(ROOT).as_posix()
        run("check", relative, environment=example_environment)
    for name in ("basics", "conditionals", "contexts", "collections-and-numbers", "echo"):
        relative = f"examples/language/{name}.mettle"
        run("run", relative, "--raw", environment=example_environment)
        run("test", relative, "--quiet", environment=example_environment)
    example_environment["API_TOKEN"] = "safe-demo-token"
    run("run", "examples/language/secrets.mettle", "--raw", environment=example_environment)
    run("test", "examples/language/secrets.mettle", "--quiet", environment=example_environment)

    print(f"Portable HTTP, load, and example smoke tests passed on {sys.platform}.")


if __name__ == "__main__":
    main()
