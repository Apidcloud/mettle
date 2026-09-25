#!/usr/bin/env bash
set -euo pipefail

repository_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state_dir="$(mktemp -d)"
server_pid=""
flow_pid=""

cleanup() {
  if [[ -n "$flow_pid" ]]; then
    kill "$flow_pid" 2>/dev/null || true
    wait "$flow_pid" 2>/dev/null || true
  fi
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$state_dir"
}
trap cleanup EXIT

python3 "$repository_dir/util/test-server/http_fixture.py" \
  --port 0 --port-file "$state_dir/port" \
  >"$state_dir/server.log" 2>&1 &
server_pid=$!

for _ in {1..100}; do
  [[ -s "$state_dir/port" ]] && break
  kill -0 "$server_pid" 2>/dev/null || { cat "$state_dir/server.log" >&2; exit 1; }
  sleep 0.05
done
[[ -s "$state_dir/port" ]] || { echo "HTTP fixture did not start" >&2; exit 1; }

port="$(<"$state_dir/port")"
base_url="http://127.0.0.1:$port"
cargo build --quiet --manifest-path "$repository_dir/Cargo.toml"

result="$(METTLE_BASE_URL="$base_url" "$repository_dir/target/debug/mettle" \
  run "$repository_dir/tests/fixtures/execution-policies.mettle" policies --raw)"
python3 - "$result" <<'PY'
import json
import sys

result = json.loads(sys.argv[1])
assert result["retryAttempt"] == 3, result
assert len(result["resultCount"]) == 4, result
assert result["maximumConcurrency"] == 2, result
PY

if METTLE_BASE_URL="$base_url" "$repository_dir/target/debug/mettle" \
  run "$repository_dir/tests/fixtures/execution-policies.mettle" deadline \
  >"$state_dir/deadline.out" 2>"$state_dir/deadline.err"; then
  echo "deadline unexpectedly completed" >&2
  exit 1
fi
rg -q 'deadline exceeded after 50ms' "$state_dir/deadline.err"

METTLE_BASE_URL="$base_url" "$repository_dir/target/debug/mettle" \
  run "$repository_dir/tests/fixtures/execution-policies.mettle" cancellable \
  >"$state_dir/cancel.out" 2>"$state_dir/cancel.err" &
flow_pid=$!
sleep 0.1
kill -INT "$flow_pid"
set +e
wait "$flow_pid"
status=$?
set -e
flow_pid=""
[[ "$status" -eq 130 ]] || { echo "cancelled flow exited with $status" >&2; exit 1; }
rg -qi 'execution cancelled' "$state_dir/cancel.err" || {
  echo "cancellation diagnostic was missing:" >&2
  cat "$state_dir/cancel.err" >&2
  exit 1
}

echo "Structured execution acceptance checks passed."
