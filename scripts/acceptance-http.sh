#!/usr/bin/env bash
set -euo pipefail

repository_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state_dir="$(mktemp -d)"
server_pids=()

cleanup() {
  for server_pid in "${server_pids[@]}"; do
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  done
  rm -rf "$state_dir"
}
trap cleanup EXIT

python3 "$repository_dir/util/test-server/http_fixture.py" \
  --port 0 \
  --port-file "$state_dir/port" \
  >"$state_dir/server.log" 2>&1 &
server_pid=$!
server_pids+=("$server_pid")

for _ in {1..100}; do
  if [[ -s "$state_dir/port" ]]; then
    break
  fi
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$state_dir/server.log" >&2
    exit 1
  fi
  sleep 0.05
done

if [[ ! -s "$state_dir/port" ]]; then
  echo "HTTP fixture did not start" >&2
  exit 1
fi

port="$(cat "$state_dir/port")"
result="$(
  cd "$repository_dir"
  METTLE_BASE_URL="http://127.0.0.1:$port" \
    METTLE_API_TOKEN="local-test-token" \
    cargo run --quiet -- run tests/fixtures/http.mettle --raw
)"

python3 - "$result" <<'PY'
import json
import sys

result = json.loads(sys.argv[1])
assert result["id"] == "created-seed-42", result
assert result["active"] is True, result
assert result["roles"] == ["tester"], result
assert result["seedConnection"] == result["connectionId"], result
print(json.dumps(result, indent=2))
PY

methods_result="$(
  cd "$repository_dir"
  METTLE_BASE_URL="http://127.0.0.1:$port" \
    cargo run --quiet -- run tests/fixtures/http-methods.mettle --raw
)"

python3 - "$methods_result" <<'PY'
import json
import sys

result = json.loads(sys.argv[1])
assert result["posted"]["method"] == "POST", result
assert result["posted"]["json"] == {"action": "create"}, result
assert result["posted"]["contentType"] == "application/json", result
assert result["put"]["method"] == "PUT", result
assert result["put"]["body"] == "replacement", result
assert result["put"]["contentType"] == "text/plain; charset=utf-8", result
assert result["patched"]["method"] == "PATCH", result
assert result["patched"]["json"] == {"active": True}, result
assert result["deleted"]["method"] == "DELETE", result
assert result["headBody"] == "", result
assert "PATCH" in result["allowed"], result
assert result["responseContentType"] == "application/json", result
PY

anonymous_result="$(
  cd "$repository_dir"
  METTLE_BASE_URL="http://127.0.0.1:$port" \
    METTLE_API_TOKEN="local-test-token" \
    cargo run --quiet -- run tests/fixtures/entry-flows.mettle --line 12 --raw
)"

named_result="$(
  cd "$repository_dir"
  METTLE_BASE_URL="http://127.0.0.1:$port" \
    METTLE_API_TOKEN="local-test-token" \
    cargo run --quiet -- run tests/fixtures/entry-flows.mettle getSeed --raw \
      --arg "baseUrl=http://127.0.0.1:$port" \
      --arg apiToken=local-test-token
)"

python3 - "$anonymous_result" "$named_result" <<'PY'
import json
import sys

anonymous = json.loads(sys.argv[1])
named = json.loads(sys.argv[2])
assert anonymous["status"] == 200, anonymous
assert anonymous["json"]["id"] == "seed-42", anonymous
assert named["status"] == 200, named
assert named["json"]["id"] == "seed-42", named
PY

if cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  check "$repository_dir/tests/fixtures/invalid-http-option.mettle" \
  >"$state_dir/invalid.out" 2>"$state_dir/invalid.err"; then
  echo "invalid HTTP option unexpectedly compiled" >&2
  exit 1
fi

rg -q 'unknown option `banana`' "$state_dir/invalid.err"

if cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  check "$repository_dir/tests/fixtures/invalid-http-body-options.mettle" \
  >"$state_dir/body-options.out" 2>"$state_dir/body-options.err"; then
  echo "conflicting HTTP body options unexpectedly compiled" >&2
  exit 1
fi

rg -q 'options `json` and `body` cannot be used together' "$state_dir/body-options.err"

if METTLE_BASE_URL="http://127.0.0.1:$port" \
  cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  run "$repository_dir/tests/fixtures/invalid-http-content-type.mettle" \
  >"$state_dir/content-type.out" 2>"$state_dir/content-type.err"; then
  echo "JSON with a non-JSON Content-Type unexpectedly ran" >&2
  exit 1
fi

rg -q 'JSON request body requires a JSON Content-Type' "$state_dir/content-type.err"

if METTLE_BASE_URL="http://127.0.0.1:$port" \
  cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  run "$repository_dir/tests/fixtures/http-response-limit.mettle" \
  >"$state_dir/response-limit.out" 2>"$state_dir/response-limit.err"; then
  echo "oversized HTTP response unexpectedly completed" >&2
  exit 1
fi

rg -q 'HTTP response exceeded the 100 byte limit' "$state_dir/response-limit.err"

if METTLE_BASE_URL="http://127.0.0.1:$port" \
  cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  run "$repository_dir/tests/fixtures/invalid-http-json.mettle" \
  >"$state_dir/invalid-json.out" 2>"$state_dir/invalid-json.err"; then
  echo "malformed declared JSON unexpectedly completed" >&2
  exit 1
fi

rg -q 'HTTP response declared JSON but its body could not be decoded' "$state_dir/invalid-json.err"

if METTLE_BASE_URL="http://127.0.0.1:$port" \
  cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  run "$repository_dir/tests/fixtures/http-timeout.mettle" \
  >"$state_dir/timeout.out" 2>"$state_dir/timeout.err"; then
  echo "slow HTTP request unexpectedly completed" >&2
  exit 1
fi

rg -q 'exceeded its 50 ms timeout' "$state_dir/timeout.err"

python3 "$repository_dir/util/test-server/http_fixture.py" \
  --port 0 \
  --port-file "$state_dir/tls-port" \
  --tls-cert "$repository_dir/tests/fixtures/tls/localhost-cert.pem" \
  --tls-key "$repository_dir/tests/fixtures/tls/localhost-key.pem" \
  >"$state_dir/tls-server.log" 2>&1 &
tls_server_pid=$!
server_pids+=("$tls_server_pid")

for _ in {1..100}; do
  if [[ -s "$state_dir/tls-port" ]]; then
    break
  fi
  if ! kill -0 "$tls_server_pid" 2>/dev/null; then
    cat "$state_dir/tls-server.log" >&2
    exit 1
  fi
  sleep 0.05
done

if [[ ! -s "$state_dir/tls-port" ]]; then
  echo "HTTPS fixture did not start" >&2
  exit 1
fi

tls_port="$(cat "$state_dir/tls-port")"
tls_url="https://localhost:$tls_port"
tls_result="$(
  cd "$repository_dir"
  METTLE_BASE_URL="$tls_url" cargo run --quiet -- run tests/fixtures/https-insecure.mettle --raw
)"
[[ "$tls_result" == "200" ]]

if METTLE_BASE_URL="$tls_url" cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  run "$repository_dir/tests/fixtures/https-secure.mettle" \
  >"$state_dir/tls-secure.out" 2>"$state_dir/tls-secure.err"; then
  echo "self-signed HTTPS unexpectedly passed secure verification" >&2
  exit 1
fi

rg -qi 'certificate|issuer|unknownca' "$state_dir/tls-secure.err"
echo "HTTP acceptance checks passed."
