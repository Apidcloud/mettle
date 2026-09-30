#!/usr/bin/env python3
"""Portable codecs, kind conversions, representation metadata, and redaction."""

from __future__ import annotations

import gzip
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
                            env={**os.environ, "METTLE_PRIVATE_NUMBER": "private-invalid-number", "METTLE_TEST_AUTH": "Bearer local-test-token"},
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
            assert json.loads(good)["body"]["json"] == {"valid": True}
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
            assert equivalent["body"]["json"] == {"ok": True}
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
            assert streamed["body"]["json"] == {"ok": True}
            assert "maxBodyBytes" in program(f'http.post({url}, {authorization}, body: fs.stream({source_path}), maxBodyBytes: 2)', success=False).stderr

            # Incoming representation selection never invents a JSON value kind.
            for case, expected, media in [
                ("object", {"name": "Ada", "json": "ordinary field"}, "application/json"),
                ("array", [1, True, None], "application/json"),
                ("number", 23, "application/json"),
                ("boolean", True, "application/json"),
                ("null", None, "application/json"),
                ("string", "hello", "application/json"),
                ("suffix", {"celsius": 23}, "application/vnd.example+json; charset=utf-8; version=One"),
                ("text", '{"not":"decoded as JSON"}', "text/plain; charset=utf-8"),
                ("binary", [0, 255, 1], "application/octet-stream"),
                ("unknown", list(b'{"not":"sniffed"}'), "application/x-unknown"),
                ("missing", list(b'{"not":"sniffed"}'), None),
                ("empty-text", "", "text/plain"),
                ("empty-bytes", [], "application/octet-stream"),
                ("no-content", None, "application/json"),
                ("reset-content", None, "application/json"),
                ("not-modified", None, "application/json"),
                ("error", {"error": "invalid input"}, "application/json"),
            ]:
                result = json.loads(program(f'http.get("{base}/content/{case}", {authorization})').stdout)
                assert result["body"] == expected, (case, result)
                assert result["mediaType"] == media, (case, result)
                assert "json" not in result, (case, result)
                assert isinstance(result["bodyBytes"], list), (case, result)
            for case, message in [
                ("empty-json", "invalid JSON"), ("invalid-json", "invalid JSON"),
                ("invalid-text", "UTF-8"), ("invalid-type", "Content-Type"),
                ("duplicate-type", "duplicate"), ("unsupported-charset", "charset"),
                ("unsupported-encoding", "Content-Encoding: br"), ("overflow", "64-bit range"),
            ]:
                failure = program(f'http.get("{base}/content/{case}", {authorization})', success=False)
                assert message in failure.stderr, (case, failure.stderr)
            head = json.loads(program(f'http.head("{base}/content/object", {authorization}, maxResponseBytes: 1)').stdout)
            assert head["body"] is None and head["bodyBytes"] == [], head
            # Bodyless responses have nothing to decode, so any Content-Encoding is accepted.
            encoded_head = json.loads(program(f'http.head("{base}/content/unsupported-encoding", {authorization})').stdout)
            assert encoded_head["body"] is None, encoded_head
            assert "byte limit" in program(f'http.get("{base}/content/object", {authorization}, maxResponseBytes: 1)', success=False).stderr
            # The dedicated gzip fixture compresses the wire representation before sending it.
            compressed = json.loads(program(f'http.get("{base}/gzip", {authorization})').stdout)
            assert compressed["body"] == {"name": "Ada", "json": "ordinary field"}, compressed
            assert compressed["mediaType"] == "application/json", compressed
            assert compressed["bodyBytes"][:2] == [0x1F, 0x8B], compressed
            # Protected response headers retain their sensitivity after decoding.
            sensitive = program(f'http.get("{base}/content/secret", headers: {{ Authorization: senv("METTLE_TEST_AUTH") }}).headers', raw=False)
            assert "local-test-token" not in sensitive.stdout + sensitive.stderr, sensitive
            assert "[REDACTED]" in sensitive.stdout, sensitive

            # Machine output stays parseable; human output renders one decoded body.
            human = execute("run", str(path), "--verbose", "--no-color")
            assert "bodyBytes" not in human.stdout and "local-test-token" not in human.stdout, human.stdout
            assert "Body" in human.stdout, human.stdout
            incoming = execute("run", "examples/http/incoming-content.mettle", "--arg", f"baseUrl={base}", "--raw")
            assert json.loads(incoming.stdout)["user"]["name"] == "Ada", incoming.stdout
            gzip_example = execute("run", "examples/http/gzip.mettle", "--arg", f"baseUrl={base}", "--raw")
            gzip_bodies = gzip_example.stdout.strip().split(" :: ")
            assert len(gzip_bodies) == 2, gzip_example.stdout
            assert all(
                json.loads(body) == {"name": "Ada", "json": "ordinary field"}
                for body in gzip_bodies
            ), gzip_example.stdout
            streamed_gzip = json.loads(execute("run", "examples/http/gzip.mettle", "streamed", "--arg", f"baseUrl={base}", "--raw").stdout)
            assert streamed_gzip["body"] == {"name": "Ada", "json": "ordinary field"}, streamed_gzip
            raw_download = (ROOT / "target/gzip-response.json.gz").read_bytes()
            assert raw_download[:2] == b"\x1f\x8b", raw_download[:2]
            assert json.loads(gzip.decompress(raw_download)) == streamed_gzip["body"], raw_download
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=5)
    print("Portable codecs, conversions, HTTP media types, limits, and redaction checks passed.")


if __name__ == "__main__":
    main()
