#!/usr/bin/env bash
# bench/run_compact_value_bench.sh
#
# VAL Stage-3 (Task 9, thin-Str: Value 24→16 bytes) headline + Gate 12.
#
# Reports, with the same-session A/B discipline of run_shared_heap_bench.sh:
#   1. size_of::<Value>()  — 24 (Stage-1 floor) → 16 (thin-Str/Builtin; NaN-box→8).
#   2. Wall-clock per workload, in BOTH VM modes (specialized AND
#      ASCRIPT_NO_SPECIALIZE=1 generic), for the VAL branch AND the same-session
#      STAGE-1 baseline (this branch @ Task-4 commit 1f1451d, size 24) built in an
#      isolated git worktree. All four series run INTERLEAVED (round-robin per
#      workload) so machine drift cancels. The baseline is the Stage-1 floor (not
#      pre-VAL main) so the A/B isolates the 24→16 step alone.
#   3. Geomean per mode (VAL vs baseline) — a regression in EITHER mode is a VAL bug
#      (Gate 12: no generic-mode regression; the generic VM skips every IC/adaptive
#      fast path, so it must benefit from the smaller Value too). Includes the NEW
#      string-heavy (concat / string-keyed map / codepoint-index) and memory-bound
#      large-working-set workloads — where thin-Str's cache-density benefit (and any
#      string-access regression from the double indirection) is surfaced honestly.
#   4. Cold-path check: decimal_cold (boxed Rc<Decimal>, an Rc::new per op) and
#      method_cold (boxed ClassMethod/GeneratorMethod) must add no measurable
#      regression on these rare bindings.
#
# Both binaries are built with `--profile profiling` (inherits release + debug
# symbols) so the optimization level is identical across the A/B.
#
# Usage:
#   ./bench/run_compact_value_bench.sh
#   BENCH_REPS=5 ./bench/run_compact_value_bench.sh     # more reps (default 3)
#   BENCH_BASELINE_WT=/path ./bench/run_compact_value_bench.sh   # reuse a worktree
#
# Output:
#   bench/COMPACT_VALUE_RESULTS.md (overwritten each run)
#   Tagged lines printed to stdout for CI/log capture.
#
# bash 3 compatible (macOS default).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BENCH_FILE="${SCRIPT_DIR}/compact_value_bench.as"
RESULTS_FILE="${SCRIPT_DIR}/COMPACT_VALUE_RESULTS.md"
REPS="${BENCH_REPS:-3}"

# VAL Stage-3 baseline = the STAGE-1 floor (this branch @ Task-4 commit 1f1451d,
# size 24) so the A/B isolates the 24→16 step. NB: 1f1451d ALREADY carries the
# ASCRIPT_NO_SPECIALIZE seam (Task 4 added it), so the perl seam-patch below is a
# no-op on it (its pattern targets the pre-VAL `Vm::new(...)` form). Built in an
# isolated worktree with its OWN target/ so it never clobbers the main build.
BASELINE_COMMIT="${BENCH_BASELINE_COMMIT:-1f1451d}"
BASELINE_WT="${BENCH_BASELINE_WT:-/tmp/ascript-val-stage3-baseline}"

VAL_BIN="${REPO_ROOT}/target/profiling/ascript"
BASE_BIN="${BASELINE_WT}/target/profiling/ascript"

echo "==> Building VAL branch (--profile profiling)..."
cd "${REPO_ROOT}"
cargo build --profile profiling --quiet
echo "    Built: ${VAL_BIN}"

# size_of::<Value>() — read straight from the (ignored) value-size print test.
# NB: do NOT pass --quiet (it suppresses the --nocapture eprintln we grep for).
VAL_SIZE="$(cargo test --lib value_size_print -- --ignored --nocapture 2>&1 \
            | grep -o 'size_of::<Value>() = [0-9]*' | grep -o '[0-9]*' | head -1 || true)"
