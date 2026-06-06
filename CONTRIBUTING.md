# Contributing to AScript

Thanks for your interest! This is the development guide; see [`README.md`](README.md) for an
overview and [`CLAUDE.md`](CLAUDE.md) for the detailed architecture map.

## Build & test

```bash
cargo build                                   # default features (full stdlib)
cargo test                                    # full suite
cargo test --no-default-features              # core language only
cargo clippy --all-targets                    # lint — must be clean
cargo clippy --no-default-features --all-targets

cargo run -- run examples/hello.as            # run a program on the bytecode VM
cargo run -- fmt file.as                       # format
cargo run -- check file.as                     # static check
cargo run -- lsp                               # language server (stdio)
```

CI (`.github/workflows/ci.yml`) runs build + both test configs + clippy (both configs) on every
push and PR. Keep clippy clean in **both** feature configurations.

## The tree-sitter grammar — source of truth & how to change it

The grammar has **one source of truth**: the top-level [`tree-sitter-ascript/`](tree-sitter-ascript/)
directory in this repo. The engine's `build.rs` compiles its `src/parser.c` directly, and a
standalone published repo — [`ascript-lang/tree-sitter-ascript`](https://github.com/ascript-lang/tree-sitter-ascript)
— is a **mirror** of that directory that editors (Zed, Neovim) and the npm/cargo packages consume.

To change the grammar:

1. Edit `tree-sitter-ascript/grammar.js`.
2. Regenerate the parser: `cd tree-sitter-ascript && tree-sitter generate --abi 14`.
3. Keep both hand-written + CST parsers and the examples passing (`cargo test`); the
   `tests/treesitter_conformance.rs` drift guard compiles every `queries/*.scm` against the grammar.
4. **Publish the mirror** (see below) and bump the pinned commit in the editor configs.

### Publishing the grammar mirror

The monorepo is the source of truth; the standalone repo is mirrored from it via `git subtree`.
Two equivalent paths:

- **Manual (works immediately, no setup):**
  ```bash
  ./scripts/sync-grammar.sh        # subtree-splits tree-sitter-ascript/ and pushes it to the mirror
  ```
  The script prints the new commit SHA. Update the pin in
  `editors/zed/extension.toml` (`commit = "…"`) and
  `editors/nvim/lua/ascript/treesitter.lua` (`revision = "…"`).

- **Automatic (CI):** `.github/workflows/mirror-grammar.yml` mirrors the grammar to the standalone
  repo whenever `tree-sitter-ascript/**` changes on `main`. It is **dormant until you add a secret**
  (one-time setup below). You still bump the editor pins yourself.

### One-time: enable the auto-mirror (`GRAMMAR_SYNC_TOKEN`)

The mirror workflow runs in `ascript-lang/ascript` but needs to push to a *different* repo
(`ascript-lang/tree-sitter-ascript`), which the default `GITHUB_TOKEN` cannot do. Give it a
scoped token:

1. **Create a fine-grained PAT** — GitHub → *Settings* → *Developer settings* →
   *Fine-grained tokens* → **Generate new token**:
   - **Resource owner:** `ascript-lang`
   - **Repository access:** *Only select repositories* → `ascript-lang/tree-sitter-ascript`
   - **Permissions:** *Repository permissions* → **Contents: Read and write**
   - Set an expiration that suits you.
2. **Add it as a repo secret** — `ascript-lang/ascript` → *Settings* → *Secrets and variables* →
   *Actions* → **New repository secret**:
   - **Name:** `GRAMMAR_SYNC_TOKEN`
   - **Value:** the token from step 1.

That's it — the next change to `tree-sitter-ascript/**` on `main` auto-publishes to the mirror.
Until the secret exists, the workflow no-ops and you use `scripts/sync-grammar.sh` manually.
GitHub Actions is free and unlimited on public repositories, so this costs nothing.

## Conventions

- Commit trailer: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` for AI-assisted work.
- The internal design specs and milestone plans live under [`superpowers/`](superpowers/) (not
  web-hosted). The authoritative language spec is `superpowers/specs/2026-05-29-ascript-design.md`.
- Behavior changes must keep the two engines (tree-walking interpreter and bytecode VM)
  byte-identical — the `vm_differential` test enforces this.

## License

By contributing you agree your contributions are licensed under the [MIT License](LICENSE).
