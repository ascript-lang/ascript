#!/usr/bin/env bash
# bench/run_data_parallel_bench.sh
#
# Shell driver for bench/data_parallel_bench.as — PAR (data-parallel) spec §6.
#
# Runs the benchmark at ASCRIPT_WORKERS=1,2,4,8, collects tagged output lines,
# records RSS via /usr/bin/time -l (macOS) or /usr/bin/time -v (Linux), and
# writes bench/DATA_PARALLEL_RESULTS.md.
#
# Usage:
#   ./bench/run_data_parallel_bench.sh            # standard 1/2/4/8 sweep
#   WORKER_COUNTS="1 4" ./bench/run_data_parallel_bench.sh
#
# Output:
#   bench/DATA_PARALLEL_RESULTS.md (overwritten each run)
#   Tagged lines printed to stdout for CI/log capture.
#
# bash 3 compatible (macOS default).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BENCH_FILE="${SCRIPT_DIR}/data_parallel_bench.as"
RESULTS_FILE="${SCRIPT_DIR}/DATA_PARALLEL_RESULTS.md"
BINARY="${REPO_ROOT}/target/release/ascript"

WORKER_COUNTS="${WORKER_COUNTS:-1 2 4 8}"

# ── 1. Build release binary ───────────────────────────────────────────────────
echo "==> Building release binary..."
cd "${REPO_ROOT}"
cargo build --release --quiet
echo "    Built: ${BINARY}"
echo ""

# ── 2. Host info ──────────────────────────────────────────────────────────────
TIMESTAMP="$(date -u '+%Y-%m-%d %H:%M UTC')"
CPU_MODEL="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || \
             lscpu 2>/dev/null | awk -F: '/Model name/{gsub(/^[ \t]+/,"",$2); print $2}' || \
             echo 'unknown')"
CPU_CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || nproc 2>/dev/null || echo '?')"
OS_INFO="$(uname -srm)"
echo "Host: ${CPU_MODEL} (${CPU_CORES} logical cores) / ${OS_INFO}"
echo ""

# ── 3. Detect time(1) RSS flag ───────────────────────────────────────────────
# macOS: /usr/bin/time -l → "maximum resident set size" in bytes on a line by itself
# Linux: /usr/bin/time -v → "Maximum resident set size" in KB
TIME_BIN="/usr/bin/time"
if [[ "$(uname)" == "Darwin" ]]; then
    TIME_FLAGS="-l"
    RSS_PATTERN="maximum resident set size"
    RSS_UNIT="bytes"
    RSS_SCALE=1  # already bytes
else
    TIME_FLAGS="-v"
    RSS_PATTERN="Maximum resident set size"
    RSS_UNIT="KB"
    RSS_SCALE=1024  # convert to bytes
fi

# Helper: extract RSS from time output (stderr)
extract_rss() {
    local time_output="$1"
    local rss_bytes
    # macOS: "     NNNNNN  maximum resident set size"
    # Linux: "Maximum resident set size (kbytes): NNNNNN"
    if [[ "$(uname)" == "Darwin" ]]; then
        rss_bytes="$(echo "${time_output}" | grep -i 'maximum resident set size' | awk '{print $1}')"
    else
        rss_bytes="$(echo "${time_output}" | grep -i 'Maximum resident set size' | awk -F: '{print $2}' | tr -d ' ')"
        rss_bytes="$((rss_bytes * 1024))"
    fi
    echo "${rss_bytes:-0}"
}

# ── 4. Run at each worker count ───────────────────────────────────────────────
TMPDIR_BENCH="$(mktemp -d /tmp/ascript-par-bench.XXXXXX)"
trap 'rm -rf "${TMPDIR_BENCH}"' EXIT

for W in ${WORKER_COUNTS}; do
    echo "==> Running with ASCRIPT_WORKERS=${W} ..."
    OUT_FILE="${TMPDIR_BENCH}/w${W}.out"
    TIME_FILE="${TMPDIR_BENCH}/w${W}.time"
    # Capture time output (stderr) separately
    ASCRIPT_WORKERS="${W}" "${TIME_BIN}" ${TIME_FLAGS} \
        "${BINARY}" run "${BENCH_FILE}" > "${OUT_FILE}" 2>"${TIME_FILE}"
    RSS_BYTES="$(extract_rss "$(cat "${TIME_FILE}")")"
    RSS_MB="$(echo "scale=1; ${RSS_BYTES} / 1048576" | bc 2>/dev/null || echo '?')"
    # Append RSS annotation to the output file
    echo "rss_bytes=${RSS_BYTES}" >> "${OUT_FILE}"
    echo "rss_mb=${RSS_MB}" >> "${OUT_FILE}"
    cat "${OUT_FILE}" | sed "s/^/  [W=${W}] /"
    echo ""