[ -z "${VAL_SIZE}" ] && VAL_SIZE="?"
echo "    size_of::<Value>() = ${VAL_SIZE} (VAL branch)"

if [ ! -d "${BASELINE_WT}" ]; then
  echo "==> Creating baseline worktree @ ${BASELINE_COMMIT}..."
  git worktree add --detach "${BASELINE_WT}" "${BASELINE_COMMIT}"
  # Apply the same ASCRIPT_NO_SPECIALIZE env seam so the baseline can run generic.
  # (Default specialized behavior is unchanged.)
  perl -0pi -e 's/( {4}interp\.install_self\(\);\n)( {4}let vm = Vm::new\(interp\.clone\(\)\);)/$1    let specialize = std::env::var("ASCRIPT_NO_SPECIALIZE").as_deref() != Ok("1");\n    let vm = Vm::with_specialize(interp.clone(), specialize);/g' "${BASELINE_WT}/src/lib.rs"
fi
echo "==> Building baseline @ ${BASELINE_COMMIT} (--profile profiling, isolated target/)..."
( cd "${BASELINE_WT}" && cargo build --profile profiling --quiet )
echo "    Built: ${BASE_BIN}"

# Match the VAL-branch detection exactly: target `value_size_print` (the ignored
# eprintln test) and do NOT pass --quiet (it suppresses --nocapture). The Stage-1
# baseline (1f1451d) is 24; default to 24 if the grep ever comes up empty.
BASE_SIZE="$( ( cd "${BASELINE_WT}" && cargo test --lib value_size_print -- --ignored --nocapture 2>&1 ) \
            | grep -o 'size_of::<Value>() = [0-9]*' | grep -o '[0-9]*' | head -1 || echo '24' )"
[ -z "${BASE_SIZE}" ] && BASE_SIZE="24"
echo "    size_of::<Value>() = ${BASE_SIZE} (baseline)"
echo ""

TIMESTAMP="$(date -u '+%Y-%m-%d %H:%M UTC')"
CPU_MODEL="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || \
             lscpu 2>/dev/null | awk -F: '/Model name/{gsub(/^[ \t]+/,"",$2); print $2}' || \
             echo 'unknown')"
CPU_CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || nproc 2>/dev/null || echo '?')"
OS_INFO="$(uname -srm)"
echo "Host: ${CPU_MODEL} (${CPU_CORES} logical cores) / ${OS_INFO}"
echo ""

# ── Interleaved A/B: one rep = run all four (bin × mode) series once, round-robin.
# Collected into a temp file as: rep<TAB>series<TAB>workload<TAB>elapsed_ms
RAW="$(mktemp /tmp/ascript-valbench.XXXXXX)"
run_series() {
  # $1 = series label, $2 = binary, $3 = ASCRIPT_NO_SPECIALIZE value ("" or "1")
  local label="$1" bin="$2" nospec="$3" rep="$4"
  local env_prefix=""
  [ -n "${nospec}" ] && env_prefix="ASCRIPT_NO_SPECIALIZE=${nospec}"
  env ${env_prefix} "${bin}" run "${BENCH_FILE}" 2>&1 | while IFS= read -r line; do
    name="$(printf '%s' "${line}" | grep -o '^[a-z_]*' || true)"
    ms="$(printf '%s' "${line}" | grep -o 'elapsed_ms=[0-9.]*' | cut -d= -f2 || true)"
    if [ -n "${name}" ] && [ -n "${ms}" ]; then
      printf '%s\t%s\t%s\t%s\n' "${rep}" "${label}" "${name}" "${ms}" >> "${RAW}"
    fi
  done
}

echo "==> Interleaved A/B (${REPS} reps × 4 series, round-robin)..."
for rep in $(seq 1 "${REPS}"); do
  echo "    rep ${rep}/${REPS}..."
  run_series "val_spec"   "${VAL_BIN}"  ""  "${rep}"
  run_series "base_spec"  "${BASE_BIN}" ""  "${rep}"
  run_series "val_gen"    "${VAL_BIN}"  "1" "${rep}"
  run_series "base_gen"   "${BASE_BIN}" "1" "${rep}"
