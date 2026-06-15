#!/usr/bin/env bash
# scripts/nanb-cross-repr-diff.sh
#
# NANB Phase 3, Task 3.1 — cross-BINARY old-vs-new repr differential.
#
# The within-process `vm_differential` cannot prove the repr is behavior-invisible
# on its own: both engines in ONE binary share the SAME `Value` repr (spec §0
# "Engines"). The real oracle is TWO separately-built binaries from the SAME commit
# — the 24-byte default and the 16-byte `--features value16` — run over the WHOLE
# corpus, with stdout/stderr/exit-code diffed byte-for-byte.
#
# The two binaries are built into the ONE existing target/ by FEATURE-TOGGLE (a
# feature flip recompiles in place — no worktree, no second target/). The caller
# may pass pre-built binaries to skip the build:
#   scripts/nanb-cross-repr-diff.sh [base-bin] [value16-bin]
#
# Output: a per-file PASS/FAIL line + a summary. Exit non-zero on ANY diff.
# bash 3 compatible (macOS default).

set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

BASE_BIN="${1:-}"
V16_BIN="${2:-}"

if [ -z "${BASE_BIN}" ]; then
  echo "==> Building default (24-byte) binary..."
  cargo build --release --quiet
  BASE_BIN="/tmp/ascript-nanb-base"
  cp target/release/ascript "${BASE_BIN}"
  echo "==> Building value16 (16-byte) binary..."
  cargo build --release --features value16 --quiet
  V16_BIN="/tmp/ascript-nanb-v16"
  cp target/release/ascript "${V16_BIN}"
fi

# Files excluded from the run-to-completion stdout-equality oracle. This list MIRRORS
# tests/vm_differential.rs EXAMPLE_SKIPS (nondeterministic / shared-external-state /
# long-running-server / relative-imports). A diff oracle over two binaries has the
# SAME limitations a two-run oracle has.
SKIPS="
examples/host_info.as
examples/system.as
examples/advanced/crypto_and_compress.as
examples/advanced/datetime_intl.as
examples/advanced/sse_client.as
examples/advanced/ws_client.as
examples/advanced/ai_chat.as
examples/advanced/ai_tools.as
examples/advanced/typed_api.as
examples/advanced/http_client.as
examples/advanced/fs_toolkit.as
examples/advanced/workflow_signup.as
examples/advanced/http_server.as
examples/advanced/ws_server.as
examples/advanced/server_multicore.as
examples/bundle_multimodule.as
examples/advanced/bundle_caps.as
"

is_skipped() {
  case "${SKIPS}" in
    *"
$1
"*) return 0 ;;
  esac
  return 1
}

run_capture() {
  # $1 binary, $2 file → prints "<exit>\n<stdout+stderr>"
  local out rc
  out="$("$1" run "$2" 2>&1)"; rc=$?
  printf '%s\n%s' "${rc}" "${out}"
}

FILES=$(find examples -name '*.as' | sort)
total=0; ran=0; skipped=0; failed=0
FAILED_LIST=""

for f in ${FILES}; do
  total=$((total+1))
  if is_skipped "${f}"; then
    skipped=$((skipped+1))
    continue
  fi
  ran=$((ran+1))
  a="$(run_capture "${BASE_BIN}" "${f}")"
  b="$(run_capture "${V16_BIN}" "${f}")"
  if [ "${a}" = "${b}" ]; then
    printf "  PASS  %s\n" "${f}"
  else
    printf "  FAIL  %s\n" "${f}"
    failed=$((failed+1))
    FAILED_LIST="${FAILED_LIST}${f}\n"
    echo "----- base (24-byte) -----"; printf '%s\n' "${a}"
    echo "----- value16 (16-byte) -----"; printf '%s\n' "${b}"
    echo "--------------------------"
  fi
done

echo ""
echo "==> cross-repr differential summary"
echo "    corpus files : ${total}"
echo "    ran (diffed) : ${ran}"
echo "    skipped      : ${skipped}  (nondeterministic / server / relative-import)"
echo "    DIFFS        : ${failed}"
if [ "${failed}" -ne 0 ]; then
  echo "    FAILED FILES:"; printf "${FAILED_LIST}"
  echo "==> RESULT: DIVERGENCE — repr is NOT behavior-invisible (BUG)"
  exit 1
fi
echo "==> RESULT: byte-identical across both reprs (${ran}/${ran})"
