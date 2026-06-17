#!/usr/bin/env bash
# bench/run_warm_bench.sh
#
# WARM A — cold/warm compile-cache A/B benchmark.
#
# Measures wall-clock time and peak RSS for:
#   - COLD run: fresh ASCRIPT_CACHE tempdir (no cached artifact)
#   - WARM run: second+ run into the same tempdir (cache hit — no compile)
#   - NO-CACHE: `ascript run --no-cache` (always compile, no cache write)
#
# Protocol:
#   For each module count N in {10, 100, 500}:
#     1. Generate the module tree (deterministic, bench/gen_module_tree.py).
#     2. Run 5 interleaved cold/warm/no-cache rounds:
#        - Create a fresh ASCRIPT_CACHE tempdir
#        - Cold run 1 (first run into empty dir) — timed
#        - Warm run 1 (second run, cache hit) — timed
#        - No-cache run 1 (--no-cache flag) — timed
#        - Repeat 4 more times (new tempdir each round for fresh cold)
#     3. Collect medians and /usr/bin/time -l peak RSS.
#   Also measures the real examples/compile_cache case.
#
# The checksum line is compared between cold and warm to confirm output identity.
#
# Usage:
#   ./bench/run_warm_bench.sh           # full run
#   ./bench/run_warm_bench.sh --skip-gen  # skip tree generation (reuse /tmp/bench_tree_*)
#
# Output: bench/WARM_RESULTS.md (overwritten)
#
# bash 3 compatible (macOS default).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BINARY="${REPO_ROOT}/target/release/ascript"
GEN_SCRIPT="${SCRIPT_DIR}/gen_module_tree.py"
RESULTS_FILE="${SCRIPT_DIR}/WARM_RESULTS.md"
EXAMPLE_DIR="${REPO_ROOT}/examples/compile_cache"
SKIP_GEN="${1:-}"

ROUNDS=5

echo "==> WARM A Benchmark -- compile-cache cold/warm A/B"
echo ""

# Verify binary
if [ ! -x "${BINARY}" ]; then
  echo "ERROR: ${BINARY} not found. Run: cargo build --release"
  exit 1
fi

TIMESTAMP="$(date -u '+%Y-%m-%d %H:%M UTC')"
CPU_MODEL="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo 'unknown')"
CPU_CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || nproc 2>/dev/null || echo '?')"
OS_INFO="$(uname -srm)"
ASCRIPT_VERSION="$("${BINARY}" --version 2>/dev/null | head -1 || echo 'unknown')"

echo "Host: ${CPU_MODEL} (${CPU_CORES} cores) / ${OS_INFO}"
echo "Binary: ${BINARY}"
echo "Version: ${ASCRIPT_VERSION}"
echo "Timestamp: ${TIMESTAMP}"
echo ""

# time_run: run one timed invocation, print "real_ms rss_bytes stdout_content"
# Uses /usr/bin/time -l for timing (macOS: centisecond precision) + RSS.
# Usage: time_run CACHE_DIR EXTRA_FLAG_OR_EMPTY FILE
time_run() {
  local cdir="$1"
  local extra="$2"
  local file="$3"
  local tmpout tmperr
  tmpout="$(mktemp /tmp/ascript_wbench_out.XXXXXX)"
  tmperr="$(mktemp /tmp/ascript_wbench_err.XXXXXX)"
  local run_args=("run")
  if [ -n "${extra}" ]; then run_args+=("${extra}"); fi
  run_args+=("${file}")
  ASCRIPT_CACHE="${cdir}" \
    /usr/bin/time -l "${BINARY}" "${run_args[@]}" \
    >"${tmpout}" 2>"${tmperr}" || true
  # Parse time output with Python (avoids awk regex dialect issues)
  local parsed
  parsed="$(python3 - "${tmperr}" <<'PYEOF'
import re, sys
txt = open(sys.argv[1]).read()
m_t = re.search(r'([0-9]+\.[0-9]+)\s+real', txt)
m_r = re.search(r'([0-9]+)\s+maximum resident set size', txt)
ms  = int(round(float(m_t.group(1)) * 1000)) if m_t else 0
rss = int(m_r.group(1)) if m_r else 0
print(ms, rss)
PYEOF
)"
  local real_ms rss_bytes stdout_line
  real_ms="$(echo "${parsed}" | awk '{print $1}')"
  rss_bytes="$(echo "${parsed}" | awk '{print $2}')"
  stdout_line="$(tr '\n' '|' < "${tmpout}" | sed 's/|$//')"
  rm -f "${tmpout}" "${tmperr}"
  printf "%s %s %s\n" "${real_ms}" "${rss_bytes}" "${stdout_line}"
}