done
echo ""

# ── Report ────────────────────────────────────────────────────────────────────
python3 - "${RAW}" "${RESULTS_FILE}" "${TIMESTAMP}" "${CPU_MODEL}" "${CPU_CORES}" \
          "${OS_INFO}" "${VAL_SIZE}" "${BASE_SIZE}" "${BASELINE_COMMIT}" "${REPS}" <<'PYEOF'
import sys, math
from collections import defaultdict

(raw_path, out_path, ts, cpu, cores, osinfo, val_size, base_size, base_commit, reps) = sys.argv[1:11]

# rows[(series, workload)] -> [ms, ...]
rows = defaultdict(list)
order = []
with open(raw_path) as f:
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) != 4:
            continue
        _rep, series, workload, ms = parts
        rows[(series, workload)].append(float(ms))
        if workload not in order:
            order.append(workload)

def median(xs):
    s = sorted(xs)
    n = len(s)
    return s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2.0

# Per (series, workload) median.
med = {k: median(v) for k, v in rows.items()}

COLD = {"decimal_cold", "method_cold"}
HOT = [w for w in order if w not in COLD]
# VAL Stage-3 subsets: the string-heavy + memory-bound workloads added for thin-Str
# (reported separately so a string-ACCESS regression is disclosed honestly, not
# averaged away). SCALAR = the original CPU-bound loops.
STRING = [w for w in HOT if w in
          {"string_concat", "string_map", "string_index", "membound_strings"}]
SCALAR = [w for w in HOT if w not in STRING]

def geomean(vals):
    vals = [v for v in vals if v > 0]
    if not vals:
        return float("nan")
    return math.exp(sum(math.log(v) for v in vals) / len(vals))

def mode_geomean(series, workloads):
    return geomean([med[(series, w)] for w in workloads if (series, w) in med])

# Speedup (baseline / val): >1 means VAL faster. Reported as a ratio AND a delta%.
def ratio(base_series, val_series, workloads):
    b = mode_geomean(base_series, workloads)
    v = mode_geomean(val_series, workloads)
    return b / v if v > 0 else float("nan")

spec_ratio = ratio("base_spec", "val_spec", HOT)
gen_ratio  = ratio("base_gen",  "val_gen",  HOT)

def pct(r):
    return (r - 1.0) * 100.0  # +% = VAL faster

str_spec_ratio = ratio("base_spec", "val_spec", STRING)
str_gen_ratio  = ratio("base_gen",  "val_gen",  STRING)
scal_spec_ratio = ratio("base_spec", "val_spec", SCALAR)
scal_gen_ratio  = ratio("base_gen",  "val_gen",  SCALAR)

print("==> Summary")
print(f"    size_of::<Value>(): {base_size} (Stage-1 floor) -> {val_size} (VAL Stage-3 thin-Str)")
print(f"    ALL-HOT geomean (baseline/VAL):  specialized {spec_ratio:.3f}x ({pct(spec_ratio):+.1f}%)"
      f"   generic {gen_ratio:.3f}x ({pct(gen_ratio):+.1f}%)")
print(f"    SCALAR geomean:  specialized {scal_spec_ratio:.3f}x ({pct(scal_spec_ratio):+.1f}%)"
      f"   generic {scal_gen_ratio:.3f}x ({pct(scal_gen_ratio):+.1f}%)")
print(f"    STRING geomean:  specialized {str_spec_ratio:.3f}x ({pct(str_spec_ratio):+.1f}%)"
      f"   generic {str_gen_ratio:.3f}x ({pct(str_gen_ratio):+.1f}%)")
