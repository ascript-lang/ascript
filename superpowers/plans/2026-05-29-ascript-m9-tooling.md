# AScript Milestone 9 — Tooling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Ship the `ascript` developer tooling (spec §10): a multi-command CLI, rich source-pointing diagnostics (ariadne), a REPL, `ascript fmt` (canonical formatter), `ascript test` (built-in test runner), and a Tree-sitter grammar that generates cleanly + a two-parser conformance test (spec §10.1).

**Architecture:** Restructure `main.rs` into a `clap`-based multi-command binary (`run`/`repl`/`fmt`/`test`). Errors gain optional source context (path + text) attached by the module loader, rendered with `ariadne` for caret/span diagnostics. The REPL (`rustyline`) keeps one environment across lines and uses `recover`-style panic isolation. `fmt` is an AST pretty-printer (idempotent). `test` collects `test("name", fn)` registrations and runs them, catching panics as failures. The Tree-sitter grammar is reconciled with the implemented language, generated via `tree-sitter generate`, compiled in `build.rs`, and conformance-tested against the interpreter parser over the example corpus.

**Tech Stack:** Rust 2021, tokio (current_thread), async-recursion, indexmap. **New deps:** `clap` (derive), `ariadne`, `rustyline`; **dev/build:** `tree-sitter` (+ `cc` build-dep). Toolchain confirmed present: node v22, tree-sitter 0.26.9, clang.

**Starting state (end of M8, on `main`):** Full language + modules. `main.rs` hand-parses `run <file>` and calls `run_file`. `AsError { message, span }`. 120 lib + 9 cli + 4 module tests.

**Conventions:** spans char offsets; single-threaded; `Control` error channel; failed contracts/bad ops panic.

## Scope & Justified Deferrals

| Deferred | Why | Milestone |
|---|---|---|
| Language Server (LSP, `tower-lsp`) | Large standalone subsystem; spec lists it separately | **M-LSP (final tooling milestone)** |
| `std/*` built-in modules used by `import` | Stdlib doesn't exist yet | **Phase 2 stdlib** |

(The REPL, fmt, test runner, diagnostics, and Tree-sitter conformance are all delivered here. The LSP is a separate, large milestone after the stdlib — it reuses the same front-end.)

---

## Task 1: clap CLI + ariadne diagnostics

**Files:** `Cargo.toml`, `src/error.rs`, `src/interp.rs`, `src/lib.rs`, `src/main.rs`, `src/diagnostics.rs` (new).

- [ ] **Step 1: `Cargo.toml`** — add deps: `clap = { version = "4", features = ["derive"] }`, `ariadne = "0.6"`.

- [ ] **Step 2: `src/error.rs`** — give `AsError` optional source context so multi-file errors can render. Add a field and keep constructors working:
```rust
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub path: String,
    pub text: String,
}

#[derive(Debug)]
pub struct AsError {
    pub message: String,
    pub span: Option<Span>,
    pub source: Option<std::rc::Rc<SourceInfo>>,
}
```
Update `new`/`at` to set `source: None`. Add `pub fn with_source(mut self, src: Rc<SourceInfo>) -> Self { if self.source.is_none() { self.source = Some(src); } self }`. The `Display` impl stays the same (message + span offsets) — ariadne rendering is separate.

