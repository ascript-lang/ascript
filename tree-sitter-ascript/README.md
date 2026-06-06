# tree-sitter-ascript

A [tree-sitter](https://tree-sitter.github.io/tree-sitter/) grammar for
[AScript](https://github.com/ascript-lang/ascript) — a small, dynamically-typed, JavaScript-flavored
scripting language (`.as` files) with optional runtime-checked type contracts and a
batteries-included standard library. This package provides incremental parsing plus a full set of
editor query files (syntax highlighting, code folding, indentation, and more) for any editor or tool
built on tree-sitter.

## Install

npm:

```sh
npm install tree-sitter-ascript
```

Cargo (`Cargo.toml`):

```toml
[dependencies]
tree-sitter-ascript = "0.1"
```

## Queries

The package ships the following tree-sitter query files under `queries/`:

- `highlights.scm` — syntax highlighting
- `injections.scm` — embedded-language injections
- `locals.scm` — local scope / definition & reference tracking
- `folds.scm` — code folding regions
- `indents.scm` — auto-indentation rules
- `textobjects.scm` — selection text objects (functions, classes, parameters, etc.)
- `tags.scm` — symbol tags for code navigation
- `brackets.scm` — matching-bracket pairs

## Source of truth

This repo is a **published mirror**. The canonical grammar lives in the AScript monorepo
([`ascript-lang/ascript`](https://github.com/ascript-lang/ascript)) at the top-level
`tree-sitter-ascript/` directory — the single source of truth for both this npm/cargo
artifact and the AScript engine itself (the engine's `build.rs` compiles the same
`src/parser.c` directly). Changes are made there and mirrored here via `git subtree`
(see `scripts/sync-grammar.sh` / the `mirror-grammar` workflow in the monorepo). When the
grammar changes, regenerate `parser.c` with `tree-sitter generate --abi 14` so both
consumers stay in sync.
