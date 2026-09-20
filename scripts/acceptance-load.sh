#!/usr/bin/env bash
set -euo pipefail

repository_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state_dir="$(mktemp -d)"
server_pid=""

cleanup() {
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

run_load() {
  METTLE_BASE_URL="$base_url" "$repository_dir/target/debug/mettle" \
    run "$repository_dir/tests/fixtures/load.mettle" "$1" --verbose
}

steady="$(run_load steady)"
saturated="$(run_load saturated)"
fixed="$(run_load fixedConcurrency)"
maximum="$(METTLE_BASE_URL="$base_url" "$repository_dir/target/debug/mettle" \
  run "$repository_dir/tests/fixtures/load.mettle" serverMaximumConcurrency --raw)"

python3 - "$steady" "$saturated" "$fixed" "$maximum" <<'PY'
import json
import sys

steady, saturated, fixed = (json.loads(value) for value in sys.argv[1:4])
maximum = int(sys.argv[4])

assert steady["scheduled"] == 20, steady
assert steady["count"] == 20, steady
assert steady["success"] == 20, steady
assert steady["dropped"] == 0, steady
assert steady["rate"]["actual"] == 40.0, steady

assert saturated["scheduled"] == 100, saturated
assert saturated["dropped"] > 0, saturated
assert saturated["started"] + saturated["dropped"] == 100, saturated
assert saturated["saturated"] is True, saturated

assert fixed["count"] >= 4, fixed
assert fixed["failed"] == 0, fixed
assert maximum <= 4, maximum
PY

echo "Local load-engine acceptance checks passed."
