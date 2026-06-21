#!/usr/bin/env bash
# Phase-0 profiling harness for AScript (macOS).
# Builds a symbol-rich binary, then for each representative workload records:
#   - in-program hot-section time (time.monotonic) on VM and tree-walker
#   - peak RSS (/usr/bin/time -l)
#   - a CPU call-graph via macOS `sample`, attributed to buckets by parse_sample.py
#   - a samply profile (bench/out/<name>.json) for interactive Firefox-Profiler view
#
# Usage:  bench/profiling/run.sh
# View a flame graph later:  samply load bench/out/object_churn.json
set -euo pipefail
cd "$(dirname "$0")/../.."

BENCHES=(async_inline async_concurrent json_roundtrip object_churn workflow_loop \
         func_pipeline call_heavy server_request spawn_wake)
BIN=target/profiling/ascript
OUT=bench/out
mkdir -p "$OUT"

echo ">> building profiling binary (release codegen + debug symbols)"
cargo build --profile profiling --quiet

echo ">> timing (VM vs tree-walker) + peak RSS"
printf "%-18s | %10s | %12s | %8s\n" bench "VM ms" "tree-walk ms" "peak RSS"
for f in "${BENCHES[@]}"; do
  vm=$("$BIN" run "bench/profiling/$f.as" 2>/dev/null | grep -oE 'elapsed_ms=[0-9.]+' | cut -d= -f2)
  tw=$("$BIN" run --tree-walker "bench/profiling/$f.as" 2>/dev/null | grep -oE 'elapsed_ms=[0-9.]+' | cut -d= -f2)
  rss=$(/usr/bin/time -l "$BIN" run "bench/profiling/$f.as" 2>&1 >/dev/null | grep -i "maximum resident" | grep -oE '^ *[0-9]+' | tr -d ' ')
  printf "%-18s | %10.0f | %12.0f | %5d MB\n" "$f" "${vm:-0}" "${tw:-0}" "$(( ${rss:-0} / 1048576 ))"
done

echo ">> sampling (macOS sample, worker thread) + bucket attribution"
for f in "${BENCHES[@]}"; do
  "$BIN" run "bench/profiling/$f.as" >/dev/null 2>&1 &
  pid=$!
  sleep 0.3
  sample "$pid" 60 1 -file "$OUT/$f.sample.txt" -mayDie >/dev/null 2>&1 || true
  wait "$pid" 2>/dev/null || true
  # interactive flame graph artifact
  samply record --save-only -r 4000 -o "$OUT/$f.json" -- "$BIN" run "bench/profiling/$f.as" >/dev/null 2>&1 || true
  python3 bench/profiling/parse_sample.py "$f" "$OUT/$f.sample.txt"
done
