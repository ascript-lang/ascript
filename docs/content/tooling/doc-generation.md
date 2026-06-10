# Doc generation

`ascript doc` turns the doc-comments in your source into browsable API documentation —
a self-contained HTML site or Markdown — straight from the `ascript` binary, with no
external toolchain. It is the documentation half of AScript's "real tooling in one
binary" story, alongside the [LSP](lsp-capabilities), the [formatter](../cli), and the
[debugger and profiler](debugging-profiling).

## Doc-comments: `///` and `//!`

Two comment forms are documentation (everything else is an ordinary comment):

- **`///`** — an **item** doc-comment. Placed directly above a declaration, it documents
  that `fn`, `class`, `enum`, field, method, or `const`.
- **`//!`** — a **module** doc-comment. Placed at the top of a file, it documents the
  module as a whole (it becomes the page's lead paragraph).

The body is **Markdown**, rendered as-is — so you can use lists, code spans, links, and
emphasis. A light `@param name — …` / `@returns …` convention reads well and renders
verbatim; it is a convention, not a required syntax.

```ascript
//! A small money ledger — amounts in integer minor units to avoid float drift.

/// The minor-unit scale of a currency (100 cents per dollar, 1 for yen).
///
/// @param c — the currency to look up
/// @returns the scale (1, 10, or 100)
export fn scaleOf(c: Currency): int {
  match (c) {
    Currency.USD, Currency.EUR => 100,
    Currency.JPY => 1,
  }
}

/// An immutable signed money amount in a single currency.
export class Money {
  minor: int
  currency: Currency
}
```

By default only the **public API** (exported declarations) is documented; pass
`--private` to include unexported items.

## Usage

```bash
ascript doc                       # document the current project (HTML → target/doc/)
ascript doc src/                  # document a file or directory
ascript doc lib.as --format md    # Markdown to stdout
ascript doc lib.as --format md --out docs-out/   # Markdown to a directory
ascript doc --open                # generate HTML and open it (best-effort, sys-gated)
```

Paths default to the current directory, discovered the same way [`ascript check`](../cli)
discovers sources; imports are resolved so a whole project documents together.

| Option | Meaning |
|---|---|
| `--format html` *(default)* | A self-contained site under `--out` (default `target/doc/`). |
| `--format md` | Markdown to stdout, or to `--out` if given. |
| `--out <DIR>` | Output directory for the generated site/files. |
| `--private` | Include non-exported declarations (default: public API only). |
| `--open` | Open the generated `index.html` afterwards (best-effort, `sys`-gated). |
| `--check` | Write nothing; **exit non-zero** if any public declaration lacks a doc-comment, listing the offenders. |

## `--check`: a documentation CI gate

`ascript doc --check` is a lint, not a generator: it produces no output and exits
non-zero when a public declaration is undocumented, naming each one. Wire it into CI to
keep the public API fully documented:

```bash
ascript doc --check        # exits 1 (and lists symbols) if any public API is undocumented
```

## Dogfooding

The repository's [`examples/advanced/documented_library.as`](https://github.com/ascript-lang/ascript/blob/main/examples/advanced/documented_library.as)
is the canonical worked example: a fully-documented module that simultaneously serves as
the `ascript doc` golden source, runs to completion, and ships an in-file
[`test(...)`](../cli) suite exercised under `ascript test --coverage`.
