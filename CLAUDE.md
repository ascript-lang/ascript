# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

AScript is a small, dynamically-typed scripting language (`.as` files) with JavaScript-flavored
syntax, optional runtime-checked type contracts, and a batteries-included standard library. It is
implemented as a single Rust binary `ascript` containing an async tree-walking interpreter.

The design goal is **"Lua-simple language, Go/Deno-class standard library"**: the core stays tiny
(~8 value kinds, gradual contracts, no hidden control flow) while the stdlib is deliberately rich.
The authoritative design is `docs/superpowers/specs/2026-05-29-ascript-design.md` — the entire spec
(§§2–16) is implemented. `docs/superpowers/roadmap.md` is the milestone-by-milestone record.

## Commands

```bash
cargo build                              # build (default features = full stdlib)
cargo test                               # full suite (~540 tests, all features)
cargo test --no-default-features         # core language only (~245 tests)
cargo test <name>                        # run a single test by name substring
cargo test --test cli                    # run one integration test file (tests/*.rs)
cargo clippy --all-targets               # lint — must be clean in BOTH feature configs

cargo run -- run examples/hello.as       # run a .as program
cargo run -- repl                        # interactive REPL
cargo run -- fmt file.as                 # format in place
cargo run -- test file.as ...            # run a .as file's test(name, fn) registrations
cargo run -- lsp                         # language server over stdio (LSP)
```

Clippy must be clean under both `--all-targets` and `--no-default-features --all-targets`. Run both
before considering work done.

## Architecture

### Pipeline
`lexer` → `parser` (precedence-climbing) → `interp` (async tree-walker). The shared front-end
(`lexer`, `token`, `ast`, `parser`, `span`) is also consumed by `fmt`, `repl`, and the `lsp` (which
is static-analysis only and never instantiates the interpreter).

Source flows as: `lexer::lex(src)` → `parser::parse(&tokens)` → `Interp::exec`/`load_module`. Every
token and AST node carries a `Span` (byte offsets + line/col) so `diagnostics` (ariadne-backed) can
point at exact source. Entry points live in `src/lib.rs`: `run_file`, `run_source`, `run_tests`.

### The interpreter (`src/interp.rs`)
- `eval_expr`/`exec` are `async` (`#[async_recursion]`) to establish spec §7's single-threaded event
  loop. The whole runtime is `Rc`/`RefCell`-based and therefore **`!Send`** — the binary uses
  `#[tokio::main(flavor = "current_thread")]`. Do not introduce `Send` bounds or spawn onto a
  multi-thread runtime; async I/O is dispatched inline rather than via a new future kind.
- **Control flow uses two enums, not `Result<_, io::Error>`:**
  - `Flow { Normal, Return(Value), Break, Continue }` — normal statement-level control flow.
  - `Control { Panic(AsError), Propagate(Value) }` — the error channel. `Panic` is an unrecoverable
    Tier-2 programmer error (aborts unless caught by `recover`); `Propagate` is the `?`-operator
    early return carrying a `[nil, err]` Result pair. `AsError` converts into `Control::Panic`.
- `global_env()` builds a fresh environment with builtins installed; programs run in a `.child()` of
  it so they can shadow builtins (`let len = 5`).
- **Native resource handles:** OS resources (TCP streams, child processes, HTTP bodies/servers, SSE,
  WebSocket connections, terminals) are NOT embedded in `Value`. They live in `Interp.resources`
  (a `HashMap<u64, ResourceState>`) and are referenced from script by a `Value::Native` handle id.
  This keeps `Value` cheap/cloneable and lets the interpreter reclaim fds deterministically. When
  adding a stateful native API, add a `ResourceState` variant + accessor methods on `Interp`.

### Values (`src/value.rs`)
`Value` is the ~14-variant runtime tagged union: `Bool`, `Number(f64)`, `Str(Rc<str>)`, `Builtin`,
`Function`, `Array`, `Object` (insertion-ordered `IndexMap`), `Map`, `Bytes`, `Regex`, `Native`,
`Enum`, `Class`, `Instance`, `Super`. Mutable containers are `Rc<RefCell<...>>`. There is a separate
hashable `MapKey` (numbers canonicalized: −0.0→+0.0, NaN unified) for `Map` keys.

### Standard library (`src/stdlib/`)
Each `std/*` module is native Rust over the `Value` model. Two routing entry points in
`src/stdlib/mod.rs`:
- `std_module_exports("std/math")` → the `(name, Value)` bindings an `import` brings in.
- `call(module, func, args, span)` → routes qualified builtin calls (`"math.abs"`) to e.g.
  `math::call`.

To add a stdlib module: create `src/stdlib/foo.rs` exposing `exports()` and `call(...)`, register it
in both match arms of `src/stdlib/mod.rs`, declare the `pub mod` (gated by the right `#[cfg(feature)]`),
and add the example/test. Native functions are ordinary `function` values; argument-type misuse is a
Tier-2 panic.

### Feature flags (`Cargo.toml`)
The stdlib is split into Cargo features, all on by `default`: `data` (json/regex/encoding/csv/toml/
yaml/uuid/bytes), `datetime`, `intl`, `sys` (fs/process/env), `crypto`, `compress`, `sql` (sqlite),
`net` (tcp/http/ws/server), `tui` (crossterm), `lsp` (tower-lsp). Every module is `#[cfg]`-gated so
`--no-default-features` builds the bare language. `http3` is opt-in and additionally requires
`RUSTFLAGS="--cfg reqwest_unstable"` (reqwest's http3 is unstable).

### Tree-sitter grammar
`build.rs` compiles a vendored Tree-sitter parser from
`docs/superpowers/specs/grammar/tree-sitter-ascript/src/parser.c` via the `cc` crate.
`tests/treesitter_conformance.rs` asserts BOTH the grammar and the hand-written parser accept every
`examples/*.as` file with no errors. `tests/frontend_conformance.rs` is a differential parser
guardrail. If you change syntax, update both parsers and keep the examples passing.

## Tests
- Unit tests live inline (`#[test]` / `#[tokio::test]`) in `src/*.rs`.
- Integration tests in `tests/`: `cli.rs`, `modules.rs`, `lsp.rs`, `treesitter_conformance.rs`,
  `frontend_conformance.rs`. These spawn the built binary (`env!("CARGO_BIN_EXE_ascript")`).
- `examples/*.as` double as living documentation and are exercised by the conformance tests — keep
  them runnable.

## Conventions
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Workflow per milestone (see roadmap): writing-plans → subagent-driven-development (a fresh
  implementer plus an *independent* reviewer that runs commands and probes edges) → holistic review →
  merge `--no-ff`. Plans live in `docs/superpowers/plans/`.
- Any spec deferral must be a documented, owner-noted Cargo feature or Tier-1 error — never a silent
  drop. Current deferrals: `http3` (feature), HTTP trailers (best-effort), `icu`/crossterm subsets,
  cross-file LSP features.
