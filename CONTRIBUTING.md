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

## Fuzzing & property tests

AScript is fuzzed two ways:

- **In-tree property tests** (`tests/property.rs`, `proptest` + the `src/fuzzgen` grammar-aware
  generator) run as part of the normal `cargo test` — deterministic and seeded. They guard the
  three-way differential, the `.aso`/worker-clone round-trips, and the GC. **No special toolchain.**
- **libFuzzer targets** live in `fuzz/` — an **isolated cargo workspace member** (its own
  `[workspace]`) so `libfuzzer-sys`/`cargo-fuzz` **never enter the root `ascript` build graph**
  (verify with `cargo tree -e normal` — neither `libfuzzer-sys` nor a non-optional `arbitrary`
  should appear). Four targets: `aso_roundtrip` (the `.aso` reader+verifier), `worker_serialize`
  (the structured-clone airlock), `differential` (three-engine divergence), `parser`. Run one:

  ```bash
  cargo install cargo-fuzz                    # one-time (needs the nightly toolchain)
  cargo +nightly fuzz run aso_roundtrip       # ctrl-C to stop; grows fuzz/corpus/aso_roundtrip/
  ```

  Only the hand-curated `ex_*`/`bad_*` seeds under `fuzz/corpus/<target>/` are committed; the
  nightly-grown corpus is gitignored (CI-cached). Regenerate the `.aso` example seeds after an
  `ASO_FORMAT_VERSION` bump: `./fuzz/regenerate_aso_corpus.sh` (the in-suite
  `aso_seed_corpus_is_present_and_current` test tripwires a stale corpus).

- **CI:** `ci.yml`'s `fuzz-smoke` job builds every target and replays the committed corpus per PR;
  `fuzz-nightly.yml` runs the deep, time-boxed campaign (the `aso_roundtrip` run is **4 h**).
- **When a fuzz run crashes:** minimize the reproducer (`cargo +nightly fuzz tmin <target> <file>`),
  add it to `fuzz/corpus/<target>/` as a `bad_*` seed, add a **normal-suite regression test** (e.g.
  in `tests/property.rs`) that pins the now-clean behavior, *then* fix the bug — the regression guard
  is permanent (Gate 0).
- **The BIN gate:** `ascript build --native` (BIN) may not land until the `aso_roundtrip` target has
  **≥ 7 consecutive nightly ≥ 4 h crash-free runs** (the streak resets on any `src/vm/aso.rs`
  `read_*` / `src/vm/verify.rs` edit) **and** the corpus reaches ≥ 90 % line coverage of the
  `read_*` family. See `.github/workflows/fuzz-nightly.yml` and the native-binary plan's Task 0.

## Conventions

- Commit trailer: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` for AI-assisted work.
- The internal design specs and milestone plans live under [`superpowers/`](superpowers/) (not
  web-hosted). The authoritative language spec is `superpowers/specs/2026-05-29-ascript-design.md`.
- Behavior changes must keep the two engines (tree-walking interpreter and bytecode VM)
  byte-identical — the `vm_differential` test enforces this.

## License

By contributing you agree your contributions are licensed under the [MIT License](LICENSE).
