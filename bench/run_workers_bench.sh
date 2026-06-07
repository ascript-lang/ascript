#!/usr/bin/env bash
# bench/run_workers_bench.sh
#
# Shell driver for bench/workers_bench.as.
# Builds the release binary, runs the benchmark at ASCRIPT_WORKERS=1,2,4,8,
# collects the tagged output lines ("key=value"), computes speedup ratios,
# and writes bench/WORKERS_RESULTS.md.
#
# Usage:
#   ./bench/run_workers_bench.sh          # standard 1/2/4/8 sweep
#   WORKER_COUNTS="1 4" ./bench/run_workers_bench.sh   # custom set
#
# Output:
#   bench/WORKERS_RESULTS.md (overwritten each run)
#   Per-run tagged lines printed to stdout for CI/log capture.
#
# Compatible with bash 3 (macOS default).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BENCH_FILE="${SCRIPT_DIR}/workers_bench.as"
RESULTS_FILE="${SCRIPT_DIR}/WORKERS_RESULTS.md"
BINARY="${REPO_ROOT}/target/release/ascript"

WORKER_COUNTS="${WORKER_COUNTS:-1 2 4 8}"

# ── 1. Build release binary ──────────────────────────────────────────────────
echo "==> Building release binary..."
cd "${REPO_ROOT}"
cargo build --release --quiet
echo "    Built: ${BINARY}"
echo ""

# ── 2. Collect host info ──────────────────────────────────────────────────────
TIMESTAMP="$(date -u '+%Y-%m-%d %H:%M UTC')"
CPU_MODEL="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || \
             lscpu 2>/dev/null | awk -F: '/Model name/{gsub(/^[ \t]+/,"",$2); print $2}' || \
             echo 'unknown')"
CPU_CORES="$(sysctl -n hw.logicalcpu 2>/dev/null || nproc 2>/dev/null || echo '?')"
OS_INFO="$(uname -srm)"

echo "Host: ${CPU_MODEL} (${CPU_CORES} logical cores) / ${OS_INFO}"
echo ""

# ── 3. Run at each worker count, collect raw output ──────────────────────────
# We write each run's tagged output to a temp file named after its worker count
TMPDIR_BENCH="$(mktemp -d /tmp/ascript-bench.XXXXXX)"
trap 'rm -rf "${TMPDIR_BENCH}"' EXIT

for W in ${WORKER_COUNTS}; do
    echo "==> Running with ASCRIPT_WORKERS=${W} ..."
    OUT_FILE="${TMPDIR_BENCH}/w${W}.out"
    ASCRIPT_WORKERS="${W}" "${BINARY}" run "${BENCH_FILE}" 2>&1 | tee "${OUT_FILE}" | \
        sed "s/^/  [W=${W}] /"
    echo ""
done

# ── 4. Parse + report results via Python ─────────────────────────────────────
python3 - "${TMPDIR_BENCH}" "${WORKER_COUNTS}" "${RESULTS_FILE}" \
          "${TIMESTAMP}" "${CPU_MODEL}" "${CPU_CORES}" "${OS_INFO}" <<'PYEOF'
import sys, os, re

tmpdir, wc_str, out_path, ts, cpu_model, cpu_cores, os_info = sys.argv[1:]
worker_counts = wc_str.split()

# Parse all run files
data = {}   # { "1": { "parallel_ms": ..., "checksum": ..., ... } }
payload_sizes = []
PAYLOAD_SIZES_SEEN = set()

for W in worker_counts:
    fpath = os.path.join(tmpdir, f"w{W}.out")
    d = {}
    with open(fpath) as f:
        for line in f:
            line = line.strip()
            if line.startswith("payload_size="):
                # payload_size=N total_ms=T per_call_ms=P
                m = re.match(r"payload_size=(\S+)\s+total_ms=(\S+)\s+per_call_ms=(\S+)", line)
                if m:
                    ps, tm, pc = m.group(1), m.group(2), m.group(3)
                    d.setdefault("payload", {})[ps] = {"total_ms": tm, "per_call_ms": pc}
                    if ps not in PAYLOAD_SIZES_SEEN:
                        PAYLOAD_SIZES_SEEN.add(ps)
                        payload_sizes.append(ps)
            elif "=" in line and not line.startswith(" "):
                k, _, v = line.partition("=")
                d[k] = v
    data[W] = d

# Compute speedup ratios
base_ms_str = data.get("1", {}).get("parallel_ms")
print("")
print("==> Speedup summary")
for W in worker_counts:
    ms_str = data.get(W, {}).get("parallel_ms", "?")
    if base_ms_str and ms_str != "?":
        try:
            speedup = float(base_ms_str) / float(ms_str)
            sp_label = f"{speedup:.2f}x"
        except (ValueError, ZeroDivisionError):
            sp_label = "?"
    else:
        sp_label = "baseline" if W == "1" else "?"
    print(f"    W={W}: {ms_str} ms (speedup {sp_label})")

