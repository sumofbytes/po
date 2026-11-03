#!/usr/bin/env bash
# Streams a scripted reply into a profiled Po for each history size and prints what it cost.
# Nobody needs to touch the window. It opens for about half a minute per size, in a scratch
# config, so nothing of yours is read or saved.
#
#   cargo build --release --features hotpath-alloc
#   scripts/bench-stream.sh                 # histories 2, 20 and 80
#   scripts/bench-stream.sh 5 50            # your own
#
# Environment: PO_BIN (default target/release/po), CHUNKS (600), INTERVAL_MS (25),
# CORES (the cores to run on, "3"; "" for no pinning), COOL_C (wait until the machine is below
# this many degrees before each run, 70; needs `sensors`).
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
bin=${PO_BIN:-$repo/target/release/po}
chunks=${CHUNKS:-600}
interval=${INTERVAL_MS:-25}
cores=${CORES-3}
cool=${COOL_C:-70}
histories=("$@")
[ ${#histories[@]} -gt 0 ] || histories=(2 20 80)
out=$repo/target/bench
mkdir -p "$out"

pin=()
[ -z "$cores" ] || pin=(taskset -c "$cores")

# The engine keeps a core busy for the whole run, so give the machine time to cool between runs.
wait_cool() {
  command -v sensors >/dev/null || return 0
  local t
  while t=$(sensors | awk '/Tctl|Package id 0/{gsub(/[+°C]/,"",$2); print int($2); exit}'); [ -n "$t" ] && [ "$t" -ge "$cool" ]; do
    echo "waiting for the machine to cool (${t}C, want below ${cool}C)" >&2
    sleep 10
  done
}

for history in "${histories[@]}"; do
  wait_cool
  scratch=$(mktemp -d "$out/home.XXXXXX")
  report=$out/stream-$history.json
  rm -f "$report"
  # A command that never answers: the blank thread starts it, and no agent is run for real.
  if ! env XDG_CONFIG_HOME="$scratch/config" XDG_DATA_HOME="$scratch/data" XDG_CACHE_HOME="$scratch/cache" \
    PO_AGENT="sleep 3600" PO_BENCH="stream:$history:$chunks:$interval" \
    HOTPATH_METRICS_SERVER_OFF=1 HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH="$report" HOTPATH_LIMIT=0 \
    timeout 600 "${pin[@]}" nice -n 19 "$bin" -f "$repo" >/dev/null 2>"$scratch/stderr"; then
    echo "history=$history: Po did not finish (see $scratch/stderr)" >&2
    continue
  fi
  rm -rf "$scratch"
done

exec python3 "$repo/scripts/bench-stream-summary.py" "$out" "${histories[@]}"
