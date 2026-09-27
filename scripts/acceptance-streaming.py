#!/usr/bin/env python3
"""End-to-end generated sources, streamed HTTP responses, text operations, and reports."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import tempfile
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "target/debug" / ("mettle.exe" if os.name == "nt" else "mettle")


def call(*arguments: str, success: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run([str(BINARY), *arguments], cwd=ROOT, capture_output=True,
                            text=True, timeout=20)
    assert (result.returncode == 0) == success, (arguments, result.stdout, result.stderr)
    return result


def main() -> None:
    subprocess.run(["cargo", "build", "--locked", "--quiet", "-p", "mettle-cli"], cwd=ROOT, check=True)
    text = json.loads(call("run", "examples/language/text.mettle", "--raw").stdout)
    assert text == {"id": 1042, "label": "request_id", "names": ["Ada", "Lin"], "valid": True}
    call("test", "examples/language/text.mettle", "--jobs", "2")
    generated = json.loads(call("run", "examples/language/producers.mettle", "--raw").stdout)
    assert generated == {"completed": True}
    assert (ROOT / "target/producer-example.txt").read_text() == "Heading\nfirst\nsecond\n"
    assert (ROOT / "target/producer-lines.txt").read_text() == "first\nsecond\n"

    spec = importlib.util.spec_from_file_location("fixture", ROOT / "util/test-server/http_fixture.py")
    assert spec is not None and spec.loader is not None
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    server = fixture.FixtureServer(("127.0.0.1", 0))
    worker = threading.Thread(target=server.serve_forever)
    worker.start()
    try:
        # The example defaults to port 8080; override it with a temporary copy
        # whose sole difference is the loopback fixture address.
        source = (ROOT / "examples/http/streaming.mettle").read_text()
        source = source.replace("127.0.0.1:8080", f"127.0.0.1:{server.server_address[1]}")
        with tempfile.TemporaryDirectory(prefix="mettle-streaming-") as directory:
            program = Path(directory) / "streaming.mettle"
            program.write_text(source)
            result = json.loads(call("run", str(program), "--raw").stdout)
            assert result["firstUpload"] == {"status": 201, "accepted": True}
            assert result["secondUpload"] == {"status": 201, "accepted": True}
            assert result["objectDownload"]["bytesWritten"] > 0
            assert json.loads((ROOT / "target/stream-object.json").read_text())["name"] == "Ada"
            call("test", str(program), "--jobs", "2")
            verbose = call("run", str(program), "download", "--arg", "url=/content/object",
                           "--arg", "destination=target/stream-object.json", "--verbose")
            assert "headers received" in verbose.stdout, verbose.stdout
            assert "deferred response body" not in verbose.stdout

            program.write_text(f'''flow main {{
                response = http.get("http://127.0.0.1:{server.server_address[1]}/content/object", stream: true,
                    headers: {{ Authorization: "Bearer local-test-token" }})
                response
            }}\n''')
            failure = call("run", str(program), success=False)
            assert "live" in failure.stderr.lower(), failure.stderr
    finally:
        server.shutdown()
        server.server_close()
        worker.join()
    print("Streaming, producers, text, reports, and local HTTP fixture: OK")


if __name__ == "__main__":
    main()
