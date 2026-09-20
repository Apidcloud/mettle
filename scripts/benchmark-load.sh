#!/usr/bin/env bash
set -euo pipefail

repository_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_dir="${1:-$repository_dir/target/benchmarks/load-$(date -u +%Y%m%dT%H%M%SZ)}"
state_dir="$(mktemp -d)"
server_pid=""
benchmark_pid=""

cleanup() {
  if [[ -n "$benchmark_pid" ]]; then
    kill "$benchmark_pid" 2>/dev/null || true
    wait "$benchmark_pid" 2>/dev/null || true
  fi
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$state_dir"
}
trap cleanup EXIT

mkdir -p "$output_dir"
python3 "$repository_dir/util/test-server/http_fixture.py" \
  --port 0 --port-file "$state_dir/port" \
  >"$output_dir/fixture.log" 2>&1 &
server_pid=$!

for _ in {1..100}; do
  [[ -s "$state_dir/port" ]] && break
  kill -0 "$server_pid" 2>/dev/null || { cat "$output_dir/fixture.log" >&2; exit 1; }
  sleep 0.05
done
[[ -s "$state_dir/port" ]] || { echo "HTTP fixture did not start" >&2; exit 1; }

port="$(<"$state_dir/port")"
base_url="http://127.0.0.1:$port"
workload="$repository_dir/tests/fixtures/load-benchmark.mettle"
binary="$repository_dir/target/release/mettle"

cargo build --quiet --release --manifest-path "$repository_dir/Cargo.toml"

{
  echo "timestampUtc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "revision=$(git -C "$repository_dir" rev-parse HEAD)"
  if [[ -n "$(git -C "$repository_dir" status --porcelain)" ]]; then
    echo "workingTreeDirty=true"
  else
    echo "workingTreeDirty=false"
  fi
  echo "os=$(uname -srmo)"
  echo "cpu=$(LC_ALL=C lscpu | awk -F: '/Model name/ {sub(/^[[:space:]]+/, "", $2); print $2; exit}')"
  echo "logicalCpus=$(nproc)"
  echo "memoryKiB=$(awk '/MemTotal/ {print $2}' /proc/meminfo)"
  echo "rustc=$(rustc --version)"
  echo "fixture=Python ThreadingHTTPServer on 127.0.0.1:$port"
  echo "command=METTLE_BASE_URL=$base_url $binary run $workload --verbose"
} >"$output_dir/environment.txt"

started_ns="$(date +%s%N)"
METTLE_BASE_URL="$base_url" "$binary" run "$workload" --verbose \
  >"$output_dir/result.json" &
benchmark_pid=$!
peak_rss_kib=0
cpu_ticks=0
while kill -0 "$benchmark_pid" 2>/dev/null; do
  if [[ -r "/proc/$benchmark_pid/status" ]]; then
    rss_kib="$(awk '/VmRSS/ {print $2}' "/proc/$benchmark_pid/status")"
    if [[ -n "$rss_kib" && "$rss_kib" -gt "$peak_rss_kib" ]]; then
      peak_rss_kib="$rss_kib"
    fi
  fi
  if [[ -r "/proc/$benchmark_pid/stat" ]]; then
    read -r -a process_stat <"/proc/$benchmark_pid/stat"
    cpu_ticks="$((process_stat[13] + process_stat[14]))"
  fi
  sleep 0.02
done
wait "$benchmark_pid"
benchmark_pid=""
finished_ns="$(date +%s%N)"
clock_ticks="$(getconf CLK_TCK)"
{
  echo "elapsedNanoseconds=$((finished_ns - started_ns))"
  echo "cpuTicks=$cpu_ticks"
  echo "clockTicksPerSecond=$clock_ticks"
  echo "peakResidentSetKiB=$peak_rss_kib"
} >"$output_dir/resources.txt"

python3 - "$output_dir/result.json" <<'PY'
import json
import pathlib
import sys

result = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert result["scheduled"] == 5_000, result
assert result["started"] + result["dropped"] == 5_000, result
assert result["failed"] == 0, result
PY

echo "Load benchmark written to $output_dir"
