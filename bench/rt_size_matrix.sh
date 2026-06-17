#!/usr/bin/env bash
# bench/rt_size_matrix.sh — RT Phase-0 binary-size matrix
# Measures the ascript binary size across feature configurations so later
# size claims about ascript-rt stub tiers can trace to real numbers.
#
# Usage: ./bench/rt_size_matrix.sh (run from repo root)
# Output: human-readable table on stdout; capture with tee.
#
# ## Per-tier ascript-rt sizes (appended by Task 2)
# [PLACEHOLDER — Task 2 will fill in the ascript-rt stub sizes per tier]

set -euo pipefail

BINARY="target/release/ascript"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "=== RT Phase-0 Size Matrix ==="
echo "Date:     $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
echo "Machine:  $(uname -a)"
echo "Rust:     $(rustc --version)"
echo ""

###############################################################################
# Step 1: Full-binary baseline (all default features)
###############################################################################
echo "--- Step 1: Full binary baseline (default features) ---"
cargo build --release 2>&1 | grep -E '(Compiling ascript|Finished|error)' || true
FULL_SIZE=$(stat -f%z "$BINARY" 2>/dev/null || stat -c%s "$BINARY")
FULL_SIZE_MB=$(echo "scale=2; $FULL_SIZE / 1048576" | bc)
echo "Full binary size: $FULL_SIZE bytes (${FULL_SIZE_MB} MB)"
echo ""

###############################################################################
# Step 2: cargo bloat (or fallback to size/ls)
###############################################################################
echo "--- Step 2: Symbol breakdown ---"
if cargo bloat --version >/dev/null 2>&1; then
    echo "[cargo-bloat available]"
    cargo bloat --release -n 40 2>&1
else
    echo "[cargo-bloat not found — attempting install]"
    if cargo install cargo-bloat 2>&1; then
        cargo bloat --release -n 40 2>&1
    else
        echo "[cargo-bloat install failed — falling back to section sizes]"
        if command -v size >/dev/null 2>&1; then
            size "$BINARY"
        else
            ls -lh "$BINARY"
        fi
    fi
fi
echo ""

###############################################################################
# Step 3: Per-feature size deltas (floor = --no-default-features --features shared)
###############################################################################
echo "--- Step 3: Per-feature size deltas ---"
echo "(floor = --no-default-features --features shared)"
echo ""

FEATURES=(
    shared
    data
    binary
    log
    workflow
    datetime
    crypto
    compress
    sys
    sysinfo
    sql
    tui
    net
    postgres
    redis
    telemetry
    intl
    ai
    ffi
)

# Record floor (shared alone)
echo "Building floor: --no-default-features --features shared ..."
cargo build --release --no-default-features --features "shared" 2>&1 | grep -E '(Compiling ascript|Finished|error)' || true
FLOOR_SIZE=$(stat -f%z "$BINARY" 2>/dev/null || stat -c%s "$BINARY")
FLOOR_SIZE_MB=$(echo "scale=2; $FLOOR_SIZE / 1048576" | bc)
echo "Floor (shared only): $FLOOR_SIZE bytes (${FLOOR_SIZE_MB} MB)"
echo ""

printf "%-20s %15s %15s %12s\n" "Feature" "Binary (bytes)" "Binary (MB)" "Delta vs floor"
printf "%-20s %15s %15s %12s\n" "-------" "--------------" "-----------" "--------------"
printf "%-20s %15s %15s %12s\n" "shared (floor)" "$FLOOR_SIZE" "${FLOOR_SIZE_MB} MB" "0"

for F in "${FEATURES[@]}"; do
    if [[ "$F" == "shared" ]]; then
        continue
    fi
    echo "Building --no-default-features --features shared,$F ..."
    cargo build --release --no-default-features --features "shared,$F" 2>&1 | grep -E '(Compiling ascript|Finished|error)' || true
    SZ=$(stat -f%z "$BINARY" 2>/dev/null || stat -c%s "$BINARY")
    SZ_MB=$(echo "scale=2; $SZ / 1048576" | bc)
    DELTA=$((SZ - FLOOR_SIZE))
    printf "%-20s %15s %15s %12s\n" "$F" "$SZ" "${SZ_MB} MB" "+$DELTA"
done

echo ""
echo "=== Done ==="
