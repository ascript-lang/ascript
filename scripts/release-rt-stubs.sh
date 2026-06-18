#!/usr/bin/env bash
# RT §3.3 / §5.1 / Task 11 — build the per-target×tier stub matrix and produce the
# SIGNED release manifest. This is the worker the tag-triggered CI workflow
# (.github/workflows/release-rt.yml) drives; it can also be dry-run locally.
#
# Usage:
#   scripts/release-rt-stubs.sh --key <path-to-seed-file> [--host-only] [--out-dir DIR]
#                               [--created TS] [--target TRIPLE ...]
#
#   --key <path>     File holding the 64-hex-char ed25519 private signing seed (the CI
#                    secret ASCRIPT_RT_SIGNING_KEY). REQUIRED. Never echoed.
#   --host-only      Build only the host triple's 4 tiers (the locally-runnable dry run)
#                    instead of the full §3.3 8×4 matrix.
#   --out-dir DIR    Where to assemble stubs + rt-manifest.json[.sig] (default: ./rt-release).
#   --created TS     The deterministic `created` timestamp embedded in the manifest
#                    (default: 1970-01-01T00:00:00Z — a fixed input, never now()).
#   --target TRIPLE  Restrict to a specific triple (repeatable). Overrides --host-only.
#
# For each (target, tier): `scripts/build-rt.sh <tier> --target <triple>` (or no --target
# for the host), then — on a darwin target built on a macOS host — ad-hoc sign the stub
# (codesign -s -), per the BIN sign-BEFORE-publish rule (the signature covers the clean
# stub so the builder's later payload append does not invalidate it, RT §6.2). Then
# compute sha256 + size, collect an entries record, and finally invoke the in-tree
# generator (`ascript rt-manifest-gen`) to emit the signed manifest.
#
# The §3.3 published set: {x86_64, aarch64} × {apple-darwin, unknown-linux-gnu,
# unknown-linux-musl, pc-windows-msvc} = 8 triples × 4 tiers = 32 artifacts. Whether a
# given triple actually builds (notably the musl ones — rusqlite bundled-C + rustls) is
# validated in CI; see the musl feasibility note in release-rt.yml + the spec §12 risk.
set -euo pipefail

# ---------------------------------------------------------------------------
# Args
# ---------------------------------------------------------------------------
KEY=""
HOST_ONLY=0
OUT_DIR="rt-release"
CREATED="1970-01-01T00:00:00Z"
EXPLICIT_TARGETS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --key)        KEY="${2:?--key needs a path}"; shift 2 ;;
    --host-only)  HOST_ONLY=1; shift ;;
    --out-dir)    OUT_DIR="${2:?--out-dir needs a dir}"; shift 2 ;;
    --created)    CREATED="${2:?--created needs a value}"; shift 2 ;;
    --target)     EXPLICIT_TARGETS+=("${2:?--target needs a triple}"); shift 2 ;;
    *) echo "unknown arg '$1'" >&2; exit 2 ;;
  esac
done

if [[ -z "$KEY" ]]; then
  echo "error: --key <path-to-seed-file> is required" >&2
  exit 2
fi
if [[ ! -f "$KEY" ]]; then
  echo "error: signing key file '$KEY' does not exist" >&2
  exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="$(cargo metadata --no-deps --format-version 1 \
  | grep -o '"name":"ascript","version":"[^"]*"' \
  | head -1 | sed 's/.*"version":"//;s/"//')"
if [[ -z "$VERSION" ]]; then
  # Fallback: parse Cargo.toml's [package] version.
  VERSION="$(awk '/^\[package\]/{p=1} p&&/^version *=/{gsub(/[" ]/,"");split($0,a,"=");print a[2];exit}' Cargo.toml)"
fi
echo "ascript version: $VERSION" >&2

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
HOST_OS="$(uname -s)"

# The §3.3 published target set.
ALL_TARGETS=(
  x86_64-apple-darwin
  aarch64-apple-darwin
  x86_64-unknown-linux-gnu
  aarch64-unknown-linux-gnu
  x86_64-unknown-linux-musl
  aarch64-unknown-linux-musl
  x86_64-pc-windows-msvc
  aarch64-pc-windows-msvc
)
TIERS=(rt-core rt-local rt-net rt-full)