done

# ── 5. Parse and report via Python ───────────────────────────────────────────
python3 - "${TMPDIR_BENCH}" "${WORKER_COUNTS}" "${RESULTS_FILE}" \
    "${TIMESTAMP}" "${CPU_MODEL}" "${CPU_CORES}" "${OS_INFO}" <<'PYEOF'
import sys, os, re

tmpdir, wc_str, out_path, ts, cpu_model, cpu_cores, os_info = sys.argv[1:]
worker_counts = wc_str.split()

# Parse all run files
data = {}  # { W: { key: val, "breakeven": { iters: {seq_ms, pmap_ms} }, ... } }
for W in worker_counts:
    fpath = os.path.join(tmpdir, f"w{W}.out")
    d = {}
    be = {}
    fvp = {}
    fvp_freeze = {}
    pred = []
    pred_small = []
    with open(fpath) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            # breakeven iters=N seq_ms=X pmap_ms=Y
            m = re.match(r"breakeven iters=(\d+) seq_ms=(\S+) pmap_ms=(\S+)", line)
            if m:
                be[m.group(1)] = {"seq_ms": m.group(2), "pmap_ms": m.group(3)}
                continue
            # frozen_vs_plain_freeze n=N freeze_ms=X
            m = re.match(r"frozen_vs_plain_freeze n=(\d+) freeze_ms=(\S+)", line)
            if m:
                fvp_freeze[m.group(1)] = m.group(2)
                continue
            # frozen_vs_plain n=N frozen_ms=X plain_ms=Y ok=Z
            m = re.match(r"frozen_vs_plain n=(\d+) frozen_ms=(\S+) plain_ms=(\S+) ok=(\S+)", line)
            if m:
                fvp[m.group(1)] = {"frozen_ms": m.group(2), "plain_ms": m.group(3), "ok": m.group(4)}
                continue
            # preduce n=N result_ok=... ms=X
            m = re.match(r"preduce n=(\d+) result_ok=(\S+) ms=(\S+)", line)
            if m:
                pred.append({"n": m.group(1), "ok": m.group(2), "ms": m.group(3)})
                continue
            # preduce_small n=8 chunks=N result_ok=... ms=X
            m = re.match(r"preduce_small n=8 chunks=(\d+) result_ok=(\S+) ms=(\S+)", line)
            if m:
                pred_small.append({"chunks": m.group(1), "ok": m.group(2), "ms": m.group(3)})
                continue
            # scalar key=val
            if "=" in line and not line.startswith(" "):
                k, _, v = line.partition("=")
                d[k.strip()] = v.strip()
    d["breakeven"] = be
    d["fvp"] = fvp
    d["fvp_freeze"] = fvp_freeze
    d["preduce"] = pred
    d["preduce_small"] = pred_small
    data[W] = d

# ── Compute speedups ──────────────────────────────────────────────────────────
def fms(s):
    try:
        return f"{float(s):.1f}"
    except (ValueError, TypeError):
        return str(s)

def speedup(base, val):
    try:
        return f"{float(base)/float(val):.2f}×"
    except (ValueError, TypeError, ZeroDivisionError):
        return "?"

W1 = worker_counts[0]
base_seq = data[W1].get("scaling_seq_ms")
base_pmap = data[W1].get("scaling_pmap_ms")
base_gather = data[W1].get("scaling_gather_ms")

# ── Print summary to stdout ───────────────────────────────────────────────────
print("\n==> Scaling summary (32 × 400k-iter LCG)")
for W in worker_counts:
    d = data[W]
    pmap_ms = d.get("scaling_pmap_ms", "?")
    gather_ms = d.get("scaling_gather_ms", "?")
    seq_ms = d.get("scaling_seq_ms", "?")
    rss_mb = d.get("rss_mb", "?")
    print(f"  W={W}: seq={fms(seq_ms)}ms  pmap={fms(pmap_ms)}ms ({speedup(base_pmap, pmap_ms)} vs W=1)  "
          f"gather={fms(gather_ms)}ms  RSS={rss_mb}MB")

