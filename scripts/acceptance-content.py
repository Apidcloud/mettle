#!/usr/bin/env python3
"""Portable codecs, kind conversions, representation metadata, and redaction."""

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


def execute(*arguments: str, success: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run([str(BINARY), *arguments], cwd=ROOT,
                            env={**os.environ, "METTLE_PRIVATE_NUMBER": "private-invalid-number"},
                            capture_output=True, text=True, timeout=20)
    assert (result.returncode == 0) == success, (arguments, result.stdout, result.stderr)
    return result


def main() -> None:
    subprocess.run(["cargo", "build", "--locked", "--quiet", "-p", "mettle-cli"], cwd=ROOT, check=True)
    sample = execute("run", "examples/language/content.mettle", "--raw")
    assert json.loads(sample.stdout) == {"id": 42, "jsonText": '{"active":true,"id":42}', "mediaType": "application/json"}
    execute("test", "examples/language/content.mettle", "--jobs", "2")

    spec = importlib.util.spec_from_file_location("fixture", ROOT / "util/test-server/http_fixture.py")
    assert spec is not None and spec.loader is not None
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    server = fixture.FixtureServer(("127.0.0.1", 0))
    worker = threading.Thread(target=server.serve_forever)
    worker.start()
    base = f"http://127.0.0.1:{server.server_address[1]}"
    try:
        http = execute("run", "examples/http/content.mettle", "main", "--arg", f"baseUrl={base}", "--raw")
        assert json.loads(http.stdout) == {"custom": "application/vnd.example.temperature+json", "inferred": "application/json", "raw": {"celsius": 23}, "text": "23"}
        with tempfile.TemporaryDirectory(prefix="mettle-content-") as temporary:
            directory = Path(temporary)
            path = directory / "case.mettle"

            def program(expression: str, *, success: bool = True, raw: bool = True) -> subprocess.CompletedProcess[str]:
                path.write_text(f'flow main = {expression}\n', encoding="utf-8")
                return execute("run", str(path), *( ["--raw"] if raw else ["--output", "json"] ), success=success)

            for bad, message in [
                ('senv("METTLE_PRIVATE_NUMBER") as number', "cannot convert"),
                ('1.5 as integer', "cannot convert"),
                ('"9223372036854775808" as integer', "cannot convert"),
                ('json.decode("9223372036854775808")', "64-bit range"),
                ('json.decode("1e999")', "finite number range"),
                ('json.decode("{")', "JSON"),
                ('json.encode({ key: "too long" }, maxBytes: 3)', "maxBytes"),
                ('text.decode("long", maxBytes: 2)', "maxBytes"),
                ('text.encode("hello", maxBytes: 0)', "positive integer"),
                ('json.decode("{}", maxBytes: 0)', "positive integer"),
            ]:
                failure = program(bad, success=False)
                assert message in failure.stderr, (bad, failure.stderr)
                assert "private-invalid-number" not in failure.stderr

            for expression in [
                'secret("23") as number',
                'secret(23) is number',
                'not secret(23) is number',
                'secret(23) is number and true',
                '(secret("23") as number) == 23',
                'text.decode(json.encode({ token: secret("private-token") }))',
            ]:
                result = program(expression, raw=False)
                assert "[REDACTED]" in result.stdout
                assert "private-token" not in result.stdout + result.stderr

            authorization = 'headers: { Authorization: "Bearer local-test-token" }'
            url = f'"{base}/method"'
            body = '{ valid: true }'
            good = program(f'http.post({url}, {authorization}, body: {body}, mediaType: json.mediaType)').stdout
            assert json.loads(good)["json"]["json"] == {"valid": True}
            for options, message in [
                ('body: "abc", maxBodyBytes: 2', "maxBodyBytes"),
                ('body: true, maxBodyBytes: 0', "positive integer"),
                ('body: "hi", mediaType: "text/plain;charset=latin1"', "charset"),
                ('body: { ok: true }, mediaType: "application/unknown"', "already encoded"),
                ('body: "hi", bodyFormat: "text"', "bodyFormat"),
                ('body: { ok: true }, mediaType: "bad"', "invalid media"),
            ]:
                assert message in program(f'http.post({url}, {authorization}, {options})', success=False).stderr

            path.write_text(f'''flow main = http.post({url}, body: {{ ok: true }}, mediaType: json.mediaType,
                headers: {{ Authorization: "Bearer local-test-token", "Content-Type": "Application/JSON; charset=\\"UTF-8\\"" }})\n''', encoding="utf-8")
            equivalent = json.loads(execute("run", str(path), "--raw").stdout)
            assert equivalent["json"]["json"] == {"ok": True}
            path.write_text(f'''flow main = http.post({url}, body: {{ ok: true }}, mediaType: json.mediaType,
                headers: {{ "Content-Type": "text/plain" }})\n''', encoding="utf-8")
            assert "conflicts" in execute("run", str(path), success=False).stderr
            path.write_text(f'''flow main = http.post({url}, body: {{ ok: true }},
                headers: {{ "Content-Type": "application/json", "content-type": "text/plain" }})\n''', encoding="utf-8")
            assert "duplicate Content-Type" in execute("run", str(path), success=False).stderr

            # Sources retain representation bytes and their original consumption rules.
            payload = directory / "payload.json"
            payload.write_bytes(b'{"ok":true}')
            source_path = json.dumps(str(payload))
            streamed = json.loads(program(f'http.post({url}, {authorization}, body: fs.stream({source_path}), mediaType: json.mediaType)').stdout)
            assert streamed["json"]["json"] == {"ok": True}
            assert "maxBodyBytes" in program(f'http.post({url}, {authorization}, body: fs.stream({source_path}), maxBodyBytes: 2)', success=False).stderr
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)
    print("Portable codecs, conversions, HTTP media types, limits, and redaction checks passed.")


if __name__ == "__main__":
    main()