- [ ] **Step 3: `src/interp.rs`** — attach source in `load_module`. When the module body exec returns `Err(Control::Panic(e))`, enrich it: `e.with_source(Rc::new(SourceInfo { path: canon.display().to_string(), text: src.clone() }))` (only if not already set — so the innermost module's source wins). Wrap the `result?` accordingly:
```rust
        let result = self.exec(&program, &env).await;
        self.module_dir = prev_dir;
        self.current_exports = prev_exports;
        if let Err(Control::Panic(e)) = result {
            let src_info = std::rc::Rc::new(crate::error::SourceInfo { path: canon.display().to_string(), text: src });
            return Err(Control::Panic(e.with_source(src_info)));
        }
        result?;
        Ok(entry)
```
Also attach source for lex/parse errors of the module (they have spans into `src`): map their `AsError` through `.with_source(...)` before returning.

`run_source` (string eval) should similarly attach a `SourceInfo { path: "<input>", text: src }` to a panicking error so REPL/string errors render.

- [ ] **Step 4: `src/diagnostics.rs` (new)** — render an `AsError` with ariadne when it has source + span, else plain:
```rust
//! Pretty diagnostic rendering via ariadne.
use crate::error::AsError;

/// Render an error to stderr — a source-pointing report if span+source are
/// present, otherwise a plain `error: <message>` line.
pub fn report(err: &AsError) {
    use ariadne::{Color, Label, Report, ReportKind, Source};
    match (&err.source, err.span) {
        (Some(src), Some(span)) => {
            // Spans are CHAR offsets; ariadne wants byte ranges. Convert.
            let text = &src.text;
            let start = char_to_byte(text, span.start);
            let end = char_to_byte(text, span.end);
            let path = src.path.as_str();
            let _ = Report::build(ReportKind::Error, (path, start..end))
                .with_message(&err.message)
                .with_label(
                    Label::new((path, start..end))
                        .with_message(&err.message)
                        .with_color(Color::Red),
                )
                .finish()
                .eprint((path, Source::from(text.as_str())));
        }
        _ => eprintln!("error: {}", err),
    }
}

/// Convert a char offset into a byte offset within `text`.
fn char_to_byte(text: &str, char_off: usize) -> usize {
    text.char_indices().nth(char_off).map(|(b, _)| b).unwrap_or(text.len())
}
```

- [ ] **Step 5: `src/lib.rs`** — expose `pub mod diagnostics;`. Keep `run_file`/`run_source` returning `Result<String, AsError>` (the enriched error flows out).

- [ ] **Step 6: `src/main.rs`** — replace hand-rolled arg parsing with clap subcommands. Only `run` is wired now (`repl`/`fmt`/`test` are added in later tasks — define them as subcommands that print "not yet implemented" OR add them incrementally; CLEANEST: define the full `Command` enum now with `run`, and stub the others to a clear message, replaced in their tasks). On a run error, call `diagnostics::report(&e)` and exit 1.
```rust
use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "ascript", about = "The AScript interpreter")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a .as program
    Run { file: String },
    /// Start the interactive REPL
    Repl,
    /// Format .as source files
    Fmt { files: Vec<String> },
    /// Run .as test files
    Test { files: Vec<String> },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { file } => match ascript::run_file(std::path::Path::new(&file)).await {
            Ok(output) => { print!("{}", output); ExitCode::SUCCESS }
            Err(e) => { ascript::diagnostics::report(&e); ExitCode::from(1) }
        },
        Command::Repl => { eprintln!("repl: implemented in a later step"); ExitCode::from(1) }
        Command::Fmt { .. } => { eprintln!("fmt: implemented in a later step"); ExitCode::from(1) }
        Command::Test { .. } => { eprintln!("test: implemented in a later step"); ExitCode::from(1) }
    }
}
```

- [ ] **Step 7: Update `tests/cli.rs`** — the binary now requires the `run` SUBCOMMAND (`ascript run <file>` still works — that's unchanged). The `reports_usage_without_args` test: with clap, no args → clap prints usage to stderr and exits 2. Update that test to assert exit is unsuccessful and stderr mentions usage (clap's output contains "Usage"). Adjust the assertion to match clap's behavior (e.g. `!status.success()` and stderr contains "Usage" or "ascript").

- [ ] **Step 8: Tests** — add a diagnostics integration test in `tests/cli.rs`:
```rust
#[test]
fn run_error_shows_source_caret() {
    let file = std::env::temp_dir().join(format!("ascript_diag_{}.as", std::process::id()));
    std::fs::write(&file, "let x = 1\nprint(missing)\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = std::process::Command::new(bin).arg("run").arg(&file).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    // ariadne renders the message and points at the source
    assert!(err.contains("undefined variable 'missing'"));
}
```

- [ ] **Step 9: Run** `cargo test` + `cargo clippy --all-targets`. (Confirm `ascript run examples/oop.as` still works and an error renders with a caret.) **Commit:** `feat: clap CLI with subcommands and ariadne diagnostics`

---

## Task 2: REPL (`ascript repl`)

**Files:** `Cargo.toml`, `src/repl.rs` (new), `src/lib.rs`, `src/main.rs`.

- [ ] **Step 1: `Cargo.toml`** — add `rustyline = "14"`.

- [ ] **Step 2: `src/repl.rs` (new)** — an interactive loop that keeps ONE `Interp` + one module `Environment` across inputs, evaluates each line as statements, prints the value of a trailing expression, and isolates panics (a panic prints the error but does NOT exit the REPL).

Key design:
- Create `let mut interp = Interp::new();` and `let env = interp::global_env();` once.
- Per input line: lex+parse to `Vec<Stmt>`. If the LAST stmt is an `Stmt::Expr`, exec the preceding stmts then eval the last expr and print its value (if not nil). Else exec all.
- Wrap evaluation so a `Control::Panic` is caught and reported via `diagnostics::report` (with the line as source) WITHOUT exiting; then continue the loop.
- Use `rustyline::DefaultEditor` for line editing + history. `Ctrl-D`/`Ctrl-C` exits cleanly.
- Provide `pub async fn run_repl() -> std::io::Result<()>`.

Provide complete code (the implementer writes the rustyline loop following this spec; multi-line input may be handled simply by treating each line independently — if parse fails with "unexpected Eof", optionally accumulate lines; a basic single-line REPL is acceptable for v1, but accumulate-on-incomplete is preferred).

- [ ] **Step 3: `src/lib.rs`** — `pub mod repl;` and re-export `run_repl` (or call via `ascript::repl::run_repl`).

- [ ] **Step 4: `src/main.rs`** — wire `Command::Repl => ascript::repl::run_repl().await ...`.

- [ ] **Step 5: Tests** — REPL is interactive; add a test that pipes input via stdin to the binary:
```rust
#[test]
fn repl_evaluates_and_persists_bindings() {
    use std::process::{Command, Stdio};
    use std::io::Write;
    let bin = env!("CARGO_BIN_EXE_ascript");
    let mut child = Command::new(bin).arg("repl")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"let x = 21\nx * 2\n").unwrap();
    let out = child.wait_with_output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("42"));
}
```
(If rustyline refuses a non-tty stdin, the REPL must fall back to reading lines from stdin directly — implement a tty/non-tty path so the piped test works. Detect with `std::io::IsTerminal`.)

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`. **Commit:** `feat: add interactive REPL`

---

## Task 3: `ascript fmt` (formatter)

**Files:** `src/fmt.rs` (new), `src/lib.rs`, `src/main.rs`, plus tests.

- [ ] **Step 1: `src/fmt.rs` (new)** — an AST pretty-printer producing canonical source. `pub fn format_source(src: &str) -> Result<String, AsError>`: lex → parse → render the `Vec<Stmt>` back to formatted source.

Formatting rules (canonical):
- 2-space indentation; blocks `{ … }` on their own lines.
- One statement per line; no trailing semicolons.
- `let`/`const NAME[: type] = expr`; `fn name(params) [: ret] { … }`; `if (cond) { … } else { … }`; `while`/`for`; `class`/`enum`; `import`/`export`.
- Binary ops spaced (`a + b`); calls `f(a, b)`; arrays/objects `[1, 2]` / `{a: 1, b: 2}`; member `a.b`; index `a[i]`.
- Idempotent: `format(format(x)) == format(x)`.

This is a recursive `write_stmt`/`write_expr` over the AST with an indent level. Implementer writes the printer following these rules. Expression rendering can largely reuse the precedence structure (parenthesize only where needed — for v1, a correct-but-sometimes-over-parenthesized output is acceptable as long as it's idempotent and re-parses to the same AST).

- [ ] **Step 2: `src/lib.rs`** — `pub mod fmt;`.

- [ ] **Step 3: `src/main.rs`** — `Command::Fmt { files }`: for each file, read, `format_source`, and write back (or print to stdout if no files / a `--check`-less v1 just rewrites in place and prints the path). Keep it simple: format each file in place; print "formatted <path>".

- [ ] **Step 4: Tests** (in `src/fmt.rs`):
```rust
    #[test]
    fn formats_and_is_idempotent() {
        let src = "let   x=1+2\nfn f(a,b){return a+b}\nif(x>2){print(\"big\")}else{print(\"small\")}";
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice, "fmt must be idempotent");
        // re-parses to an equivalent program (no parse error)
        assert!(crate::parser::parse(&crate::lexer::lex(&once).unwrap()).is_ok());
    }

    #[test]
    fn formats_canonically() {
        let out = format_source("let x=1").unwrap();
        assert_eq!(out, "let x = 1\n");
    }
