#!/usr/bin/env bash
# bench/run_elide_bench.sh
#
# ELIDE baseline harness + same-session A/B (ELIDE §7, plan Task 0.1 + Task 5.1).
#
# Measures wall time + peak RSS for the call-heavy workloads (the ELIDE headline
# pair) and the supporting corpus workloads under the release binary.
#
# Phase 0 (baseline): run on the unmodified branch to capture pre-ELIDE numbers.
# Phase 5 (A/B): re-run with BENCH_LABEL= set to each phase label to capture the
# elide-on vs --no-elide split.
#
# Each workload is run RUNS times; the script reports the MEDIAN wall time and the
# RSS from the final run (RSS is stable run-to-run; timing is subject to thermal
# drift, so median over RUNS cancels outliers while staying fast).
#
# Usage:
#   ./bench/run_elide_bench.sh                          # 5 runs per workload (default/no-elide)
#   RUNS=7 ./bench/run_elide_bench.sh                   # override run count
#   BINARY=path/to/ascript ./bench/run_elide_bench.sh   # custom binary
#   BENCH_LABEL="post-ELIDE elide-on" ELIDE_FLAG=--elide ./bench/run_elide_bench.sh
#   ELIDE_FLAG=--no-elide ./bench/run_elide_bench.sh    # explicit no-elide
#
# Output:
#   bench/ELIDE_RESULTS.md  (section APPENDED; run after each phase)
#   A summary table to stdout.
#
# bash 3 compatible (macOS default).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
RESULTS_FILE="${SCRIPT_DIR}/ELIDE_RESULTS.md"
BINARY="${BINARY:-${REPO_ROOT}/target/release/ascript}"
RUNS="${RUNS:-5}"
# ELIDE_FLAG: optional --elide or --no-elide to be inserted before the workload path.
# When empty (the default), the binary's default applies (currently DEFAULT-OFF).
ELIDE_FLAG="${ELIDE_FLAG:-}"

# Workloads: the ELIDE headline pair first, then supporting corpus.
BENCHES="call_heavy call_heavy_typed object_churn json_roundtrip func_pipeline"

# ── build ─────────────────────────────────────────────────────────────────────
echo "==> Building release binary..."
cd "${REPO_ROOT}"
cargo build --release --quiet
echo "    Built: ${BINARY}"
echo ""

# ── host info ─────────────────────────────────────────────────────────────────
TIMESTAMP="$(date -u '+%Y-%m-%d %H:%M UTC')"
CPU_MODEL="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || \
             lscpu 2>/dev/null | awk -F: '/Model name/{gsub(/^[ \t]+/,"",$2); print $2}' || \
             echo 'unknown')"
CPU_CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || nproc 2>/dev/null || echo '?')"
OS_INFO="$(uname -srm)"
GIT_SHA="$(git -C "${REPO_ROOT}" rev-parse --short HEAD 2>/dev/null || echo 'unknown')"

echo "Host: ${CPU_MODEL} (${CPU_CORES} logical cores) / ${OS_INFO}"
echo "Binary: ${BINARY} (${GIT_SHA})"
echo "Runs per workload: ${RUNS}"
echo ""

# ── helpers ───────────────────────────────────────────────────────────────────
median() {
    sort -n | awk '{a[NR]=$1} END {printf "%d", a[int((NR+1)/2)]}'
}

run_ms() {
    # shellcheck disable=SC2086
    "${BINARY}" run ${ELIDE_FLAG} "${SCRIPT_DIR}/profiling/$1.as" 2>/dev/null \
        | grep -oE 'elapsed_ms=[0-9.]+' | cut -d= -f2
}

peak_rss_mb() {
    local bytes
    # shellcheck disable=SC2086
    bytes=$(/usr/bin/time -l "${BINARY}" run ${ELIDE_FLAG} "${SCRIPT_DIR}/profiling/$1.as" \
        2>&1 >/dev/null \
        | grep -i "maximum resident" | grep -oE '[0-9]+' | head -1)
    awk -v b="${bytes:-0}" 'BEGIN { printf "%d", b / 1048576 }'
}

# ── run workloads ─────────────────────────────────────────────────────────────
echo "==> Running workloads (${RUNS} runs each)..."
printf "%-22s | %10s | %7s\n" "workload" "median ms" "RSS MB"
printf "%-22s-+-%10s-+-%7s\n" "----------------------" "----------" "-------"

TMPDATA="$(mktemp /tmp/ascript-elide-bench.XXXXXX)"
trap 'rm -f "${TMPDATA}"' EXIT

for bench in ${BENCHES}; do
    if [[ ! -f "${SCRIPT_DIR}/profiling/${bench}.as" ]]; then
        printf "%-22s | %10s | %7s\n" "${bench}" "SKIP" "-"
        echo "${bench} SKIP -" >> "${TMPDATA}"
        continue
    fi
    times=""
    for r in $(seq 1 "${RUNS}"); do
        t="$(run_ms "${bench}" 2>/dev/null || true)"
        if [[ -n "${t}" ]]; then
            times="${times}${t}"$'\n'
        fi
    done
    if [[ -z "${times}" ]]; then
        med="???"
        rss_mb="?"
    else
        med="$(echo "${times}" | grep -v '^$' | median)"
        rss_mb="$(peak_rss_mb "${bench}")"
    fi
    printf "%-22s | %10s | %7s\n" "${bench}" "${med}" "${rss_mb}"
    echo "${bench} ${med} ${rss_mb}" >> "${TMPDATA}"
done
echo ""

# ── append to ELIDE_RESULTS.md ────────────────────────────────────────────────
LABEL="${BENCH_LABEL:-Baseline (pre-ELIDE, same session)}"

{
    echo ""
    echo "## ${LABEL}"
    echo ""
    echo "**Date:** ${TIMESTAMP}"
    echo "**Host:** ${CPU_MODEL} (${CPU_CORES} logical cores)"
    echo "**OS:** ${OS_INFO}"
    echo "**Binary:** \`target/release/ascript\` @ \`${GIT_SHA}\`"
    echo "**Runs per workload (median):** ${RUNS}"
    echo ""
    echo "| workload | median ms | RSS MB |"
    echo "|----------|----------:|-------:|"
    while IFS=' ' read -r bench med rss_mb; do
        printf "| %-20s | %9s | %6s |\n" "${bench}" "${med}" "${rss_mb}"
    done < "${TMPDATA}"
    echo ""
    echo "*Generated by \`bench/run_elide_bench.sh\`.*"
} >> "${RESULTS_FILE}"

echo "==> Results appended to ${RESULTS_FILE}"
