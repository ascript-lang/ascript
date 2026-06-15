#!/usr/bin/env bash
# bench/run_nanb_bench.sh
#
# NANB Phase 3, Task 3.3 — same-session A/B + RSS report (Gates 16/18) for the
# 24-byte default repr vs the 16-byte `--features value16` repr.
#
# THREE HARD RULES (NANB Phase 3): NO worktree, NO second target/ — the two
# reprs are two RELEASE binaries built by FEATURE-TOGGLE into the ONE existing
# target/ (a feature flip recompiles in place). This script does NOT build; pass
# the two pre-built binaries:
#
#   bench/run_nanb_bench.sh <base-24byte-bin> <value16-16byte-bin> [reps=7]
#
# Build them first (one target/, no worktree):
#   cargo build --release            && cp target/release/ascript /tmp/ascript-base
#   cargo build --release --features value16 && cp target/release/ascript /tmp/ascript-v16
#
# Workloads = bench/profiling/* (json_roundtrip, object_churn, async_inline,
# async_concurrent, workflow_loop, func_pipeline, call_heavy, server_request) —
# the LANE/CALL profiling corpus, each emitting `elapsed_ms=`.
#
# Columns per workload: base-spec / v16-spec / base-gen / v16-gen / base-tw /
# v16-tw (Gate 12/17 wants no regression in the GENERIC mode either — it skips
# every IC/adaptive fast path, so it must benefit from the smaller Value too).
# Interleaved round-robin per rep so machine drift cancels.
#
# Memory (Gate 18): /usr/bin/time -l peak RSS per workload per binary (spec mode).
# The 24->16 byte shrink should reduce RSS on Value-heavy workloads — THE memory
# case for the 16-byte repr.
#
# Output: bench/NANB_RESULTS.md gets a Phase-3 section APPENDED (not overwritten).
# bash 3 compatible (macOS default).

set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

BASE_BIN="${1:?usage: run_nanb_bench.sh <base-bin> <value16-bin> [reps]}"
V16_BIN="${2:?usage: run_nanb_bench.sh <base-bin> <value16-bin> [reps]}"
REPS="${3:-7}"
RESULTS_FILE="${SCRIPT_DIR}/NANB_RESULTS.md"

BENCHES="async_inline async_concurrent json_roundtrip object_churn workflow_loop func_pipeline call_heavy server_request"

TIMESTAMP="$(date -u '+%Y-%m-%d %H:%M UTC')"
CPU_MODEL="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)"
CPU_CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || nproc 2>/dev/null || echo '?')"
OS_INFO="$(uname -srm)"
COMMIT="$(git rev-parse --short HEAD)"

echo "Host: ${CPU_MODEL} (${CPU_CORES} cores) / ${OS_INFO}  commit ${COMMIT}"
echo "reps=${REPS}  base=${BASE_BIN}  v16=${V16_BIN}"
echo ""

RAW="$(mktemp /tmp/ascript-nanb.XXXXXX)"

run_one() {  # $1 label  $2 bin  $3 NO_SPECIALIZE("" |1)  $4 TREE_WALKER("" |1)  $5 workload  $6 rep
  local label="$1" bin="$2" nospec="$3" tw="$4" f="$5" rep="$6" pre="" ms
  [ -n "${nospec}" ] && pre="ASCRIPT_NO_SPECIALIZE=1"
  local args=""
  [ -n "${tw}" ] && args="--tree-walker"
  ms="$(env ${pre} "${bin}" run ${args} "bench/profiling/${f}.as" 2>/dev/null \
        | grep -oE 'elapsed_ms=[0-9.]+' | cut -d= -f2)"
  [ -n "${ms}" ] && printf '%s\t%s\t%s\t%s\n' "${rep}" "${label}" "${f}" "${ms}" >> "${RAW}"
}

echo "==> Interleaved A/B (${REPS} reps × 6 series)..."
for rep in $(seq 1 "${REPS}"); do
  echo "    rep ${rep}/${REPS}"
  for f in ${BENCHES}; do
    run_one base_spec "${BASE_BIN}" ""  ""  "${f}" "${rep}"
    run_one v16_spec  "${V16_BIN}"  ""  ""  "${f}" "${rep}"
    run_one base_gen  "${BASE_BIN}" "1" ""  "${f}" "${rep}"
    run_one v16_gen   "${V16_BIN}"  "1" ""  "${f}" "${rep}"
    run_one base_tw   "${BASE_BIN}" ""  "1" "${f}" "${rep}"
    run_one v16_tw    "${V16_BIN}"  ""  "1" "${f}" "${rep}"
  done
done
echo ""

# ── Peak RSS (Gate 18), spec mode, single measured run per (bin, workload). ──
RSS_RAW="$(mktemp /tmp/ascript-nanb-rss.XXXXXX)"
peak_rss_kb() {  # $1 bin $2 workload -> peak RSS in KiB
  /usr/bin/time -l "$1" run "bench/profiling/$2.as" 2>&1 >/dev/null \
    | grep -i 'maximum resident' | grep -oE '[0-9]+' | head -1
}
echo "==> Peak RSS per workload (spec mode)..."
for f in ${BENCHES}; do
  br="$(peak_rss_kb "${BASE_BIN}" "${f}")"
  vr="$(peak_rss_kb "${V16_BIN}"  "${f}")"
  printf '%s\t%s\t%s\n' "${f}" "${br}" "${vr}" >> "${RSS_RAW}"
