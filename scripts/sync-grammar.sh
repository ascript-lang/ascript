#!/usr/bin/env bash
# Publish the in-repo tree-sitter grammar (tree-sitter-ascript/) to the standalone
# ascript-lang/tree-sitter-ascript repo via git subtree. The MONOREPO is the source of
# truth; this mirrors it so editors (Zed/Neovim) + npm/cargo can consume it. CI does this
# automatically (.github/workflows/mirror-grammar.yml); run this for a manual push or if
# Actions are unavailable. To ingest external PRs made on the grammar repo, instead use:
#   git subtree pull --prefix=tree-sitter-ascript <grammar-url> main
set -euo pipefail
PREFIX="tree-sitter-ascript"
REMOTE_URL="${1:-git@github.com:ascript-lang/tree-sitter-ascript.git}"
cd "$(git rev-parse --show-toplevel)"
echo "Splitting '$PREFIX' subtree…"
SPLIT=$(git subtree split --prefix="$PREFIX")
echo "Pushing $SPLIT -> $REMOTE_URL (main)"
git push "$REMOTE_URL" "$SPLIT:main"
echo
echo "Done. Pinned commit: $SPLIT"
echo "Bump the pin in editors/zed/extension.toml (commit) and"
echo "editors/nvim/lua/ascript/treesitter.lua (revision) if the grammar changed."