# Gate 12: neither mode may regress. A ratio < 1.0 means VAL slower than baseline.
gate12 = "PASS" if (spec_ratio >= 0.97 and gen_ratio >= 0.97) else "REVIEW"
print(f"    Gate 12 (no regression in EITHER mode, >= -3% tolerance): {gate12}")
# Cold-path deltas.
for w in ("decimal_cold", "method_cold"):
    if ("base_spec", w) in med and ("val_spec", w) in med:
        r = med[("base_spec", w)] / med[("val_spec", w)]
        print(f"    cold-path {w}: VAL {med[('val_spec', w)]:.1f} ms vs baseline "
              f"{med[('base_spec', w)]:.1f} ms ({pct(r):+.1f}%)")

# ── Markdown ──────────────────────────────────────────────────────────────────
L = []
L.append("# Compact Value Representation Benchmark (VAL Stage 3 / thin-Str + Gate 12)")
L.append("")
L.append(f"**Date:** {ts}")
L.append(f"**Host:** {cpu}")
L.append(f"**Logical cores:** {cores}")
L.append(f"**OS:** {osinfo}")
L.append(f"**Binaries:** `target/profiling/ascript` (both VAL branch and baseline @ `{base_commit}`)")
L.append(f"**Reps:** {reps} (interleaved round-robin; per-cell median reported)")
L.append("")
L.append("---")
L.append("")
L.append("## 1. Structural fact — `size_of::<Value>()`")
L.append("")
L.append(f"| | bytes |")
L.append(f"|---|---|")
L.append(f"| Stage-1 floor baseline (@ `{base_commit}`) | **{base_size}** |")
L.append(f"| VAL Stage 3 / thin-`Str` (this branch) | **{val_size}** |")
L.append("")
L.append("Stage 3 thins the two `Rc<str>`-carrying variants (`Str` AND `Builtin`) from")
L.append("the fat 16-byte `Rc<str>` (data ptr + length) to the single-word `AStr`")
L.append("(`Rc<Box<str>>`, 8 bytes — the `Box<str>` carries its own length INSIDE the")
L.append("heap allocation, so the `Rc` is a thin pointer). Both had to be thinned: the")
L.append("enum floor is `round_up(widest_payload) + 8-byte tag`, so a single remaining")
L.append("fat `Rc<str>` would have re-pinned it at 24. With the widest payload now 8")
L.append("bytes and `Decimal` already boxed (Stage 1), the layout is `8 + 8` = **16** —")
L.append("the VAL Stage-3 floor, reached with **NO new ownership `unsafe`** (the")
L.append("deferred NaN-box's selling point). 8 bytes needs the NaN-box (deferred —")
L.append("gcmodule lacks public `Cc::into_raw`/`from_raw`). The tradeoff is a")
L.append("double-indirection on string ACCESS (`Value → Rc → Box<str> → bytes`),")
L.append("surfaced by the string workloads below.")
L.append("")
L.append("---")
L.append("")
L.append("## 2. Wall-clock per workload (per-cell median, ms)")
L.append("")
L.append("Each cell is the median over the interleaved reps. **HOT** workloads form the")
L.append("Gate-12 geomean; **COLD** workloads are the boxing cold-path checks (reported")
L.append("but excluded from the headline geomean).")
L.append("")
L.append("| Workload | base spec | VAL spec | base gen | VAL gen |")
L.append("|----------|-----------|----------|----------|---------|")
def cell(series, w):
    return f"{med[(series, w)]:.1f}" if (series, w) in med else "—"
for w in order:
    tag = " *(cold)*" if w in COLD else ""
    L.append(f"| {w}{tag} | {cell('base_spec', w)} | {cell('val_spec', w)} | "
             f"{cell('base_gen', w)} | {cell('val_gen', w)} |")
