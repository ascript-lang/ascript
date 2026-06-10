#!/usr/bin/env bash
# FUZZ Task 5 — regenerate the `aso_roundtrip` seed corpus from `examples/**`.
#
# The seeds are REAL `.aso` files built by `ascript build` (→ `ascript::build_file`), so they
# carry the CURRENT `ASO_FORMAT_VERSION`. The libFuzzer mutator flips bytes inside these
# valid proto trees to reach the deep `read_*` arms (spec §3.2 / §4.2).
#
# MUST be re-run on any `ASO_FORMAT_VERSION` bump (CLAUDE.md `.aso`-versioning checklist):
# stale-version seeds only ever hit the version-reject path, collapsing the reader coverage
# floor (spec §4.2 / must-fix #5).
#
# Usage (from the repo root):   ./fuzz/regenerate_aso_corpus.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORPUS_DIR="$REPO_ROOT/fuzz/corpus/aso_roundtrip"
BIN="$REPO_ROOT/target/release/ascript"

if [[ ! -x "$BIN" ]]; then
  echo "building release binary…" >&2
  (cd "$REPO_ROOT" && cargo build --release --bin ascript)
fi

mkdir -p "$CORPUS_DIR"
# Remove only previously-generated example seeds (prefixed `ex_`); keep curated known-bad seeds.
rm -f "$CORPUS_DIR"/ex_*.aso

built=0
skipped=0
# server_multicore binds a port + blocks at runtime, but `build` only COMPILES (never runs),
# so it serializes fine and is a valid, useful seed — no skip needed for the build step.
while IFS= read -r src; do
  rel="${src#"$REPO_ROOT"/examples/}"
  name="ex_$(echo "$rel" | tr '/' '_' | sed 's/\.as$//')"
  out="$CORPUS_DIR/$name.aso"
  if "$BIN" build "$src" -o "$out" >/dev/null 2>&1; then
    built=$((built + 1))
  else
    skipped=$((skipped + 1))
    echo "  skip (build failed): $rel" >&2
  fi
done < <(find "$REPO_ROOT/examples" -name '*.as' | sort)

echo "seed corpus regenerated: $built built, $skipped skipped → $CORPUS_DIR" >&2