done
echo ""

python3 - "${RAW}" "${RSS_RAW}" "${RESULTS_FILE}" "${TIMESTAMP}" "${CPU_MODEL}" \
          "${CPU_CORES}" "${OS_INFO}" "${COMMIT}" "${REPS}" <<'PYEOF'
import sys, math
from collections import defaultdict
(raw, rss_raw, out, ts, cpu, cores, osinfo, commit, reps) = sys.argv[1:10]

rows = defaultdict(list); order = []
with open(raw) as f:
    for line in f:
        p = line.rstrip("\n").split("\t")
        if len(p) != 4: continue
        _rep, series, wl, ms = p
        rows[(series, wl)].append(float(ms))
        if wl not in order: order.append(wl)

def median(xs):
    s = sorted(xs); n = len(s)
    return s[n//2] if n % 2 else (s[n//2-1]+s[n//2])/2.0
med = {k: median(v) for k, v in rows.items()}

def speedup(base, val, wl):  # base/val : >1 means value16 faster
    b, v = med.get((base, wl)), med.get((val, wl))
    return (b / v) if (b and v) else float("nan")

def geomean(xs):
    xs = [x for x in xs if x and x > 0]
    return math.exp(sum(math.log(x) for x in xs)/len(xs)) if xs else float("nan")

# RSS
rss = {}
with open(rss_raw) as f:
    for line in f:
        p = line.rstrip("\n").split("\t")
        if len(p) != 3: continue
        wl, br, vr = p
        rss[wl] = (int(br), int(vr))

L = []
L.append("")
L.append("---")
L.append("")
L.append("## Phase 3 — the evidence: cross-repr differential, deep fuzz, same-session A/B")
L.append("")
L.append(f"**Date:** {ts} | **Machine:** {cpu} ({cores} cores) / {osinfo} | "
         f"**Commit:** {commit} | **Reps:** {reps} (interleaved, per-cell median)")
L.append("")
L.append("Same-session A/B: the 24-byte default repr vs the 16-byte `--features value16` "
         "repr, two RELEASE binaries built by feature-toggle into ONE `target/` (no "
         "worktree). `size_of::<Value>()` = **24** (default) vs **16** (value16), asserted "
         "by each binary's own `value_size` test. Speedup = base_ms / v16_ms (>1.000 means "
         "value16 is FASTER).")
L.append("")
L.append("### Time A/B — per workload, per VM mode")
L.append("")
L.append("| workload | base-spec ms | v16-spec ms | spec× | base-gen ms | v16-gen ms | gen× | base-tw ms | v16-tw ms | tw× |")
L.append("|---|---|---|---|---|---|---|---|---|---|")
for wl in order:
    bs, vs = med.get(("base_spec", wl)), med.get(("v16_spec", wl))
    bg, vg = med.get(("base_gen", wl)),  med.get(("v16_gen", wl))
    bt, vt = med.get(("base_tw", wl)),   med.get(("v16_tw", wl))
    L.append(f"| {wl} | {bs:.0f} | {vs:.0f} | {speedup('base_spec','v16_spec',wl):.3f}× | "
             f"{bg:.0f} | {vg:.0f} | {speedup('base_gen','v16_gen',wl):.3f}× | "
             f"{bt:.0f} | {vt:.0f} | {speedup('base_tw','v16_tw',wl):.3f}× |")

spec_g = geomean([speedup("base_spec","v16_spec",w) for w in order])
gen_g  = geomean([speedup("base_gen","v16_gen",w)   for w in order])
tw_g   = geomean([speedup("base_tw","v16_tw",w)     for w in order])
L.append("")
L.append(f"**Geomean speedup (value16 / default):**  spec **{spec_g:.3f}×**  ·  "
         f"gen **{gen_g:.3f}×**  ·  tree-walker **{tw_g:.3f}×**")
L.append("")
L.append("### Peak RSS A/B (Gate 18) — spec mode")
L.append("")
L.append("| workload | base RSS (MB) | v16 RSS (MB) | Δ (v16-base) | v16/base |")
L.append("|---|---|---|---|---|")
rss_ratios = []
for wl in order:
    if wl not in rss: continue
    br, vr = rss[wl]
    brm, vrm = br/1048576.0, vr/1048576.0
    ratio = vr/br if br else float("nan")
    rss_ratios.append(ratio)
    L.append(f"| {wl} | {brm:.1f} | {vrm:.1f} | {vrm-brm:+.1f} | {ratio:.3f} |")
rss_g = geomean(rss_ratios)
L.append("")
L.append(f"**Geomean RSS ratio (value16 / default) = {rss_g:.3f}×**  "
         f"(< 1.000 means value16 uses LESS memory — the 24→16 byte case).")
L.append("")

with open(out, "a") as f:
    f.write("\n".join(L) + "\n")

print(f"spec geomean = {spec_g:.3f}x | gen = {gen_g:.3f}x | tw = {tw_g:.3f}x")
print(f"RSS geomean ratio v16/base = {rss_g:.3f}x")
PYEOF

echo ""
echo "==> appended to ${RESULTS_FILE}"
rm -f "${RAW}" "${RSS_RAW}"
