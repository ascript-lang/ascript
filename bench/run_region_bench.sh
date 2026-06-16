#!/usr/bin/env bash
# bench/run_region_bench.sh
#
# REGION Phase-2 (spec §5.5) — the GO/NO-GO A/B against the proven-dead ObjectCell
# recycler. Same-session interleaved A/B with the run_compact_value_bench.sh
# protocol, but the A/B axis is the recycler kill switch on ONE binary (not two
# worktree builds): the recycler is purely runtime, gated by `ASCRIPT_NO_REGIONS`,
# so a single `--release --features region-spike` binary serves both series:
#
#   baseline  = `ASCRIPT_NO_REGIONS=1`  (recycler OFF — the pre-REGION allocator)
#   candidate = (env unset)             (recycler ON)
#
# For EACH workload it records, region-on vs region-off:
#   1. wall-clock (the in-program `elapsed_ms=` hot-section, median over REPS>=5),
#   2. the recycler pool counters (recycled/reused/overflow/miss) via the
#      ASCRIPT_REGION_STATS stderr dump (region-spike + env gated; byte-invisible
#      to a default build), and
#   3. peak RSS (`/usr/bin/time -l`, region-on AND region-off).
#
# Gate workloads (spec §5.5): json_roundtrip + server_request (the G1 pair),
# object_churn (G2 no-regress), region_escape (G2 worst case). Plus an examples
# corpus sweep for the G3 regions-off geomean sanity (regions-off ~ 1.00x vs the
# pre-REGION engine is structurally true — same bytes, recycler simply never fires
# — so the corpus sweep here is the candidate-vs-baseline whole-program geomean).
#
# Also captures, on the two G1 gate workloads ONLY, a macOS `sample` call-graph and
# attributes the alloc+gc/refcount CPU share via parse_sample.py — the
# allocation-attributed-CPU denominator for the G1 arithmetic (the recycler can
# only ever reduce the eligible fraction of THIS share).
#
# Usage:
#   ./bench/run_region_bench.sh
#   BENCH_REPS=7 ./bench/run_region_bench.sh
#
# Output: bench/REGION_RESULTS.md (written by the Phase-2 verdict step, NOT here —
# this script prints tagged raw lines + a machine-readable RAW dump the verdict
# step consumes). bash 3 compatible (macOS default).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PROF_DIR="${SCRIPT_DIR}/profiling"
REPS="${BENCH_REPS:-5}"
BIN="${REPO_ROOT}/target/release/ascript"
RAW="${BENCH_REGION_RAW:-/tmp/ascript-regionbench-raw.tsv}"
SAMPLE_DIR="${SCRIPT_DIR}/out"
mkdir -p "${SAMPLE_DIR}"
: > "${RAW}"

# Gate workloads (the §5.5 named set) + the G1/G2 framing.
GATE_WORKLOADS=(json_roundtrip server_request object_churn region_escape)
# Corpus sweep for the whole-program geomean (candidate vs baseline). These are the
# pre-existing run-to-completion profiling workloads.
CORPUS_WORKLOADS=(async_inline async_concurrent func_pipeline call_heavy workflow_loop)

echo "==> Building region-spike release binary..."
cd "${REPO_ROOT}"
cargo build --release --features region-spike --quiet
echo "    Built: ${BIN}"
echo ""

# Sanity: prove the recycler is wired (region ON activates, OFF deactivates).
echo "==> Recycler wiring sanity (a fn-scoped churn loop):"
SANITY=$(mktemp /tmp/ascript-region-sanity.XXXXXX).as
cat > "${SANITY}" <<'EOF'
fn churn() { let t = 0; for (i in 0..1000) { let o = { a: i, b: i + 1 }; t = t + o.a + o.b }; return t }
print(churn())
EOF
echo "    region ON : $(ASCRIPT_REGION_STATS=1 "${BIN}" run "${SANITY}" 2>&1 | grep REGION_STATS)"
echo "    region OFF: $(ASCRIPT_REGION_STATS=1 ASCRIPT_NO_REGIONS=1 "${BIN}" run "${SANITY}" 2>&1 | grep REGION_STATS)"
rm -f "${SANITY}"
echo ""

TIMESTAMP="$(date -u '+%Y-%m-%d %H:%M UTC')"
COMMIT="$(git -C "${REPO_ROOT}" rev-parse HEAD)"
CPU_MODEL="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
CPU_CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || echo '?')"
OS_INFO="$(uname -srm)"
echo "Host: ${CPU_MODEL} (${CPU_CORES} cores) / ${OS_INFO}"
echo "Commit: ${COMMIT}"
echo ""

