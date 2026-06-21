#!/usr/bin/env bash
# EXEC Task 10 — same-session A/B: the bespoke executor (CANDIDATE, default) vs
# stock tokio (BASELINE, ASCRIPT_EXECUTOR=tokio), SAME binary, interleaved per
# workload so thermal/load drift cancels. speedup = base/cand (>1 = bespoke faster).
#   Usage: bench/exec_ab.sh [runs=7]
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=target/release/ascript
RUNS="${1:-7}"

# Async-scheduler-bound (the ≥10% win target) | neutral (zero-regression) | char.
ASYNC=(async_inline async_concurrent spawn_wake)
NEUTRAL=(func_pipeline call_heavy server_request object_churn json_roundtrip)
CHAR=(race_compute)
ALL=("${ASYNC[@]}" "${NEUTRAL[@]}" "${CHAR[@]}")

median() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }
run_ms() { ASCRIPT_EXECUTOR="$1" "$BIN" run "bench/profiling/$2.as" 2>/dev/null \
            | grep -oE 'elapsed_ms=[0-9.]+' | head -1 | cut -d= -f2; }
rss_mb() { ASCRIPT_EXECUTOR="$1" /usr/bin/time -l "$BIN" run "bench/profiling/$2.as" 2>&1 >/dev/null \
            | grep -i "maximum resident" | grep -oE '[0-9]+' | head -1 | awk '{printf "%d", $1/1048576}'; }

printf "%-16s | %10s | %10s | %8s | %7s %7s\n" workload "tokio ms" "besp ms" "speedup" "tokMB" "bespMB"
async_ln=0; an=0
for f in "${ALL[@]}"; do
  ts=(); cs=()
  for ((r=0; r<RUNS; r++)); do
    ts+=("$(run_ms tokio "$f")"); cs+=("$(run_ms bespoke "$f")")
  done
  tm=$(printf '%s\n' "${ts[@]}" | median); cm=$(printf '%s\n' "${cs[@]}" | median)
  sp=$(awk -v b="$tm" -v c="$cm" 'BEGIN {printf "%.3f", (c>0)? b/c : 0}')
  trss=$(rss_mb tokio "$f"); crss=$(rss_mb bespoke "$f")
  printf "%-16s | %10.0f | %10.0f | %7sx | %7s %7s\n" "$f" "$tm" "$cm" "$sp" "$trss" "$crss"
  case " ${ASYNC[*]} " in *" $f "*) async_ln=$(awk -v t="$async_ln" -v s="$sp" 'BEGIN {print t+log(s)}'); an=$((an+1));; esac
done
echo "---"
awk -v t="$async_ln" -v n="$an" 'BEGIN {printf "ASYNC-CORPUS geomean speedup (bespoke/tokio) = %.3fx  (ship gate: >= 1.10)\n", exp(t/n)}'