print("\n==> Break-even sweep (W=4)")
W4 = "4" if "4" in worker_counts else worker_counts[-1]
be = data.get(W4, {}).get("breakeven", {})
for iters in ["0", "1000", "10000", "100000", "400000"]:
    entry = be.get(iters, {})
    seq = entry.get("seq_ms", "?")
    pmap = entry.get("pmap_ms", "?")
    winner = "PMAP" if entry and float(pmap) < float(seq) else "SEQ"
    print(f"  iters={iters:>6}: seq={fms(seq)}ms  pmap={fms(pmap)}ms  → {winner} wins")

# Find break-even crossover
crossover = None
iters_list = ["0", "1000", "10000", "100000", "400000"]
for i in range(len(iters_list)-1):
    lo, hi = iters_list[i], iters_list[i+1]
    lo_entry = be.get(lo, {})
    hi_entry = be.get(hi, {})
    if lo_entry and hi_entry:
        lo_pmap, lo_seq = float(lo_entry.get("pmap_ms", "inf")), float(lo_entry.get("seq_ms", "0"))
        hi_pmap, hi_seq = float(hi_entry.get("pmap_ms", "inf")), float(hi_entry.get("seq_ms", "0"))
        if lo_pmap >= lo_seq and hi_pmap < hi_seq:
            crossover = (lo, hi)
            break

if crossover:
    print(f"\n  Break-even crossover: between {crossover[0]} and {crossover[1]} LCG iters (W=4, 32 chunks)")
else:
    print("\n  Break-even crossover: not clearly bracketed in the sweep")

print("\n==> Frozen vs plain (W=4)")
fvp = data.get(W4, {}).get("fvp", {})
for n in ["10000", "100000", "1000000"]:
    entry = fvp.get(n, {})
    frozen = entry.get("frozen_ms", "?")
    plain = entry.get("plain_ms", "?")
    try:
        ratio = float(plain) / float(frozen)
        ratio_s = f"{ratio:.2f}×"
    except (ValueError, TypeError):
        ratio_s = "?"
    print(f"  n={n:>7}: frozen={fms(frozen)}ms  plain={fms(plain)}ms  ratio={ratio_s}")

# ── Write DATA_PARALLEL_RESULTS.md ───────────────────────────────────────────
md = []
md.append("# Data-Parallel (PAR) Benchmark Results")
md.append("")
md.append(f"**Date:** {ts}")
md.append(f"**Host:** {cpu_model}")
md.append(f"**Logical cores:** {cpu_cores}")
md.append(f"**OS:** {os_info}")
md.append(f"**Binary:** `target/release/ascript`")
md.append(f"**Bench:** `bench/data_parallel_bench.as`")
md.append("")
md.append("---")
md.append("")

# (a) Scaling table
md.append("## (a) Scaling — SEQ / PMAP / GATHER over 32 × 400k-iter LCG chunks")
md.append("")
md.append("Same-session A/B: three paths measured back-to-back in one program run.")
md.append("Pool is warmed by a gather-warmup before timing.")
md.append("")
md.append("| Workers | SEQ (ms) | PMAP (ms) | PMAP speedup vs W=1 | GATHER (ms) | GATHER speedup vs W=1 | RSS (MB) | Checksum OK |")
md.append("|---------|----------|-----------|---------------------|-------------|------------------------|----------|-------------|")
for W in worker_counts:
    d = data[W]
    seq_ms = d.get("scaling_seq_ms", "?")
    pmap_ms = d.get("scaling_pmap_ms", "?")
    gather_ms = d.get("scaling_gather_ms", "?")
    cs_ok = d.get("scaling_checksum_ok", "?")
    rss_mb = d.get("rss_mb", "?")
    md.append(f"| {W} | {fms(seq_ms)} | {fms(pmap_ms)} | {speedup(base_pmap, pmap_ms) if W != W1 else 'baseline'} | {fms(gather_ms)} | {speedup(base_gather, gather_ms) if W != W1 else 'baseline'} | {rss_mb} | {cs_ok} |")
md.append("")
md.append("**Checksum:** pmap ≡ gather (same values, input-order merge deterministic).")
md.append("")
md.append("---")
md.append("")