if [[ ${#EXPLICIT_TARGETS[@]} -gt 0 ]]; then
  TARGETS=("${EXPLICIT_TARGETS[@]}")
elif [[ "$HOST_ONLY" -eq 1 ]]; then
  TARGETS=("$HOST_TRIPLE")
else
  TARGETS=("${ALL_TARGETS[@]}")
fi

mkdir -p "$OUT_DIR"
STUBS_DIR="$OUT_DIR"            # stubs land beside the manifest
ENTRIES_FILE="$(mktemp)"
trap 'rm -f "$ENTRIES_FILE"' EXIT

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}
size_of() {
  # Portable byte count.
  if stat -f%z "$1" >/dev/null 2>&1; then stat -f%z "$1"; else stat -c%s "$1"; fi
}

# The features list per tier, matching scripts/build-rt.sh / src/rtstub/tiers.rs.
features_for_tier() {
  case "$1" in
    rt-core)  echo '"shared","bundle-zstd"' ;;
    rt-local) echo '"shared","bundle-zstd","data","binary","log","workflow","datetime","crypto","compress","sys","sysinfo","sql","tui"' ;;
    rt-net)   echo '"shared","bundle-zstd","data","binary","log","workflow","datetime","crypto","compress","sys","sysinfo","sql","tui","net","postgres","redis","telemetry"' ;;
    rt-full)  echo '"shared","bundle-zstd","data","binary","log","workflow","datetime","crypto","compress","sys","sysinfo","sql","tui","net","postgres","redis","telemetry","intl","ai","ffi"' ;;
  esac
}

# ---------------------------------------------------------------------------
# Build the matrix
# ---------------------------------------------------------------------------
echo "[" > "$ENTRIES_FILE"
FIRST=1

for target in "${TARGETS[@]}"; do
  for tier in "${TIERS[@]}"; do
    echo ">> building $target / $tier" >&2

    # Host build omits --target (output under target/release); cross uses --target.
    if [[ "$target" == "$HOST_TRIPLE" && ${#EXPLICIT_TARGETS[@]} -eq 0 && "$HOST_ONLY" -eq 1 ]]; then
      scripts/build-rt.sh "$tier"
      built="target/release/ascript-rt"
    else
      scripts/build-rt.sh "$tier" --target "$target"
      built="target/$target/release/ascript-rt"
    fi

    # Windows stubs carry the .exe extension.
    ext=""
    if [[ "$target" == *windows* ]]; then ext=".exe"; fi
    if [[ -f "${built}.exe" ]]; then built="${built}.exe"; fi

    if [[ ! -f "$built" ]]; then
      echo "error: expected stub '$built' was not produced" >&2
      exit 1
    fi

    filename="ascript-rt-${VERSION}-${target}-${tier}${ext}"
    dest="$STUBS_DIR/$filename"
    cp "$built" "$dest"

    # RT §6.2: ad-hoc sign darwin stubs BEFORE the manifest pins them, on a macOS host.
    # The signature covers the clean stub; the builder's later payload append does not
    # invalidate it (sign-before-append rule). A non-macOS host cannot sign darwin stubs
    # — those are produced on the macos CI runner.
    if [[ "$target" == *apple-darwin* ]]; then
      if [[ "$HOST_OS" == "Darwin" ]] && command -v codesign >/dev/null 2>&1; then
        codesign --force -s - "$dest" >&2
      else
        echo "warning: cannot ad-hoc sign darwin stub '$filename' off a macOS host — \
the macos CI runner produces signed darwin stubs (RT §6.2)" >&2
      fi
    fi

    sha="$(sha256_of "$dest")"
    size="$(size_of "$dest")"
    feats="$(features_for_tier "$tier")"

    if [[ "$FIRST" -eq 0 ]]; then echo "," >> "$ENTRIES_FILE"; fi
    FIRST=0
    cat >> "$ENTRIES_FILE" <<EOF
{"target":"$target","tier":"$tier","features":[$feats],"sha256":"$sha","size":$size,"filename":"$filename"}
EOF
  done
done

echo "]" >> "$ENTRIES_FILE"

# ---------------------------------------------------------------------------
# Generate + sign the manifest via the in-tree generator.
# ---------------------------------------------------------------------------
echo ">> generating signed manifest ($CREATED)" >&2
cargo run --release --features rt-release --bin ascript -- \
  rt-manifest-gen \
  --version "$VERSION" \
  --created "$CREATED" \
  --entries-file "$ENTRIES_FILE" \
  --key "$KEY" \
  --out-dir "$OUT_DIR"

echo "release artifacts assembled in: $OUT_DIR" >&2
ls -1 "$OUT_DIR" >&2