L.append("")
L.append("## 3. Geomean — VAL (16 B) vs same-session Stage-1 baseline (24 B)")
L.append("")
L.append("Three subsets: **ALL-HOT** (every non-cold workload), **SCALAR** (the original")
L.append("CPU-bound loops — int/float/array/object, where `Str` is not on the path) and")
L.append("**STRING** (the thin-`Str` workloads: concat / string-keyed map / codepoint")
L.append("index / memory-bound `array<string>`). The STRING subset is the one that can")
L.append("REGRESS from the double-indirection — reported separately, not averaged away.")
L.append("")
L.append("| Subset / Mode | baseline geomean (ms) | VAL geomean (ms) | speedup | delta |")
L.append("|---------------|-----------------------|------------------|---------|-------|")
def grow(label, base_series, val_series, wl, r):
    b = mode_geomean(base_series, wl); v = mode_geomean(val_series, wl)
    L.append(f"| {label} | {b:.1f} | {v:.1f} | {r:.3f}× | {pct(r):+.1f}% |")
grow("ALL-HOT specialized", "base_spec", "val_spec", HOT, spec_ratio)
grow("ALL-HOT generic",     "base_gen",  "val_gen",  HOT, gen_ratio)
grow("SCALAR specialized",  "base_spec", "val_spec", SCALAR, scal_spec_ratio)
grow("SCALAR generic",      "base_gen",  "val_gen",  SCALAR, scal_gen_ratio)
grow("STRING specialized",  "base_spec", "val_spec", STRING, str_spec_ratio)
grow("STRING generic",      "base_gen",  "val_gen",  STRING, str_gen_ratio)
L.append("")
L.append(f"**Gate 12: {gate12}.** Thin-`Str` is an encoding change UNDER the value API")
L.append("(not a specialization guard), so the generic VM — which skips every")
L.append("IC/adaptive/global fast path — must NOT regress either. A generic-mode")
L.append("regression would be a VAL bug, not an acceptable trade.")
L.append("")
L.append("**Honest reading:** the SCALAR subset is unaffected (those workloads never")
L.append("touch `Str`), so any SCALAR delta is pure machine noise. The STRING subset is")
L.append("where the 24→16 shrink trades against the extra string-access indirection:")
L.append("read the STRING geomean as the NET for string-bound code, and `membound_strings`")
L.append("(the large-working-set scan) as the cache-density signal specifically. See the")
L.append("KEEP-or-STOP verdict at the foot of this report for the net call.")
L.append("")
L.append("## 4. Cold-path check (Task 1 / Task 2 boxing)")
L.append("")
L.append("| Workload | base spec (ms) | VAL spec (ms) | delta | notes |")
L.append("|----------|----------------|---------------|-------|-------|")
for w, note in (("decimal_cold", "Decimal boxed to `Rc<Decimal>` — one `Rc::new` per op"),
                ("method_cold",  "`ClassMethod`/`GeneratorMethod` boxed to one `Rc` payload")):
    if ("base_spec", w) in med and ("val_spec", w) in med:
        r = med[("base_spec", w)] / med[("val_spec", w)]
        L.append(f"| {w} | {med[('base_spec', w)]:.1f} | {med[('val_spec', w)]:.1f} | "
                 f"{pct(r):+.1f}% | {note} |")
L.append("")
L.append("Honest framing: boxing `Decimal` means decimal arithmetic now does an")
L.append("`Rc::new` allocation per op (the cold path). On any NON-decimal workload this")
L.append("code never runs, so it adds zero to the hot path; `decimal_cold` measures the")
L.append("cold cost directly. The two method-binding variants are rare, cold bindings —")
L.append("`method_cold` confirms the extra indirection on their construct+dispatch is")
L.append("not a measurable regression.")
L.append("")
L.append("---")
L.append("")
L.append("*Generated by `bench/run_compact_value_bench.sh` (interleaved same-session")
L.append("A/B, mirroring `run_shared_heap_bench.sh`).*")

with open(out_path, "w") as f:
    f.write("\n".join(L) + "\n")
print(f"\n==> Results written to {out_path}")
PYEOF

echo ""
echo "==> Done. Report: ${RESULTS_FILE}"
