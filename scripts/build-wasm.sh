#!/usr/bin/env bash
# WASM §5.5 / Task 2.2 — the ONE artifact pipeline for the browser playground.
# Builds the workspace-excluded `ascript-wasm/` crate for the web target, runs the
# `wasm-opt -Oz` size pass, and copies the optimized pkg into the static docs site.
#
# Release for the shipped artifact (size + speed); the gcmodule wasm32 `GcHeader` align fix
# (ascript-wasm/Cargo.toml `[patch]`) means both debug and release run correctly.
set -euo pipefail

cd "$(dirname "$0")/../ascript-wasm"

# --target web → the ES-module pkg the playground Web Worker imports (§5.5).
wasm-pack build --release --target web

# Size pass: optimize for size (-Oz), overwrite the bindgen-emitted wasm in place.
wasm-opt -Oz -o pkg/ascript_wasm_bg.wasm.opt pkg/ascript_wasm_bg.wasm
mv pkg/ascript_wasm_bg.wasm.opt pkg/ascript_wasm_bg.wasm

# Stage the committed artifact for the static docs site (served from a plain checkout +
# `python3 -m http.server`, per CLAUDE.md). The CI smoke rebuilds + diff-checks this so the
# committed copy can't go stale silently.
mkdir -p ../docs/assets/playground/pkg
cp pkg/ascript_wasm.js pkg/ascript_wasm_bg.wasm ../docs/assets/playground/pkg/

echo "build-wasm: wrote pkg/ + docs/assets/playground/pkg/ ($(ls -l pkg/ascript_wasm_bg.wasm | awk '{print $5}') bytes optimized)"
