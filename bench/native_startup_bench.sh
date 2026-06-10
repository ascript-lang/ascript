#!/usr/bin/env bash
# BIN Task 7 — the per-launch cost the native-bundle startup shim (`try_run_embedded`) adds
# to EVERY `ascript` launch (NOT just bundled ones): a `current_exe()` resolve + open + stat +
# seek-to-end + a 32-byte tail read (`bundle::validate_footer`). It NEVER reads the whole
# (tens-of-MB) image — that is what keeps it negligible.
#
# Budget (spec §2.3 / R2): <= 1 ms AND <= 2% of bare startup. "Negligible needs a number."
#
# Usage: cargo build --release && ./bench/native_startup_bench.sh
set -euo pipefail
BIN="${1:-target/release/ascript}"
N="${N:-1000}"
EMPTY="$(mktemp -t empty_XXXX.as)"; : > "$EMPTY"

# (1) Absolute startup: N launches of a do-nothing program (process spawn + tokio runtime +
#     the 512 MB worker-stack thread dominate; the shim runs on this path).
python3 - "$BIN" "$EMPTY" "$N" <<'PY'
import subprocess, sys, time
binp, empty, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
subprocess.run([binp,"run",empty], capture_output=True)  # warm
t0=time.time()
for _ in range(n):
    subprocess.run([binp,"run",empty], capture_output=True)
t=time.time()-t0
print(f"absolute startup: {t/n*1000:.3f} ms/launch over {n} runs")
PY

# (2) The shim's ADDED cost in isolation (the footer tail-read), 100k iterations.
python3 - "$BIN" <<'PY'
import os, sys, time
exe=sys.argv[1]; N=100000
with open(exe,'rb') as f: f.seek(-32,2); f.read(32)   # warm cache
t0=time.perf_counter()
for _ in range(N):
    os.path.getsize(exe)
    with open(exe,'rb') as f:
        f.seek(-32,2); f.read(32)
per=(time.perf_counter()-t0)/N*1000
print(f"shim footer-read added cost: {per*1000:.2f} us/launch ({per:.5f} ms)")
PY

rm -f "$EMPTY"

# ── Recorded result (Apple M-series, macOS arm64, release, 2026-06-10) ───────────────────
#   absolute startup:            6.378 ms/launch (1000 runs)
#   shim footer-read added cost: ~9.79 us/launch (0.00979 ms) — a conservative Python upper
#                                bound; the Rust tail-read is lighter.
#   => 0.154% of startup, ~0.01 ms.  Budget <= 1 ms / <= 2%:  PASS (by ~100x).
# The shim reads ONLY the 32-byte tail (bundle::validate_footer), never the whole image —
# that design is why the per-launch tax is microseconds, not milliseconds.