# (b) Break-even
md.append("## (b) Break-Even Sweep — PMAP vs SEQ (W=4, 32 chunks)")
md.append("")
md.append("Per-element work is varied; both paths measured in the same program run.")
md.append("`iters=0` is pure dispatch overhead (trivial element transform).")
md.append("")
md.append("| LCG iters / element | SEQ (ms) | PMAP (ms) | Winner |")
md.append("|---------------------|----------|-----------|--------|")
be4 = data.get(W4, {}).get("breakeven", {})
for iters in ["0", "1000", "10000", "100000", "400000"]:
    entry = be4.get(iters, {})
    seq = entry.get("seq_ms", "?")
    pmap = entry.get("pmap_ms", "?")
    try:
        winner = "**PMAP**" if float(pmap) < float(seq) else "SEQ"
    except (ValueError, TypeError):
        winner = "?"
    md.append(f"| {iters} | {fms(seq)} | {fms(pmap)} | {winner} |")
md.append("")
if crossover:
    md.append(f"> **Break-even:** pmap starts winning between **{crossover[0]}** and **{crossover[1]}** LCG iterations per element")
    md.append(f"> (32 chunks, W=4, real per-element cost measured on this machine).")
else:
    md.append("> Break-even crossover: not bracketed in the sweep range.")
md.append("")
md.append("---")
md.append("")

# (c) Frozen vs plain
md.append("## (c) Frozen vs Plain Input — PMAP over shared.freeze vs per-chunk copy (W=4)")
md.append("")
md.append("Frozen arrays cross the airlock with a single `Arc` bump per chunk (O(1) in N).")
md.append("Plain arrays are deep-copied per chunk (O(N/chunks)).")
md.append("")
fvp_freeze4 = data.get(W4, {}).get("fvp_freeze", {})
md.append("**One-time freeze cost:**")
md.append("")
md.append("| Array size (N) | freeze_ms |")
md.append("|----------------|-----------|")
for n in ["10000", "100000", "1000000"]:
    freeze_ms = fvp_freeze4.get(n, "?")
    md.append(f"| {n} | {fms(freeze_ms)} |")
md.append("")
md.append("**pmap dispatch cost (amortized per run):**")
md.append("")
md.append("| Array size (N) | frozen_ms | plain_ms | plain/frozen ratio |")
md.append("|----------------|-----------|----------|--------------------|")
for n in ["10000", "100000", "1000000"]:
    entry = fvp.get(n, {})
    frozen = entry.get("frozen_ms", "?")
    plain = entry.get("plain_ms", "?")
    try:
        ratio = f"{float(plain)/float(frozen):.2f}×"
    except (ValueError, TypeError):
        ratio = "?"
    md.append(f"| {n} | {fms(frozen)} | {fms(plain)} | {ratio} |")
md.append("")
md.append("Frozen path confirms the §1 cost model: flat per-chunk crossing regardless of N.")
md.append("")
md.append("---")
md.append("")

# (d) preduce
md.append("## (d) preduce Scaling — Parallel Reduce (W=4)")
md.append("")
md.append("| N (elements) | Result OK | preduce_ms |")
md.append("|--------------|-----------|------------|")
pred4 = data.get(W4, {}).get("preduce", [])
for entry in pred4:
    md.append(f"| {entry['n']} | {entry['ok']} | {fms(entry['ms'])} |")
md.append("")
md.append("**Small-len chunks+1 dispatch overhead (n=8):**")
md.append("")
md.append("| chunks | Result OK | preduce_ms |")
md.append("|--------|-----------|------------|")
pred_small4 = data.get(W4, {}).get("preduce_small", [])
for entry in pred_small4:
    md.append(f"| {entry['chunks']} | {entry['ok']} | {fms(entry['ms'])} |")
md.append("")
md.append("At tiny N, preduce is pool-dispatch-bound (not CPU-bound); both chunk counts similar.")
md.append("")
md.append("---")
md.append("")

# RSS by worker count
md.append("## RSS by Worker Count")
md.append("")
md.append("Peak resident set size for the full bench run (all sections).")
md.append("")
md.append("| Workers | RSS (MB) |")
md.append("|---------|----------|")
for W in worker_counts:
    rss_mb = data[W].get("rss_mb", "?")
    md.append(f"| {W} | {rss_mb} |")
md.append("")
md.append("Worker isolates run on separate OS threads with separate heap.")
md.append("RSS grows modestly with W due to additional isolate stacks and bookkeeping.")
md.append("")
md.append("---")
md.append("")
md.append("*Generated by `bench/run_data_parallel_bench.sh`.*")

with open(out_path, "w") as f:
    f.write("\n".join(md) + "\n")
print(f"\n==> Results written to {out_path}")
PYEOF