# run_one <series_label> <ASCRIPT_NO_REGIONS value ("" or "1")> <workload> <rep>
# Emits to RAW: rep<TAB>series<TAB>workload<TAB>elapsed_ms<TAB>recycled<TAB>reused<TAB>overflow<TAB>miss
run_one() {
  local series="$1" noreg="$2" wl="$3" rep="$4"
  local f="${PROF_DIR}/${wl}.as"
  local env_prefix="ASCRIPT_REGION_STATS=1"
  [ -n "${noreg}" ] && env_prefix="${env_prefix} ASCRIPT_NO_REGIONS=${noreg}"
  local combined ms stats recycled reused overflow miss
  combined="$(env ${env_prefix} "${BIN}" run "${f}" 2>&1)"
  ms="$(printf '%s' "${combined}" | grep -oE 'elapsed_ms=[0-9.]+' | head -1 | cut -d= -f2)"
  stats="$(printf '%s' "${combined}" | grep 'REGION_STATS' | head -1)"
  recycled="$(printf '%s' "${stats}" | grep -oE 'recycled=[0-9]+' | cut -d= -f2)"
  reused="$(printf '%s' "${stats}" | grep -oE 'reused=[0-9]+' | cut -d= -f2)"
  overflow="$(printf '%s' "${stats}" | grep -oE 'overflow=[0-9]+' | cut -d= -f2)"
  miss="$(printf '%s' "${stats}" | grep -oE 'miss=[0-9]+' | cut -d= -f2)"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${rep}" "${series}" "${wl}" "${ms:-0}" "${recycled:-0}" "${reused:-0}" "${overflow:-0}" "${miss:-0}" >> "${RAW}"
}

ALL_WORKLOADS=("${GATE_WORKLOADS[@]}" "${CORPUS_WORKLOADS[@]}")

echo "==> Interleaved A/B (${REPS} reps, round-robin per workload)..."
for rep in $(seq 1 "${REPS}"); do
  echo "    rep ${rep}/${REPS}..."
  for wl in "${ALL_WORKLOADS[@]}"; do
    run_one "region_on"  ""  "${wl}" "${rep}"
    run_one "region_off" "1" "${wl}" "${rep}"
  done
done
echo ""

# ── Peak RSS, one measured pair per workload (region on/off) ──────────────────
echo "==> Peak RSS (/usr/bin/time -l), region on vs off..."
: > "${RAW}.rss"
for wl in "${ALL_WORKLOADS[@]}"; do
  f="${PROF_DIR}/${wl}.as"
  rss_on=$(/usr/bin/time -l "${BIN}" run "${f}" 2>&1 >/dev/null | grep -i 'maximum resident' | grep -oE '^ *[0-9]+' | tr -d ' ')
  rss_off=$(ASCRIPT_NO_REGIONS=1 /usr/bin/time -l "${BIN}" run "${f}" 2>&1 >/dev/null | grep -i 'maximum resident' | grep -oE '^ *[0-9]+' | tr -d ' ')
  printf '%s\t%s\t%s\n' "${wl}" "${rss_on:-0}" "${rss_off:-0}" >> "${RAW}.rss"
  printf '    %-18s on=%6d MB  off=%6d MB\n' "${wl}" "$(( ${rss_on:-0} / 1048576 ))" "$(( ${rss_off:-0} / 1048576 ))"
done
echo ""

# ── Alloc-CPU attribution on the two G1 gate workloads (sample call-graph) ────
# The G1 denominator: the recycler can ONLY reduce the eligible fraction of the
# alloc+gc/refcount CPU share. With recycled=0 the reduction is 0 regardless, but
# we capture the share so the arithmetic is shown with a real denominator.
echo "==> Alloc-CPU attribution (macOS sample) on G1 gate workloads..."
: > "${RAW}.cpu"
if command -v sample >/dev/null 2>&1; then
  for wl in json_roundtrip server_request; do
    f="${PROF_DIR}/${wl}.as"
    for series in on off; do
      noreg=""; [ "${series}" = off ] && noreg="1"
      env ${noreg:+ASCRIPT_NO_REGIONS=1} "${BIN}" run "${f}" >/dev/null 2>&1 &
      pid=$!
      sleep 0.3
      sample "${pid}" 60 1 -file "${SAMPLE_DIR}/${wl}.region_${series}.sample.txt" -mayDie >/dev/null 2>&1 || true
      wait "${pid}" 2>/dev/null || true
      echo "    --- ${wl} (region ${series}) ---"
      python3 "${PROF_DIR}/parse_sample.py" "${wl}_${series}" "${SAMPLE_DIR}/${wl}.region_${series}.sample.txt" \
        | grep -E 'alloc|gc/refcount|samples' | tee -a "${RAW}.cpu" || true
    done
  done
else
  echo "    (sample not available — skipping CPU attribution)"
fi
echo ""
echo "==> RAW: ${RAW}  (+ .rss, .cpu)"
echo "==> Run the verdict step to write bench/REGION_RESULTS.md."