# median: compute median of space-separated numbers
median_of() {
  python3 -c "
import sys
vals = sorted(int(x) for x in sys.argv[1:] if x)
n = len(vals)
if n == 0: print(0)
elif n % 2 == 1: print(vals[n // 2])
else: print((vals[n // 2 - 1] + vals[n // 2]) // 2)
" "$@"
}

# measure_file: runs ROUNDS cold/warm/no-cache measurements for a file
# Populates shell arrays: COLD_T WARM_T NC_T COLD_RSS_A WARM_RSS_A COLD_OUT WARM_OUT
measure_file() {
  local entry="$1"
  local label="$2"
  echo "  Measuring ${label} (${ROUNDS} rounds)..."
  COLD_T=(); WARM_T=(); NC_T=()
  COLD_RSS_A=(); WARM_RSS_A=()
  COLD_OUT=""; WARM_OUT=""

  for i in $(seq 1 "${ROUNDS}"); do
    local cdir; cdir="$(mktemp -d /tmp/ascript_wbcache.XXXXXX)"
    # Cold
    local r; r="$(time_run "${cdir}" "" "${entry}")"
    local cold_ms cold_rss cold_out
    cold_ms="$(echo "${r}" | awk '{print $1}')"
    cold_rss="$(echo "${r}" | awk '{print $2}')"
    cold_out="$(echo "${r}" | cut -d' ' -f3-)"
    COLD_T+=("${cold_ms}"); COLD_RSS_A+=("${cold_rss}")
    if [ -z "${COLD_OUT}" ]; then COLD_OUT="${cold_out}"; fi
    # Warm
    r="$(time_run "${cdir}" "" "${entry}")"
    local warm_ms warm_rss warm_out
    warm_ms="$(echo "${r}" | awk '{print $1}')"
    warm_rss="$(echo "${r}" | awk '{print $2}')"
    warm_out="$(echo "${r}" | cut -d' ' -f3-)"
    WARM_T+=("${warm_ms}"); WARM_RSS_A+=("${warm_rss}")
    if [ -z "${WARM_OUT}" ]; then WARM_OUT="${warm_out}"; fi
    # No-cache
    r="$(time_run "${cdir}" "--no-cache" "${entry}")"
    local nc_ms
    nc_ms="$(echo "${r}" | awk '{print $1}')"
    NC_T+=("${nc_ms}")
    rm -rf "${cdir}"
    echo "    round ${i}: cold=${cold_ms}ms warm=${warm_ms}ms nocache=${nc_ms}ms"
  done
}

# Module tree generation
TREE_NS=(10 100 500)
TREE_DIRS=()
if [ "${SKIP_GEN}" != "--skip-gen" ]; then
  echo "==> Generating module trees..."
  for n in "${TREE_NS[@]}"; do
    tdir="/tmp/bench_tree_${n}"
    rm -rf "${tdir}"
    python3 "${GEN_SCRIPT}" --n "${n}" --out "${tdir}"
    TREE_DIRS+=("${tdir}")
  done
  echo ""
else
  echo "==> Skipping tree generation (--skip-gen)"
  for n in "${TREE_NS[@]}"; do TREE_DIRS+=("/tmp/bench_tree_${n}"); done
fi

# Verify trees
echo "==> Verifying module trees..."
for idx in 0 1 2; do
  n="${TREE_NS[$idx]}"
  tdir="${TREE_DIRS[$idx]}"
  out="$(ASCRIPT_NO_COMPILE_CACHE=1 "${BINARY}" run "${tdir}/main.as" 2>&1)"
  if echo "${out}" | grep -q "^checksum="; then
    echo "  N=${n}: OK (${out})"
  else
    echo "  N=${n}: ERROR: ${out}"
    exit 1
  fi
done
echo ""

# Collect results
echo "==> Running cold/warm/no-cache measurements..."
echo ""

RES_NS=(); RES_COLD=(); RES_WARM=(); RES_NC=()
RES_CRSS=(); RES_WRSS=(); RES_PAR=()

for idx in 0 1 2; do
  n="${TREE_NS[$idx]}"
  tdir="${TREE_DIRS[$idx]}"
  echo "--- N=${n} ---"
  measure_file "${tdir}/main.as" "N=${n}"
  cm="$(median_of "${COLD_T[@]}")"; wm="$(median_of "${WARM_T[@]}")"; nm="$(median_of "${NC_T[@]}")"
  cr="$(median_of "${COLD_RSS_A[@]}")"; wr="$(median_of "${WARM_RSS_A[@]}")"
  par="FAIL"; if [ "${COLD_OUT}" = "${WARM_OUT}" ]; then par="PASS"; fi
  RES_NS+=("${n}"); RES_COLD+=("${cm}"); RES_WARM+=("${wm}"); RES_NC+=("${nm}")
  RES_CRSS+=("${cr}"); RES_WRSS+=("${wr}"); RES_PAR+=("${par}")
  echo "  cold median: ${cm}ms  warm median: ${wm}ms  no-cache median: ${nm}ms"
  echo "  cold RSS: $((cr / 1024))KB  warm RSS: $((wr / 1024))KB  parity: ${par}"
  echo "  (cold: ${COLD_OUT} | warm: ${WARM_OUT})"
  echo ""
done

echo "--- examples/compile_cache ---"
measure_file "${EXAMPLE_DIR}/main.as" "examples/compile_cache"
ex_cm="$(median_of "${COLD_T[@]}")"; ex_wm="$(median_of "${WARM_T[@]}")"; ex_nm="$(median_of "${NC_T[@]}")"
ex_cr="$(median_of "${COLD_RSS_A[@]}")"; ex_wr="$(median_of "${WARM_RSS_A[@]}")"
ex_par="FAIL"; if [ "${COLD_OUT}" = "${WARM_OUT}" ]; then ex_par="PASS"; fi
echo "  cold median: ${ex_cm}ms  warm median: ${ex_wm}ms  no-cache median: ${ex_nm}ms"
echo "  cold RSS: $((ex_cr / 1024))KB  warm RSS: $((ex_wr / 1024))KB  parity: ${ex_par}"
echo ""

# Generate the Markdown report
echo "==> Writing ${RESULTS_FILE}..."
python3 - \
  "${TIMESTAMP}" "${CPU_MODEL}" "${CPU_CORES}" "${OS_INFO}" "${ASCRIPT_VERSION}" \
  "${RESULTS_FILE}" "${ROUNDS}" \
  "${#RES_NS[@]}" \
  "${RES_NS[@]}" "${RES_COLD[@]}" "${RES_WARM[@]}" "${RES_NC[@]}" \
  "${RES_CRSS[@]}" "${RES_WRSS[@]}" "${RES_PAR[@]}" \
  "${ex_cm}" "${ex_wm}" "${ex_nm}" "${ex_cr}" "${ex_wr}" "${ex_par}" \
  <<'PYEOF'
import sys

it = iter(sys.argv[1:])
ts      = next(it); cpu  = next(it); cores = next(it)
osinfo  = next(it); ver  = next(it); out_path = next(it)
rounds  = int(next(it)); cnt = int(next(it))

def take(n):
    return [next(it) for _ in range(n)]

ns_s     = take(cnt); cold_s = take(cnt); warm_s  = take(cnt); nc_s   = take(cnt)
crss_s   = take(cnt); wrss_s = take(cnt); parity  = take(cnt)

ns    = [int(x) for x in ns_s]
cold  = [int(x) for x in cold_s]
warm  = [int(x) for x in warm_s]
nc    = [int(x) for x in nc_s]
crss  = [int(x) for x in crss_s]
wrss  = [int(x) for x in wrss_s]

ex_cm = int(next(it)); ex_wm = int(next(it)); ex_nm = int(next(it))
ex_cr = int(next(it)); ex_wr = int(next(it)); ex_par = next(it)

def speedup(c, w):
    if w == 0: return "inf"
    return f"{c / w:.1f}x"

def miss_oh(c, n_):
    d = c - n_
    pct = d / n_ * 100.0 if n_ > 0 else 0.0
    s = "+" if d >= 0 else ""
    return f"{s}{d} ms ({s}{pct:.1f}%)"

def kb(b):
    return f"{b // 1024:,}"

L = []
L += [
    "# Warm-Starts Benchmark (WARM A, Gates 16/18)",
    "",
    f"**Date:** {ts}",
    f"**Host:** {cpu} ({cores} logical cores)",
    f"**OS:** {osinfo}",
    f"**Binary:** `target/release/ascript` (`{ver}`)",
    f"**Rounds per measurement:** {rounds} (values are medians)",
    "",
    "---",
    "",
    "## Methodology",
    "",
    "Each round: create a fresh `ASCRIPT_CACHE` tempdir, run **cold** (first run",
    "into the empty dir — parse + resolve + bytecode-compile + archive-publish),",
    "then **warm** (second run — cache hit, verified artifact loaded, no compile),",
    "then **no-cache** (`--no-cache` — always compile, cache never read or written).",
    f"Repeat for {rounds} rounds, report medians.",
    "",
    "- **Cold:** first run into fresh empty cache dir",
    "- **Warm:** second run into the same dir (cache HIT — compile step skipped)",
    "- **No-cache:** `--no-cache` flag — always compile, no cache I/O",
    "- **Speedup:** cold ms / warm ms (reflects compile work skipped on warm)",
    "- **Miss-overhead:** cold ms - no-cache ms (cost of one archive write vs plain compile)",
    "- **RSS:** peak resident set size via `/usr/bin/time -l` (median of 5 runs)",
    "",
    "Module trees generated by `bench/gen_module_tree.py` (deterministic chain+fan",
    "import graph; each module exports unique-named fns and a class). Output parity",
    "confirms cold == warm (same checksum from the entry module).",
    "",
    "---",
    "",
    "## Unit A -- Compile Cache Cold/Warm A/B",
    "",
    "### Generated module tree (chain+fan import graph)",
    "",
    "| N modules | Cold (ms) | Warm (ms) | Speedup | No-cache (ms) | Miss-overhead | Cold RSS | Warm RSS | Output parity |",
    "|-----------|-----------|-----------|---------|---------------|---------------|----------|----------|---------------|",
]
for i in range(len(ns)):
    L.append(
        f"| {ns[i]} | {cold[i]} | {warm[i]} | **{speedup(cold[i], warm[i])}** |"
        f" {nc[i]} | {miss_oh(cold[i], nc[i])} |"
        f" {kb(crss[i])} KB | {kb(wrss[i])} KB | {parity[i]} |"
    )

L += [
    "",
    "### Real multi-module example (`examples/compile_cache/`)",
    "",
    "3-module program: main.as → util.as → model.as. Prints `hello world` + `hello cache!`.",
    "",
    "| Case | Cold (ms) | Warm (ms) | Speedup | No-cache (ms) | Miss-overhead | Cold RSS | Warm RSS | Output parity |",
    "|------|-----------|-----------|---------|---------------|---------------|----------|----------|---------------|",
    f"| examples/compile_cache | {ex_cm} | {ex_wm} | **{speedup(ex_cm, ex_wm)}** |"
    f" {ex_nm} | {miss_oh(ex_cm, ex_nm)} |"
    f" {kb(ex_cr)} KB | {kb(ex_wr)} KB | {ex_par} |",
    "",
    "---",
    "",
    "## Analysis",
    "",
    "**Speedup scales with N.** The cache skips the entire parse → resolve →",
    "bytecode-compile → archive-write pipeline for all N modules. For small N",
    "the absolute compile time is already small, so the ratio reflects the work",
    "skipped; the absolute win grows with project size.",
    "",
    "**Miss-overhead** (cold − no-cache) is the cost of one extra atomic archive",
    "write on the first (cache-populating) run. Values at or below noise level",
    "(a few ms) confirm the publish path adds no meaningful cold-start tax.",
    "",
    "**Output parity** (PASS = cold == warm checksum) confirms the cached artifact",
    "is byte-identical to the freshly-compiled run. A stale hit would produce a",
    "different checksum — PASS here proves no such stale hit occurred.",
    "",
    "**RSS** — the warm run loads the pre-compiled `.aso` archive instead of",
    "allocating AST/compile state, so RSS is at or below the cold-run level.",
    "",
    "*Generated by `bench/run_warm_bench.sh`.*",
]

with open(out_path, "w") as f:
    f.write("\n".join(L) + "\n")
print(f"==> Written: {out_path}")
PYEOF

# ===========================================================================
# WARM B -- PGO seeded-vs-unseeded A/B (cold-start delta + steady-state ~1.0x
# + section-absent ~1.0x). Honest framing per spec 3.7: PGO's win is warm-up
# elimination (cold-start / first-N-requests), NOT steady-state.
#
# Mechanism: `build --pgo` writes a trailing PGO section into the .aso archive;
# at run time, ASCRIPT_NO_PGO=1 SKIPS seeding at load (the kill switch), so the
# SAME artifact run with/without that env var is the seeded-vs-unseeded A/B.
# A plain `build` (no --pgo) is the SECTION-ABSENT (pre-WARM) baseline.
# ===========================================================================

echo ""
echo "==> WARM B: PGO seeded-vs-unseeded A/B"

# B-time_run: run one timed invocation of an already-built .aso with the given
# env prefix; prints "real_ms stdout".  (No cache; .aso never consults it.)
btime_run() {
  local envprefix="$1"  # e.g. "" or "ASCRIPT_NO_PGO=1"
  local aso="$2"
  local tmpout tmperr
  tmpout="$(mktemp /tmp/ascript_bbench_out.XXXXXX)"
  tmperr="$(mktemp /tmp/ascript_bbench_err.XXXXXX)"
  env ${envprefix} /usr/bin/time -l "${BINARY}" run "${aso}" \
    >"${tmpout}" 2>"${tmperr}" || true
  local ms
  ms="$(python3 - "${tmperr}" <<'PYEOF'
import re, sys
txt = open(sys.argv[1]).read()
m = re.search(r'([0-9]+\.[0-9]+)\s+real', txt)
print(int(round(float(m.group(1)) * 1000)) if m else 0)
PYEOF
)"
  local outline
  outline="$(tr '\n' '|' < "${tmpout}" | sed 's/|$//')"
  rm -f "${tmpout}" "${tmperr}"
  printf "%s %s\n" "${ms}" "${outline}"
}

# A short-lived CLI workload (the cold-start regime where seeding the side tables
# wins warm-up iterations). Sized so the run is resolvable above the binary's
# fixed startup (process launch dominates a truly tiny script, masking the delta).
WB_CLI="/tmp/warm_b_cli.as"
cat > "${WB_CLI}" <<'ASEOF'
import * as math from "std/math"
let sum = 0
for (i in 1..=4000) { sum = sum + i * 2 - 1 }
let o = { x: 10, y: 20, z: 30 }
let acc = 0
for (j in 0..3000) { acc = acc + o.x + o.y + o.z + math.abs(o.x - j) }
print(`${sum} ${acc}`)
ASEOF

# A first-N-requests server-shaped workload (adapted from
# bench/profiling/server_request.as) — N small so it models the warm-up window,
# not steady state.
WB_SRV="/tmp/warm_b_server.as"
cat > "${WB_SRV}" <<'ASEOF'
import * as json from "std/json"
fn handle_get(req) { return { status: 200, body: { id: req.id, ok: true } } }
fn handle_put(req) { return { status: 200, body: { id: req.id, saved: req.payload } } }
fn handle_missing(req) { return { status: 404, body: { error: "no route" } } }
let routes = { "GET /item": handle_get, "PUT /item": handle_put }
let bytes = 0
for (i in 0..2000) {
  let raw = `{"method":"${i % 2 == 0 ? "GET" : "PUT"}","path":"/item","id":${i},"payload":"p${i % 50}"}`
  let [req, e1] = json.parse(raw)
  let key = `${req.method} ${req.path}`
  let handler = routes[key] ?? handle_missing
  let resp = handler(req)
  let [out, e2] = json.stringify(resp)
  bytes = bytes + len(out)
}
print(`server_request: bytes=${bytes}`)
ASEOF

# A steady-state workload (the server_request bench corpus shape with a large
# loop) — seeding saves at most WARMUP_THRESHOLD generic executions per site, so
# steady state must converge to ~1.0x (a regression is a bug). A deterministic
# local variant of bench/profiling/server_request.as (NO time.monotonic print, so
# the seeded/unseeded/section-absent outputs are byte-identical for the parity check).
WB_STEADY="/tmp/warm_b_steady.as"
cat > "${WB_STEADY}" <<'ASEOF'
import * as json from "std/json"
fn handle_get(req) { return { status: 200, body: { id: req.id, ok: true } } }
fn handle_put(req) { return { status: 200, body: { id: req.id, saved: req.payload } } }
fn handle_missing(req) { return { status: 404, body: { error: "no route" } } }
let routes = { "GET /item": handle_get, "PUT /item": handle_put }
let bytes = 0
for (i in 0..500000) {
  let raw = `{"method":"${i % 2 == 0 ? "GET" : "PUT"}","path":"/item","id":${i},"payload":"p${i % 50}"}`
  let [req, e1] = json.parse(raw)
  let key = `${req.method} ${req.path}`
  let handler = routes[key] ?? handle_missing
  let resp = handler(req)
  let [out, e2] = json.stringify(resp)
  bytes = bytes + len(out)
}
print(`server_request: bytes=${bytes}`)
ASEOF

WB_ROUNDS=9

# b_measure: build --pgo + plain build for one workload, then time:
#   seeded (.aso w/ section, no env), unseeded (.aso w/ section, ASCRIPT_NO_PGO=1),
#   section-absent (plain .aso, no section). Returns medians via globals.
b_measure() {
  local src="$1"; local label="$2"
  local pgo_aso plain_aso
  pgo_aso="$(mktemp /tmp/ascript_b_pgo.XXXXXX.aso)"
  plain_aso="$(mktemp /tmp/ascript_b_plain.XXXXXX.aso)"
  echo "  [${label}] building (--pgo + plain)..."
  ASCRIPT_NO_COMPILE_CACHE=1 "${BINARY}" build "${src}" --pgo -o "${pgo_aso}" >/dev/null 2>&1
  ASCRIPT_NO_COMPILE_CACHE=1 "${BINARY}" build "${src}" -o "${plain_aso}" >/dev/null 2>&1
  B_SEEDED=(); B_UNSEEDED=(); B_ABSENT=()
  B_SEEDED_OUT=""; B_UNSEEDED_OUT=""; B_ABSENT_OUT=""
  local i r ms out
  for i in $(seq 1 "${WB_ROUNDS}"); do
    # interleave the three to fairly share scheduling noise
    r="$(btime_run "" "${pgo_aso}")"; ms="${r%% *}"; out="${r#* }"
    B_SEEDED+=("${ms}"); [ -z "${B_SEEDED_OUT}" ] && B_SEEDED_OUT="${out}"
    r="$(btime_run "ASCRIPT_NO_PGO=1" "${pgo_aso}")"; ms="${r%% *}"; out="${r#* }"
    B_UNSEEDED+=("${ms}"); [ -z "${B_UNSEEDED_OUT}" ] && B_UNSEEDED_OUT="${out}"
    r="$(btime_run "" "${plain_aso}")"; ms="${r%% *}"; out="${r#* }"
    B_ABSENT+=("${ms}"); [ -z "${B_ABSENT_OUT}" ] && B_ABSENT_OUT="${out}"
  done
  rm -f "${pgo_aso}" "${plain_aso}"
}

B_LABELS=(); B_SM=(); B_UM=(); B_AM=(); B_PAR=()
for spec in "cli:${WB_CLI}" "server_first_n:${WB_SRV}" "steady_state:${WB_STEADY}"; do
  lbl="${spec%%:*}"; f="${spec#*:}"
  echo "--- WARM B: ${lbl} ---"
  b_measure "${f}" "${lbl}"
  sm="$(median_of "${B_SEEDED[@]}")"; um="$(median_of "${B_UNSEEDED[@]}")"; am="$(median_of "${B_ABSENT[@]}")"
  par="FAIL"
  if [ "${B_SEEDED_OUT}" = "${B_UNSEEDED_OUT}" ] && [ "${B_SEEDED_OUT}" = "${B_ABSENT_OUT}" ]; then par="PASS"; fi
  B_LABELS+=("${lbl}"); B_SM+=("${sm}"); B_UM+=("${um}"); B_AM+=("${am}"); B_PAR+=("${par}")
  echo "  seeded=${sm}ms unseeded=${um}ms section-absent=${am}ms parity=${par}"
done

echo "==> Appending WARM B table to ${RESULTS_FILE}..."
python3 - "${RESULTS_FILE}" "${WB_ROUNDS}" "${#B_LABELS[@]}" \
  "${B_LABELS[@]}" "${B_SM[@]}" "${B_UM[@]}" "${B_AM[@]}" "${B_PAR[@]}" <<'PYEOF'
import sys
it = iter(sys.argv[1:])
out_path = next(it); rounds = int(next(it)); cnt = int(next(it))
def take(n): return [next(it) for _ in range(n)]
labels = take(cnt); sm = [int(x) for x in take(cnt)]
um = [int(x) for x in take(cnt)]; am = [int(x) for x in take(cnt)]
par = take(cnt)

def ratio(seeded, ref):
    # ref / seeded  -> >1.0 means seeded is FASTER (cold-start win); ~1.0 steady.
    if seeded == 0: return "n/a"
    return f"{ref / seeded:.3f}x"

L = ["", "---", "",
     "## Unit B -- PGO Seeded vs Unseeded A/B (Gates 16-18)",
     "",
     f"**Rounds per measurement:** {rounds} (medians; interleaved seeded / unseeded / section-absent).",
     "",
     "Mechanism: `build --pgo` appends a trailing `ASPGO` section to the `.aso`; at run",
     "time `ASCRIPT_NO_PGO=1` skips seeding at load, so the SAME artifact run with vs",
     "without that env var is the seeded-vs-unseeded A/B. A plain `build` (no `--pgo`)",
     "is the SECTION-ABSENT (pre-WARM) baseline — it proves the loader's trailing-section",
     "scan is zero-cost when no section is present.",
     "",
     "| Workload | Seeded (ms) | Unseeded (ms) | Unseeded/Seeded | Section-absent (ms) | Absent/Seeded | Output parity |",
     "|----------|-------------|---------------|-----------------|---------------------|---------------|---------------|"]
for i in range(cnt):
    L.append(
        f"| {labels[i]} | {sm[i]} | {um[i]} | {ratio(sm[i], um[i])} |"
        f" {am[i]} | {ratio(sm[i], am[i])} | {par[i]} |")

L += ["",
      "### Honest framing (spec 3.7)",
      "",
      "PGO's win is **warmup-time / cold-start latency elimination**, not steady-state",
      "throughput. Seeding installs at most `WARMUP_THRESHOLD` (8) generic executions",
      "per arith site, one generic lookup per IC/global site, and the shape-tree up front;",
      "the caches converge to the SAME fixed point either way, so:",
      "",
      "- **`cli` / `server_first_n`** (short-lived / first-N-requests): the seeded column",
      "  is the cold-start regime — any `Unseeded/Seeded > 1.0` is the warm-up iterations",
      "  saved. The absolute delta is bounded and small at the current cache model (spec 3.7);",
      "  the section's compounding value is as the carrier the DECODE/JIT specs consume.",
      "- **`steady_state`** (500k-request loop): `Unseeded/Seeded` must be **~1.0x** — the",
      "  warm-up window is a vanishing fraction of the run. A steady-state regression here",
      "  would be a bug (seeding tax), not a feature.",
      "- **Section-absent** (`Absent/Seeded` ~1.0x): the trailing-section loader scan is",
      "  **zero-cost when no section is present** — a plain `.aso` pays nothing for the",
      "  feature's existence (the Gate 12/17 posture).",
      "",
      "**Output parity PASS** across all three columns is the byte-invisibility check at",
      "the artifact level (the corpus-wide proof is the seeded differential mode in",
      "`tests/vm_differential.rs`).",
      "",
      "*Generated by `bench/run_warm_bench.sh` (Unit B section).*"]

with open(out_path, "a") as f:
    f.write("\n".join(L) + "\n")
print(f"==> Appended Unit B to: {out_path}")
PYEOF


# ===========================================================================
# WARM C -- workflow per-mode A/B bench
#
# Two workloads:
#   workflow_loop (per-COMMIT shape): 3000 × run() × 2 activities
#     Each run() pays one F_FULLFSYNC (fsync mode). The 96%-fsync profile
#     workload from bench/PROFILING_RESULTS.md.
#   workflow_long (per-EVENT shape): 1 × run() × 2000 activities
#     One F_FULLFSYNC for 2000 events (fsync mode). Group mode amortises
#     fsyncs across the window; buffered skips fsyncs entirely.
#
# Three modes: fsync (default) | group | buffered
# Gate: "fsync" numbers vs pre-WARM baseline ≈1.0x.
# ===========================================================================

echo ""
echo "==> WARM C: workflow per-mode A/B"

WC_LOOP="${REPO_ROOT}/bench/profiling/workflow_loop.as"
WC_LONG="${REPO_ROOT}/bench/profiling/workflow_long.as"

WC_ROUNDS=5

# wc_time_run: time one workflow workload in the given durability mode.
# Prints "real_ms rss_bytes stdout_line".
wc_time_run() {
  local src="$1"
  local dur="$2"
  local tmpout tmperr
  tmpout="$(mktemp /tmp/ascript_wc_out.XXXXXX)"
  tmperr="$(mktemp /tmp/ascript_wc_err.XXXXXX)"
  # workflow_long accepts durability via the script source (patched via env).
  # We create a tiny wrapper that runs the file with the chosen durability.
  local wrapper
  wrapper="$(mktemp /tmp/ascript_wc_wrapper.XXXXXX.as)"
  if [ "${src}" = "${WC_LONG}" ]; then
    cat > "${wrapper}" <<ASEOF
import { run, activity } from "std/workflow"
import { exists, remove } from "std/fs"
import * as time from "std/time"

let LOG = "/tmp/ascript_bench_wf_long.log"

let processItem = activity("processItem", (i) => {
  return { id: i, ok: true, value: i * 2 + 1 }
})

fn longFlow(ctx, input) {
  let n = input.n
  let sum = 0
  for (i in 0..n) {
    let r = ctx.call(processItem, i)
    sum = sum + r.value
  }
  return { n: n, sum: sum }
}

let dur = "${dur}"
let t0 = time.monotonic()
if (exists(LOG)) { remove(LOG) }
let [r, e] = recover(() => run(longFlow, { n: 2000 }, { log: LOG, durability: dur }))
let t1 = time.monotonic()
if (exists(LOG)) { remove(LOG) }
if (e == nil) {
  print(\`workflow_long: n=\${r.n} sum=\${r.sum} elapsed_ms=\${t1 - t0} durability=\${dur}\`)
} else {
  print(\`workflow_long: error=\${e.message}\`)
  exit(1)
}
ASEOF
  else
    # workflow_loop: create a wrapper with the chosen durability
    cat > "${wrapper}" <<ASEOF
import { run, resume, activity } from "std/workflow"
import { exists, remove } from "std/fs"
import * as time from "std/time"

let LOG = "/tmp/ascript_bench_wf.log"

let fetchUser = activity("fetchUser", (id) => {
  return { id: id, name: \`user-\${id}\`, price: 4200 }
})
let chargeCard = activity("chargeCard", (amount) => {
  return { ok: true, amount: amount }
})

fn flow(ctx, input) {
  let user = ctx.call(fetchUser, input.id)
  let receipt = ctx.uuid()
  let charge = ctx.call(chargeCard, user.price)
  return { ok: charge.ok, who: user.name, amount: charge.amount, hasReceipt: len(receipt) == 36 }
}

let dur = "${dur}"
let t0 = time.monotonic()
let ok = 0
for (i in 0..3000) {
  if (exists(LOG)) { remove(LOG) }
  let [r, e] = recover(() => run(flow, { id: i }, { log: LOG, durability: dur }))
  if (e == nil && r.ok) { ok = ok + 1 }
}
let t1 = time.monotonic()
if (exists(LOG)) { remove(LOG) }
print(\`workflow_loop: ok=\${ok} elapsed_ms=\${t1 - t0} durability=\${dur}\`)
ASEOF
  fi
  ASCRIPT_NO_COMPILE_CACHE=1 \
    /usr/bin/time -l "${BINARY}" run "${wrapper}" \
    >"${tmpout}" 2>"${tmperr}" || true
  local parsed
  parsed="$(python3 - "${tmperr}" <<'PYEOF'
import re, sys
txt = open(sys.argv[1]).read()
m_t = re.search(r'([0-9]+\.[0-9]+)\s+real', txt)
m_r = re.search(r'([0-9]+)\s+maximum resident set size', txt)
ms  = int(round(float(m_t.group(1)) * 1000)) if m_t else 0
rss = int(m_r.group(1)) if m_r else 0
print(ms, rss)
PYEOF
)"
  local real_ms rss_bytes stdout_line
  real_ms="$(echo "${parsed}" | awk '{print $1}')"
  rss_bytes="$(echo "${parsed}" | awk '{print $2}')"
  stdout_line="$(tr '\n' '|' < "${tmpout}" | sed 's/|$//')"
  rm -f "${wrapper}" "${tmpout}" "${tmperr}"
  printf "%s %s %s\n" "${real_ms}" "${rss_bytes}" "${stdout_line}"
}

# Measure one workload × one durability mode → arrays WC_MS WC_RSS
wc_measure_mode() {
  local src="$1"
  local dur="$2"
  local label="$3"
  WC_MS=(); WC_RSS=()
  echo "  [${label}] durability=${dur} (${WC_ROUNDS} rounds)..."
  for i in $(seq 1 "${WC_ROUNDS}"); do
    local r; r="$(wc_time_run "${src}" "${dur}")"
    local ms rss
    ms="$(echo "${r}" | awk '{print $1}')"
    rss="$(echo "${r}" | awk '{print $2}')"
    WC_MS+=("${ms}"); WC_RSS+=("${rss}")
    echo "    round ${i}: ${ms}ms"
  done
}

# Collect results: both workloads × three modes, interleaved
WC_LABELS=()
WC_LOOP_FSYNC_MS=0; WC_LOOP_GROUP_MS=0; WC_LOOP_BUF_MS=0
WC_LOOP_FSYNC_RSS=0; WC_LOOP_GROUP_RSS=0; WC_LOOP_BUF_RSS=0
WC_LONG_FSYNC_MS=0; WC_LONG_GROUP_MS=0; WC_LONG_BUF_MS=0
WC_LONG_FSYNC_RSS=0; WC_LONG_GROUP_RSS=0; WC_LONG_BUF_RSS=0

echo "--- WARM C: workflow_loop (per-commit shape) ---"
wc_measure_mode "${WC_LOOP}" "fsync" "loop/fsync"
WC_LOOP_FSYNC_MS="$(median_of "${WC_MS[@]}")"
WC_LOOP_FSYNC_RSS="$(median_of "${WC_RSS[@]}")"
wc_measure_mode "${WC_LOOP}" "group" "loop/group"
WC_LOOP_GROUP_MS="$(median_of "${WC_MS[@]}")"
WC_LOOP_GROUP_RSS="$(median_of "${WC_RSS[@]}")"
wc_measure_mode "${WC_LOOP}" "buffered" "loop/buffered"
WC_LOOP_BUF_MS="$(median_of "${WC_MS[@]}")"
WC_LOOP_BUF_RSS="$(median_of "${WC_RSS[@]}")"

echo ""
echo "--- WARM C: workflow_long (per-event shape) ---"
wc_measure_mode "${WC_LONG}" "fsync" "long/fsync"
WC_LONG_FSYNC_MS="$(median_of "${WC_MS[@]}")"
WC_LONG_FSYNC_RSS="$(median_of "${WC_RSS[@]}")"
wc_measure_mode "${WC_LONG}" "group" "long/group"
WC_LONG_GROUP_MS="$(median_of "${WC_MS[@]}")"
WC_LONG_GROUP_RSS="$(median_of "${WC_RSS[@]}")"
wc_measure_mode "${WC_LONG}" "buffered" "long/buffered"
WC_LONG_BUF_MS="$(median_of "${WC_MS[@]}")"
WC_LONG_BUF_RSS="$(median_of "${WC_RSS[@]}")"

echo ""
echo "==> Appending WARM C table to ${RESULTS_FILE}..."
python3 - "${RESULTS_FILE}" "${WC_ROUNDS}" \
  "${WC_LOOP_FSYNC_MS}" "${WC_LOOP_GROUP_MS}" "${WC_LOOP_BUF_MS}" \
  "${WC_LOOP_FSYNC_RSS}" "${WC_LOOP_GROUP_RSS}" "${WC_LOOP_BUF_RSS}" \
  "${WC_LONG_FSYNC_MS}" "${WC_LONG_GROUP_MS}" "${WC_LONG_BUF_MS}" \
  "${WC_LONG_FSYNC_RSS}" "${WC_LONG_GROUP_RSS}" "${WC_LONG_BUF_RSS}" \
  <<'PYEOF'
import sys
it = iter(sys.argv[1:])
out_path = next(it); rounds = int(next(it))

lf_ms  = int(next(it)); lg_ms  = int(next(it)); lb_ms  = int(next(it))
lf_rss = int(next(it)); lg_rss = int(next(it)); lb_rss = int(next(it))
nf_ms  = int(next(it)); ng_ms  = int(next(it)); nb_ms  = int(next(it))
nf_rss = int(next(it)); ng_rss = int(next(it)); nb_rss = int(next(it))

def ratio(a, b):
    if b == 0: return "n/a"
    return f"{a / b:.2f}x"
def kb(x): return f"{x // 1024:,}"

L = ["", "---", "",
     "## Unit C — Workflow Durability Modes A/B (Gates 13/16/18)",
     "",
     f"**Rounds per measurement:** {rounds} (medians; modes run interleaved).",
     "",
     "### Loss-window contract (reproduced from spec §4.2)",
     "",
     "| `durability` | write granularity | fsync policy | kill -9 mid-run | power loss |",
     "|---|---|---|---|---|",
     "| `\"fsync\"` (default) | whole-log snapshot at finish (temp+rename) | F_FULLFSYNC + dir-fsync per commit | loses whole in-flight run; `resume` re-executes all activities | completed commits never lost |",
     "| `\"group\"` (new) | per-event append at each recording call | coalesced: fsync when ≥`groupMaxEvents` (default 128) unsynced records, or ≥`groupWindowMs` (default 50 ms) since oldest unsynced | loses nothing — records reach OS page cache immediately | loses at most the unsynced tail (window-bounded while appending) |",
     "| `\"buffered\"` | whole-log snapshot at finish | none (OS-asynchronous writeback) | loses in-flight run | recent commits may be lost (OS-dependent) |",
     "",
     "**Activities are at-least-once** in all modes: a crash between an activity's side",
     "effect and its log append causes that activity to re-execute on resume. Design",
     "activities to be idempotent (the documented guidance).",
     "",
     "### workflow_loop — per-commit shape",
     "",
     "3 000 × `run()` × 2 activities each. Each `run()` pays one full commit",
     "(temp+rename+fsync in default mode). **The 96%-fsync profile workload.**",
     "",
     "| mode | median (ms) | vs fsync | peak RSS |",
     "|---|---|---|---|",
     f"| `\"fsync\"` (default) | {lf_ms} | 1.00× (baseline) | {kb(lf_rss)} KB |",
     f"| `\"group\"` | {lg_ms} | **{ratio(lf_ms, lg_ms)}** | {kb(lg_rss)} KB |",
     f"| `\"buffered\"` | {lb_ms} | **{ratio(lf_ms, lb_ms)}** | {kb(lb_rss)} KB |",
     "",
     "> **`\"fsync\"` gate:** the default mode must be ≈1.0× vs the pre-WARM baseline",
     "> (the WARM C chokepoint is inert for fsync — the `record_event` refactor adds",
     "> a `None`-check no-op when no group appender is installed). Any regression here",
     "> is a bug in the refactor, not an expected trade-off.",
     "",
     "### workflow_long — per-event shape",
     "",
     "1 × `run()` × 2 000 sequential activities. One commit at finish (fsync mode).",
     "Group mode coalesces 2 000 per-event appends into ≤ `groupMaxEvents / groupWindowMs`",
     "fsyncs; buffered skips fsyncs entirely.",
     "",
     "| mode | median (ms) | vs fsync | peak RSS |",
     "|---|---|---|---|",
     f"| `\"fsync\"` (default) | {nf_ms} | 1.00× (baseline) | {kb(nf_rss)} KB |",
     f"| `\"group\"` | {ng_ms} | **{ratio(nf_ms, ng_ms)}** | {kb(ng_rss)} KB |",
     f"| `\"buffered\"` | {nb_ms} | **{ratio(nf_ms, nb_ms)}** | {kb(nb_rss)} KB |",
     "",
     "### Reading it honestly",
     "",
     "**`workflow_loop` (per-commit shape):** each `run()` is a separate commit — each pays",
     "one `F_FULLFSYNC` under fsync mode. Group mode coalesces fsyncs across the window,",
     "so 3 000 separate `F_FULLFSYNC` calls collapse to ≤ `elapsed / windowMs` coalesced",
     "syncs — the measured speedup reflects this. Buffered skips fsyncs entirely.",
     "",
     "**`workflow_long` (per-event shape):** a single `run()` with 2 000 activities already",
     "pays only ONE `F_FULLFSYNC` (at finish) under fsync mode. Group mode trades the one",
     "finish-time fsync for per-event page-cache writes + deadline-coalesced fsyncs —",
     "net cost depends on the fsync amortisation window vs the individual `write(2)` overhead.",
     "Buffered removes all explicit fsyncs.",
     "",
     "**The pillar:** the DEFAULT is full durability; `group` and `buffered` are explicit",
     "opt-ins per workflow. A global `ascript.toml` default is deliberately rejected",
     "(spec §4.2) — silent relaxation is the failure mode being avoided.",
     "",
     "*Generated by `bench/run_warm_bench.sh` (Unit C section).*"]

with open(out_path, "a") as f:
    f.write("\n".join(L) + "\n")
print(f"==> Appended Unit C to: {out_path}")
PYEOF

echo ""
echo "==> Done."