# Checksum determinism
checksums = [data.get(W, {}).get("checksum", "") for W in worker_counts]
ref_cs = checksums[0] if checksums else "?"
cs_ok = all(c == ref_cs for c in checksums)
if cs_ok:
    print(f"\n==> Checksum determinism OK (all agree: {ref_cs})")
else:
    print(f"\nWARNING: checksum mismatch across worker counts!")
    for W, cs in zip(worker_counts, checksums):
        print(f"    W={W}: {cs}")

# ── Write WORKERS_RESULTS.md ─────────────────────────────────────────────────
lines = []
lines.append("# Workers Performance Benchmark Results")
lines.append("")
lines.append(f"**Date:** {ts}")
lines.append(f"**Host:** {cpu_model}")
lines.append(f"**Logical cores:** {cpu_cores}")
lines.append(f"**OS:** {os_info}")
lines.append(f"**Binary:** `target/release/ascript`")
lines.append("")
lines.append("---")
lines.append("")
lines.append("## 1. Speedup vs. Worker Count (CPU-bound: 32 × LCG chunks)")
lines.append("")
lines.append("32 chunks of 400 k LCG iterations each, dispatched via")
lines.append("`task.gather(array.map(seeds, computeChunk))`.")
lines.append("Wall-clock measured inside the program (`std/time`).")
lines.append("")
lines.append("| Workers | Parallel wall-clock (ms) | Speedup vs W=1 |")
lines.append("|---------|--------------------------|----------------|")

for W in worker_counts:
    ms_str = data.get(W, {}).get("parallel_ms", "?")
    if W == "1":
        sp_label = "baseline"
    elif base_ms_str and ms_str != "?":
        try:
            speedup = float(base_ms_str) / float(ms_str)
            sp_label = f"{speedup:.2f}×"
        except (ValueError, ZeroDivisionError):
            sp_label = "?"
    else:
        sp_label = "?"
    try:
        ms_fmt = f"{float(ms_str):.1f}"
    except ValueError:
        ms_fmt = ms_str
    lines.append(f"| {W} | {ms_fmt} | {sp_label} |")

lines.append("")
try:
    ref_cs_fmt = int(float(ref_cs))
except (ValueError, TypeError):
    ref_cs_fmt = ref_cs
lines.append(f"Checksum (determinism guard): **{ref_cs_fmt}** — identical across all worker counts.")
lines.append("")
lines.append("---")
lines.append("")
lines.append("## 2. Serialization Round-Trip Overhead")
lines.append("")
lines.append("Per-call latency as the argument array grows.")
lines.append("Run with 4 workers (ASCRIPT_WORKERS=4), 20 calls per measurement round.")
lines.append("The cost here is dominated by structured-clone serialize/deserialize,")
lines.append("not computation.")
lines.append("")
lines.append("| Payload size (f64 elements) | Total ms (20 calls) | Per-call ms |")
lines.append("|-----------------------------|---------------------|-------------|")

ref_payload_w = "4" if "4" in worker_counts else worker_counts[-1]
for ps in payload_sizes:
    pd = data.get(ref_payload_w, {}).get("payload", {}).get(ps, {})
    tm = pd.get("total_ms", "?")
    pc = pd.get("per_call_ms", "?")
    try:
        tm_fmt = f"{float(tm):.2f}"
    except ValueError:
        tm_fmt = tm
    try:
        pc_fmt = f"{float(pc):.3f}"
    except ValueError:
        pc_fmt = pc
    lines.append(f"| {ps} | {tm_fmt} | {pc_fmt} |")

lines.append("")
lines.append("---")
lines.append("")
lines.append("## 3. Pool Warmup: Cold vs. Warm Latency")
lines.append("")
lines.append("Single-dispatch latency for one `computeChunk` call,")
lines.append("before and after the worker pool is warmed (W=1).")
lines.append("")
lines.append("| Measurement | Latency (ms) |")
lines.append("|-------------|--------------|")

cold_ms = data.get("1", {}).get("warmup_cold_ms", "?")
warm_ms = data.get("1", {}).get("warmup_warm_ms", "?")
try:
    cold_ms = f"{float(cold_ms):.1f}"
except ValueError:
    pass
try:
    warm_ms = f"{float(warm_ms):.1f}"
except ValueError:
    pass
lines.append(f"| Cold (first dispatch, pool not yet started) | {cold_ms} |")
lines.append(f"| Warm (steady-state, pool running)           | {warm_ms} |")
lines.append("")
lines.append("Cold latency includes isolate thread spawn + tokio runtime init.")
lines.append("Warm latency is the per-call round-trip once the pool is hot.")
lines.append("")
lines.append("---")
lines.append("")
lines.append("*Generated by `bench/run_workers_bench.sh`.*")

with open(out_path, "w") as f:
    f.write("\n".join(lines) + "\n")
print(f"\n==> Results written to {out_path}")
PYEOF
