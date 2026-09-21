#!/usr/bin/env bash
set -euo pipefail

repository_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state_dir="$(mktemp -d)"

cleanup() {
  cleanup_status=$?
  if [[ -n "${server_pid:-}" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$state_dir"
  return "$cleanup_status"
}
trap cleanup EXIT

python3 "$repository_dir/util/test-server/http_fixture.py" \
  --port 0 \
  --port-file "$state_dir/port" \
  >"$state_dir/server.log" 2>&1 &
server_pid=$!

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
project="$repository_dir/tests/projects/basic/main.mettle"
result="$(
  METTLE_BASE_URL="http://127.0.0.1:$port" \
    METTLE_API_TOKEN="local-test-token" \
    cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- run "$project" --raw
)"
[[ "$result" == "seed-42" ]]

if METTLE_BASE_URL="http://127.0.0.1:$port" \
  METTLE_API_TOKEN="local-test-token" \
  cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  run "$project" failingAssertion \
  >"$state_dir/assert.out" 2>"$state_dir/assert.err"; then
  echo "failing assertion unexpectedly passed" >&2
  exit 1
fi
rg -q 'assertion failed' "$state_dir/assert.err"

secret='token that must not appear'
if METTLE_BASE_URL="http://127.0.0.1:$port" \
  METTLE_API_TOKEN="local-test-token" \
  METTLE_TEST_SECRET="$secret" \
  cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  run "$project" secretDiagnostic \
  >"$state_dir/secret.out" 2>"$state_dir/secret.err"; then
  echo "invalid secret URL unexpectedly succeeded" >&2
  exit 1
fi
rg -q '\[REDACTED\]' "$state_dir/secret.err"
if rg -Fq "$secret" "$state_dir/secret.err"; then
  echo "secret leaked into diagnostics" >&2
  exit 1
fi

if cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  check "$repository_dir/tests/projects/cycle/main.mettle" \
  >"$state_dir/cycle.out" 2>"$state_dir/cycle.err"; then
  echo "context cycle unexpectedly compiled" >&2
  exit 1
fi
rg -q 'first -> second -> first' "$state_dir/cycle.err"

if cargo run --quiet --manifest-path "$repository_dir/Cargo.toml" -- \
  check "$repository_dir/tests/projects/ambiguous/main.mettle" \
  >"$state_dir/ambiguous.out" 2>"$state_dir/ambiguous.err"; then
  echo "ambiguous namespace reference unexpectedly compiled" >&2
  exit 1
fi
rg -q 'provided by a.shared, b.shared' "$state_dir/ambiguous.err"

echo "Project, namespace, composition, assertion, and redaction checks passed."
