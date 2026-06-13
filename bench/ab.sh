#!/usr/bin/env bash
# Same-session A/B harness (PERF campaign Gate 16). Runs BASELINE and CANDIDATE
# ascript binaries interleaved over the profiling workloads, reports per-workload
# medians, the candidate/baseline speedup, the geomean, and peak RSS (Gate 18).
#   Usage: bench/ab.sh <baseline-binary> <candidate-binary> [runs=5]
set -euo pipefail
cd "$(dirname "$0")/.."
BASE="$1"; CAND="$2"; RUNS="${3:-5}"
BENCHES=(async_inline async_concurrent json_roundtrip object_churn workflow_loop \
         func_pipeline call_heavy server_request)

median() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }
run_ms() { "$1" run "bench/profiling/$2.as" 2>/dev/null \
            | grep -oE 'elapsed_ms=[0-9.]+' | cut -d= -f2; }
peak_rss_mb() { /usr/bin/time -l "$1" run "bench/profiling/$2.as" 2>&1 >/dev/null \
            | grep -i "maximum resident" | grep -oE '[0-9]+' | head -1 \
            | awk '{printf "%d", $1 / 1048576}'; }

printf "%-16s | %10s | %10s | %8s | %6s | %6s\n" bench "base ms" "cand ms" speedup baseMB candMB
total_ln=0; n=0
for f in "${BENCHES[@]}"; do
  bs=(); cs=()
  for ((r=0; r<RUNS; r++)); do            # interleave: same-session, same thermal state
    bs+=("$(run_ms "$BASE" "$f")"); cs+=("$(run_ms "$CAND" "$f")")
  done
  bm=$(printf '%s\n' "${bs[@]}" | median); cm=$(printf '%s\n' "${cs[@]}" | median)
  sp=$(awk -v b="$bm" -v c="$cm" 'BEGIN {printf "%.3f", b / c}')
  brss=$(peak_rss_mb "$BASE" "$f"); crss=$(peak_rss_mb "$CAND" "$f")
  printf "%-16s | %10.0f | %10.0f | %7sx | %6s | %6s\n" "$f" "$bm" "$cm" "$sp" "$brss" "$crss"
  total_ln=$(awk -v t="$total_ln" -v s="$sp" 'BEGIN {print t + log(s)}'); n=$((n+1))
done
awk -v t="$total_ln" -v n="$n" 'BEGIN {printf "geomean speedup = %.3fx\n", exp(t / n)}'
