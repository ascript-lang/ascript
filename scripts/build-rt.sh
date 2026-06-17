#!/usr/bin/env bash
# Build a runtime-only stub (`ascript-rt`). RT §2.2/§3.2.
#
# Usage: scripts/build-rt.sh <rt-core|rt-local|rt-net|rt-full> [extra cargo args...]
#
# The tier names map to a cumulative CHAIN of Cargo feature sets (spec §3.2):
#   rt-core  ⊂ rt-local ⊂ rt-net ⊂ rt-full
# The `ASCRIPT_RT=1` env triggers `build.rs` to emit `cfg(ascript_rt)` (the
# frontend gate) and skip the tree-sitter `cc` compile; `ASCRIPT_RT_TIER` stamps
# the tier name into the binary (surfaced by `--rt-info`).
set -euo pipefail

TIER="${1:-}"; shift || true
case "$TIER" in
  rt-core)  FEATURES="shared,bundle-zstd" ;;
  rt-local) FEATURES="shared,bundle-zstd,data,binary,log,workflow,datetime,crypto,compress,sys,sysinfo,sql,tui" ;;
  rt-net)   FEATURES="shared,bundle-zstd,data,binary,log,workflow,datetime,crypto,compress,sys,sysinfo,sql,tui,net,postgres,redis,telemetry" ;;
  rt-full)  FEATURES="shared,bundle-zstd,data,binary,log,workflow,datetime,crypto,compress,sys,sysinfo,sql,tui,net,postgres,redis,telemetry,intl,ai,ffi" ;;
  *) echo "unknown tier '$TIER' (expected one of: rt-core, rt-local, rt-net, rt-full)" >&2; exit 2 ;;
esac

ASCRIPT_RT=1 ASCRIPT_RT_TIER="$TIER" cargo build --release --bin ascript-rt \
  --no-default-features --features "$FEATURES" "$@"