```

- [ ] **Step 5: Run** `cargo test` + `cargo clippy --all-targets`. **Commit:** `feat: add ascript fmt formatter`

---

## Task 4: `ascript test` (test runner)

**Files:** `src/interp.rs` (a `test` builtin + collection), `src/testrunner.rs` (new) or in `lib.rs`, `src/main.rs`, plus tests.

- [ ] **Step 1: Define the test API.** A built-in `test(name: string, fn: fn)` registers a test. In normal `run`, `test(...)` registers but does nothing (or runs nothing); in `ascript test`, the runner executes each registered test, catching panics as failures.

Implementation: add a `tests: Vec<(String, Value)>` field to `Interp` (registered tests). The `test` builtin pushes `(name, fn)`. Register `"test"` in `global_env()`. Add a method `Interp::run_registered_tests(&mut self) -> (usize passed, usize failed, Vec<String> failures)` that calls each fn (via `call_value`) catching `Control::Panic` as a failure.

- [ ] **Step 2: `src/lib.rs`** — `pub async fn run_tests(files: &[String]) -> Result<TestSummary, AsError>`: a fresh `Interp`; load each file (which runs `test(...)` registrations); then run registered tests; return a summary `{ passed, failed, failures: Vec<String> }`.

- [ ] **Step 3: `src/main.rs`** — `Command::Test { files }`: call `run_tests`, print `ok. N passed; M failed` (and each failure's name + message); exit 1 if any failed.

- [ ] **Step 4: Tests** — an integration test with a temp test file:
```rust
#[test]
fn test_runner_reports_pass_and_fail() {
    let file = std::env::temp_dir().join(format!("ascript_tr_{}.as", std::process::id()));
    std::fs::write(&file,
        "test(\"adds\", () => { assert(1 + 1 == 2) })\ntest(\"fails\", () => { assert(false, \"boom\") })").unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let out = std::process::Command::new(bin).arg("test").arg(&file).output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout) + &String::from_utf8_lossy(&out.stderr);
    assert!(s.contains("1 passed") || s.contains("passed"));
    assert!(s.contains("fails") && s.contains("boom"));
    assert!(!out.status.success()); // a failing test → non-zero exit
}
```

- [ ] **Step 5: Run** `cargo test` + `cargo clippy --all-targets`. **Commit:** `feat: add ascript test runner with a test() builtin`

---

## Task 5: Tree-sitter grammar reconciliation + conformance test

The committed `grammar.js` predates M2–M8 and has generate-time conflicts. Reconcile it with the implemented language, generate the parser, compile it, and conformance-test it against the interpreter parser over the example corpus.

**Files:** `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` (fix), generated `src/parser.c` (committed under the grammar dir), `build.rs` (new, at crate root), `Cargo.toml` (tree-sitter dep + build-dep), `tests/treesitter_conformance.rs` (new).

- [ ] **Step 1: Reconcile + fix `grammar.js`** so `tree-sitter generate` succeeds with NO conflicts, and so it accepts the SAME programs the interpreter accepts. Run `tree-sitter generate` (from the grammar dir) iteratively; resolve every conflict (e.g. the `return` statement vs block conflict → add a `conflicts` entry or make the return value optional with the right precedence; the object-literal vs block ambiguity; arrow vs parenthesized expr; `match`; `?.`/`??`/`?`; templates). Update the grammar to cover the ACTUAL implemented surface: classes (`extends`), enums, `match`, optional chaining, the `?` operator, template strings, modules (`import`/`export`), type annotations, comments. Verify it parses every file in `examples/` (and the corpus below) without ERROR nodes.

- [ ] **Step 2: Generate + commit the parser.** Run `tree-sitter generate` in the grammar dir; commit the generated `src/parser.c` (and `src/tree_sitter/*.h`, `grammar.json`, `node-types.json`) so the build doesn't require the tree-sitter CLI.

- [ ] **Step 3: `Cargo.toml` + `build.rs`** — add `tree-sitter = "0.24"` (match a version compatible with the generated ABI) as a [dependency], and `cc = "1"` as a [build-dependencies]. `build.rs` compiles the generated `parser.c`:
```rust
fn main() {
    let dir = "docs/superpowers/specs/grammar/tree-sitter-ascript/src";
    println!("cargo:rerun-if-changed={}/parser.c", dir);
    cc::Build::new().include(dir).file(format!("{}/parser.c", dir)).warnings(false).compile("tree_sitter_ascript");
}
```

- [ ] **Step 4: `tests/treesitter_conformance.rs` (new)** — load the generated parser and assert it parses the example corpus + golden snippets WITHOUT error nodes, agreeing with the interpreter parser (both accept):
```rust
use tree_sitter::Parser;

extern "C" { fn tree_sitter_ascript() -> tree_sitter::Language; }

fn ts_parses_clean(src: &str) -> bool {
    let mut p = Parser::new();
    p.set_language(&unsafe { tree_sitter_ascript() }).unwrap();
    let tree = p.parse(src, None).unwrap();
    !tree.root_node().has_error()
}

#[test]
fn tree_sitter_accepts_all_examples() {
    for entry in std::fs::read_dir("examples").unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("as") {
            let src = std::fs::read_to_string(&path).unwrap();
            // interpreter parser accepts it (it's a committed example)
            assert!(ascript::parser::parse(&ascript::lexer::lex(&src).unwrap()).is_ok(), "interp rejects {:?}", path);
            // tree-sitter parses it with no error nodes
            assert!(ts_parses_clean(&src), "tree-sitter has errors on {:?}", path);
        }
    }
    // also the nested example dir
    for sub in ["examples/modules/main.as", "examples/modules/geometry.as"] {
        let src = std::fs::read_to_string(sub).unwrap();
        assert!(ts_parses_clean(&src), "tree-sitter errors on {}", sub);
    }
}
```
(Adjust the `tree-sitter` crate API calls to the exact version's signatures.)

- [ ] **Step 5: Run** `cargo test` (incl. the conformance test) + `cargo clippy --all-targets`. If the generated parser's ABI mismatches the `tree-sitter` crate version, align versions. **Commit:** `feat: reconcile and generate Tree-sitter grammar with conformance test`

---

## Task 6: End-to-end tooling verification

**Files:** `tests/cli.rs` (a few assertions tying it together), `docs/` note.

- [ ] **Step 1:** Add CLI integration assertions (if not already covered): `ascript run` renders a caret error; `ascript fmt` rewrites a messy file and is idempotent on the result; `ascript test` reports pass/fail with correct exit code; `ascript repl` evaluates piped input. (Several of these exist from earlier tasks — ensure the full set is green.)

- [ ] **Step 2: Run** the FULL suite `cargo test` + `cargo clippy --all-targets`. Run each subcommand manually once and paste output: `ascript run examples/oop.as`, `ascript fmt` on a temp file, `ascript test` on a temp test file, and a piped `ascript repl`.

- [ ] **Step 3: Commit:** `test: end-to-end tooling integration coverage`

---

## Task 7: `async`/`await` surface syntax (spec §7)

Close the only genuine §§2–10 language gap and fix the grammar↔interpreter lockstep (the Tree-sitter grammar already parses `async`/`await`; the interpreter must too). The async *runtime seam* already exists (`eval_expr` is `#[async_recursion(?Send)]` on a current-thread tokio runtime). This task adds the SURFACE: `async fn`/`async (…) =>`/`async fn` methods, and the `await` prefix operator. Semantics per spec §7: synchronous code never suspends, so with no awaitable/future value kind yet (those arrive with the async I/O stdlib in Phase 3), `await expr` evaluates `expr` and returns it (identity for non-futures), and an `async fn` simply runs to completion. The syntax is fully accepted and composes, ready for the async stdlib to introduce real suspension points.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/value.rs`, `src/interp.rs`, `src/fmt.rs`, plus tests + an example.

- [ ] **Step 1: `src/token.rs`** — add `Async,` and `Await,` before `Eof`. **Lexer:** keywords `"async" => Tok::Async, "await" => Tok::Await,`.

- [ ] **Step 2: `src/ast.rs`** — add `is_async: bool` to `Stmt::Fn`, `MethodDecl`, and `ExprKind::Arrow`; add `ExprKind::Await(Box<Expr>)`. Display: `ExprKind::Await(e) => write!(f, "(await {})", e)`. (Function/method Display, if any, may show `async`.)

- [ ] **Step 3: `src/value.rs`** — add `is_async: bool` to `Function` and `Method` (carried for fidelity/fmt; does not change execution since nothing suspends without futures).

- [ ] **Step 4: `src/parser.rs`** —
  - `statement`: if `peek == Tok::Async`, expect `fn` next → parse an async `fn_decl` (set `is_async: true`). `fn_decl` takes/derives `is_async`.
  - `class_decl` method parsing: optional `async` before `fn`.
  - `try_arrow`/arrow parsing: optional leading `async` (`async (a) => …`, `async x => …`) → `is_async: true`. Non-async fn/arrow set `is_async: false`.
  - `await`: a prefix operator at unary precedence — in `unary()` (alongside `-`/`!`), if `peek == Tok::Await`, consume and parse a `unary()` operand, wrap in `ExprKind::Await`. So `await f()` parses as `Await(Call(f))` and `await a + b` as `(await a) + b`.

- [ ] **Step 5: `src/interp.rs`** — `ExprKind::Await(e) => self.eval_expr(e, env).await` (identity passthrough — no futures to suspend on yet; correct per §7 for synchronous code). `is_async` flags require no behavioral change (the evaluator is already async; functions run to completion). Update the `Stmt::Fn`/`Arrow`/method construction sites to pass `is_async` into the `Function`/`Method`.

- [ ] **Step 6: `src/fmt.rs`** — prefix `async ` for async fn declarations/methods/arrows; render `await expr` as `await ` + the operand. Keep idempotent.

- [ ] **Step 7: Tests** (interp):
```rust
    #[tokio::test]
    async fn async_fn_and_await_surface() {
        let src = "async fn fetch(x) { return x * 2 }\nlet r = await fetch(21)\nprint(r)\nprint(await 5)\nlet g = async (n) => n + 1\nprint(await g(9))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "42\n5\n10\n");
    }
```
Add a parser test that `async fn`/`await`/`async () =>` parse. Add `examples/async.as` exercising `async fn` + `await` and an integration test, AND ensure the Tree-sitter conformance corpus includes it (so both parsers are exercised on async — fixing the §10.1 lockstep). Confirm `tree-sitter parse examples/async.as` has 0 ERROR nodes (the grammar already supports async); if the grammar needs a tweak, regenerate + recommit parser.c.

- [ ] **Step 8: Run** `cargo test` (incl. the new tests + conformance over the async example) + `cargo clippy --all-targets`. **Commit:** `feat: add async/await surface syntax (spec §7)`

---

## Definition of Done

- `cargo test` passes (unit + integration + module + conformance); `cargo clippy --all-targets` clean.
- `ascript run` shows source-pointing ariadne diagnostics on errors.
- `ascript repl` evaluates input, persists bindings, isolates panics.
- `ascript fmt` produces canonical, idempotent formatting.
- `ascript test` runs `test("name", fn)` registrations, reports pass/fail, exits non-zero on failure.
- The Tree-sitter grammar generates cleanly and parses the entire example corpus with no error nodes (conformance test green).

## Hand-off to Phase 2 (Standard library)

The language + tooling are complete. Phase 2 builds the stdlib as `std/*` built-in modules: the `std/` resolution hook in `resolve_import`/`load_module` dispatches to a registry of native modules. First collections milestone adds the `Map` value kind (`map<K,V>` type) and core modules (`std/string`, `std/array`, `std/object`, `std/map`, `std/math`, `std/convert`), then serialization, time/date/intl, system, async I/O, terminal UI, and finally the LSP (`ascript lsp`) reusing this front-end + the conformance-tested grammar.
