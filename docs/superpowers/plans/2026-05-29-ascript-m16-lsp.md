# AScript Milestone 16 — Language Server Implementation Plan (FINAL MILESTONE)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement spec §10's `ascript lsp` — a Language Server (tower-lsp) providing inline diagnostics, document symbols, completion, hover, and go-to-definition, reusing the SAME lexer/parser/AST as the interpreter (no second front-end). After this milestone, EVERYTHING in the spec (§§2–16) is implemented.

**Architecture — the key fact: the LSP does STATIC analysis only (no interpreter).** It lexes/parses documents for diagnostics/symbols/completion/hover/goto-def; it NEVER runs the interpreter. So it entirely avoids the `!Send` `Rc`/`RefCell` runtime: the AST (`ast::Stmt`/`Expr`) is plain `Send` data, the analysis functions are pure (`&str` in → owned `lsp_types` out), and the document store is a `Mutex<HashMap<Url, String>>` (Send+Sync) — satisfying tower-lsp's `LanguageServer: Send + Sync` requirement without any interpreter state. The pure analysis layer (`src/lsp/analysis.rs`) is fully unit-testable; the tower-lsp server (`src/lsp/server.rs`) is a thin async adapter that maps protocol calls onto it. Char-offset spans (the pipeline's unit) convert to LSP `Position{line, character(UTF-16)}` via a `LineIndex` built from the document text.

**Tech Stack:** Rust 2021. New crate (feature `lsp`, default-on): `tower-lsp` (bundles `lsp-types` + `tower` + JSON-RPC over stdio; pulls tokio — already a dep). Reuses `ascript::{lexer, parser, ast, span, error, stdlib}`.

**Starting state (end of M15, on `main`):** 501 tests default (245 `--no-default`), clippy clean. CLI is clap with `Run`/`Repl`/`Fmt`/`Test` subcommands (`src/main.rs`). `ascript::lexer::lex(&str) -> Result<Vec<Token>, AsError>`; `ascript::parser::parse(&[Token]) -> Result<Vec<Stmt>, AsError>`. `AsError { message, span: Option<Span>, source }`; `Span { start, end }` are CHAR offsets. `Stmt` variants: Expr/Let/LetDestructure/Block/If/While/ForRange/ForOf/Return/Break/Continue/Fn/Enum/Class/Import/Export(Box<Stmt>). The full stdlib registry is `crate::stdlib::std_module_exports(path) -> Option<Vec<(String, Value)>>` + the known module paths. Runtime is `#[tokio::main(flavor="current_thread")]`.

**Conventions:** the LSP layer uses owned data + `Send+Sync` types (NO `Rc`/`RefCell`/`Value` held across protocol calls — the analysis returns owned lsp-types). Diagnostics are static (lex + parse errors), since the LSP doesn't execute. Feature-gated (`lsp`); dual-config builds (`--no-default-features` omits the LSP + the subcommand). Pure analysis fns are unit-tested; a protocol smoke test optionally drives the server.

## Capabilities (spec §10: "completion, hover, go-to-def, inline diagnostics")
- **Diagnostics:** on didOpen/didChange, lex+parse the document; a lex or parse `AsError` (with its span) → one LSP `Diagnostic` (Error severity, range from the span, message). No errors → clear diagnostics. (Static only — no runtime/contract diagnostics, since the LSP doesn't run code.)
- **Document symbols:** top-level `fn`/`class`/`enum`/`const`/`let`/`let [..]` (+ through `Export`) → `DocumentSymbol`/`SymbolInformation` with the right `SymbolKind` (Function/Class/Enum/Constant/Variable) and selection range. Class methods → nested symbols (children).
- **Completion:** keywords + global builtins (print/len/type/assert/range/Ok/Err/recover) + stdlib module paths (in an `import ... from "std/…"` string context) + a module's exports (after `alias.` when `alias` was imported via `* as`). Pragmatic context detection; always offer keywords + builtins as a baseline.
- **Hover:** the identifier/keyword under the cursor → markdown info (keyword description, builtin signature, or stdlib function note).
- **Go-to-definition:** the identifier under the cursor → the `Location` of its in-file declaration (fn/class/enum/let/const/param), searching the AST.

## File structure
| File | Responsibility | Change |
|---|---|---|
| `Cargo.toml` | `lsp` feature + `tower-lsp` | modify |
| `src/main.rs` | `Lsp` subcommand → `ascript::lsp::run_server()` | modify |
| `src/lib.rs` | `#[cfg(feature="lsp")] pub mod lsp;` | modify |
| `src/lsp/mod.rs` | `run_server()` + re-exports | create |
| `src/lsp/line_index.rs` | char-offset ↔ `Position{line, utf16 char}` | create |
| `src/lsp/analysis.rs` | PURE analysis: diagnostics/symbols/completions/hover/definition | create |
| `src/lsp/server.rs` | tower-lsp `LanguageServer` impl (thin adapter) + doc store | create |

## Scope & Justified Deferrals
| Deferred | Why | Owner |
|---|---|---|
| Cross-file goto-def / workspace symbol / rename / find-references | Per-document analysis covers the §10 list (completion/hover/goto-def/diagnostics); cross-file needs a module graph + index — out of the §10 capability list | future enhancement (documented) |
| Runtime/contract diagnostics | The LSP doesn't execute code; only lex/parse errors are static | n/a (by design) |
| Incremental sync | Full-document sync (TextDocumentSyncKind::FULL) is simpler + adequate | future (perf) |

Nothing in M16's own (§10) capability scope is deferred.

---

## Task 1: `lsp` feature + CLI subcommand + tower-lsp skeleton + diagnostics

**Files:** `Cargo.toml`, `src/main.rs`, `src/lib.rs`, create `src/lsp/{mod,line_index,analysis,server}.rs`.

- [ ] **Cargo.toml:** `lsp = ["dep:tower-lsp"]` added to `default`; `tower-lsp = { version = "0.20", optional = true }` (verify resolved version + its `lsp_types` re-export; tower-lsp 0.20 re-exports `lsp_types` and provides `LspService`/`Server`. Adapt + report if the API differs). Run `cargo build`.
- [ ] **`src/lib.rs`:** `#[cfg(feature = "lsp")] pub mod lsp;`.
- [ ] **`src/main.rs`:** add `Lsp` to the `Command` enum (`/// Run the language server (LSP over stdio)`), gated so it only exists/dispatches with the feature: `#[cfg(feature="lsp")] Command::Lsp => { ascript::lsp::run_server().await; ExitCode::SUCCESS }`. (If the subcommand must always be in the enum, add it unconditionally but have the handler `#[cfg(not(feature="lsp"))]` print "built without lsp feature". Simplest: gate both — the variant + the arm — with `#[cfg(feature="lsp")]`; clap still compiles. Report which.)
- [ ] **`src/lsp/line_index.rs`:** a `LineIndex` built from `&str` mapping CHAR offset ↔ `lsp_types::Position`. Compute line-start CHAR offsets; `position(char_offset) -> Position{line, character}` where `character` is the **UTF-16 code-unit** count from the line start to the offset (LSP default encoding is UTF-16); `offset(Position) -> char_offset` (reverse, for hover/completion/definition request positions). Unit-test: single line, multi-line, a line with a multibyte char (é → 1 char, 1 utf16 unit) and an astral char (emoji → 1 char but 2 utf16 units) so the column math is right. (Spans are char offsets; AScript source is UTF-8; LSP positions are UTF-16 — get this conversion correct, it's the subtle part.)
- [ ] **`src/lsp/analysis.rs`:** `pub fn diagnostics(text: &str) -> Vec<lsp_types::Diagnostic>`: `lex(text)` → on `Err(e)` produce one diagnostic (range from `e.span` via the LineIndex, or the whole first line if no span; severity Error; message `e.message`; source "ascript"); if lex ok, `parse(&tokens)` → on `Err(e)` likewise; if both ok → empty vec. (A pure fn — unit-testable.)
- [ ] **`src/lsp/server.rs`:** a `Backend { client: tower_lsp::Client, documents: tokio::sync::Mutex<HashMap<Url, String>> }` implementing `tower_lsp::LanguageServer`. `initialize` → advertise capabilities (textDocumentSync FULL, plus completion/hover/definition/documentSymbol provider flags — add the providers as their tasks land; Task 1 just needs sync + diagnostics). `did_open`/`did_change` → store the text, run `analysis::diagnostics`, `client.publish_diagnostics(uri, diags, version)`. `did_close` → drop the doc + clear diagnostics. `shutdown` → Ok. `src/lsp/mod.rs`: `pub async fn run_server()` builds `LspService::new(|client| Backend::new(client))` and `Server::new(stdin, stdout, socket).serve(service).await` over tokio stdin/stdout.
- [ ] **Tests:** unit-test `analysis::diagnostics` (valid program → empty; a lex error like an unterminated string → 1 diagnostic with a plausible range; a parse error like `let = 5` → 1 diagnostic). Unit-test `LineIndex` (the cases above). (The tower-lsp server glue is thin; a full protocol test is optional — Task 4. Focus Task 1 tests on the pure analysis + line index.) 
- [ ] `cargo test` (default) + `cargo test --no-default-features` (lsp cfg's out; `ascript lsp` subcommand absent) + `cargo clippy --all-targets` (both configs) + `cargo build --no-default-features` + `cargo build` (default). Green/clean/compile. Commit `feat: ascript lsp — tower-lsp server + diagnostics (feature lsp)`.

---

## Task 2: Document symbols + hover

**Files:** `src/lsp/analysis.rs`, `src/lsp/server.rs`.

- [ ] **`analysis::document_symbols(text) -> Vec<DocumentSymbol>`** (pure): `parse(text)`; on parse error → empty (diagnostics already report it). Walk the top-level `Stmt`s (unwrapping `Export(inner)`): `Fn{name,...}` → SymbolKind::FUNCTION; `Class{name, methods}` → CLASS with method children (each FUNCTION); `Enum{name, variants}` → ENUM with variant children (ENUM_MEMBER); `Let{name, mutable}`/`LetDestructure{names}` → VARIABLE (const = `!mutable` → CONSTANT). Use the decl's span for the symbol range + name span for selection range (if the AST exposes a name span; else the decl span). **NOTE:** check whether `Stmt`/decl nodes carry spans — if `Stmt` doesn't carry a span, you may need to add one (a span on the relevant AST nodes), OR derive a range from available info. Inspect `ast.rs`; if decls lack spans, ADD a span field to the declaration statements (small AST change — update the parser to populate it + any exhaustive `Stmt` matches). Report what you did.
- [ ] **`analysis::hover(text, offset) -> Option<Hover>`** (pure): find the identifier/keyword at the char offset (tokenize + locate the token spanning the offset, OR walk the AST). If it's a keyword → a markdown description; a global builtin (print/len/type/assert/range/Ok/Err/recover) → its signature + one-line doc; a known stdlib module/function (from the registry) → a note. Return `Hover{contents: Markdown, range}`. Pragmatic — a small built-in table of keyword/builtin docs.
- [ ] **`server.rs`:** implement `document_symbol` → `analysis::document_symbols`; `hover` → `analysis::hover` (convert the request Position → offset via LineIndex of the stored doc). Advertise the providers in `initialize`.
- [ ] Tests: `document_symbols` on a program with a fn, a class (+methods), an enum (+variants), a const, a let → assert the symbol names + kinds + nesting. `hover` at the offset of `print` → contains "print"; at a keyword `fn` → describes it; at a stdlib fn → a note; at whitespace/unknown → None. `cargo test` (both configs) + clippy + commit `feat: lsp document symbols + hover`.

---

## Task 3: Completion + go-to-definition

**Files:** `src/lsp/analysis.rs`, `src/lsp/server.rs`.

- [ ] **`analysis::completions(text, offset) -> Vec<CompletionItem>`** (pure): 
  - Baseline: all keywords (KEYWORD kind) + global builtins (FUNCTION) — always offered.
  - Context: if the offset is inside an `import ... from "std/<partial>"` string (detect via the text around the offset — a simple scan for `from "` / `from '` preceding the cursor in the current statement, or a token-based check), offer the known stdlib module paths (`std/string`, `std/array`, …, the full set — enumerate them; you can derive the list from a `const` array or by probing `std_module_exports` for each known path). 
  - Context: if the offset is right after `<ident>.` where `<ident>` is a `* as` namespace import of a std module, offer that module's exports (from `std_module_exports`). 
  - Keep it pragmatic + robust (never panic; partial/garbage input → at least the baseline). Each item: label + kind + (optional) detail/insertText.
- [ ] **`analysis::definition(text, offset) -> Option<Range>`** (pure): identify the identifier at the offset; search the AST for a declaration binding that name — a top-level `fn`/`class`/`enum`/`let`/`const`, or (if the offset is inside a function) that function's params + local lets. Return the declaration's name range (or decl span). Within-file only (cross-file deferred). If the identifier IS the declaration, return its own location (or None). Pragmatic scope resolution: prefer the nearest enclosing declaration; a simple "collect all top-level decls + the enclosing fn's params/lets, match by name" is acceptable.
- [ ] **`server.rs`:** implement `completion` → `analysis::completions`; `goto_definition` → `analysis::definition` (→ a `Location` with the doc's uri + the range via LineIndex). Advertise providers.
- [ ] Tests: `completions` baseline contains "fn"/"let"/"print"; in an `import from "std/` context contains "std/string"/"std/json"; after `math.` (with `import * as math from "std/math"`) contains "sqrt"/"abs". `definition`: for `fn foo(){}; foo()` → goto from the `foo()` call returns foo's decl range; for a param used in a body → the param; for an unknown ident → None. `cargo test` (both configs) + clippy + commit `feat: lsp completion + go-to-definition`.

---

## Task 4: Protocol smoke test + example/doc + holistic

**Files:** `tests/lsp.rs` (new, gated `#[cfg(feature="lsp")]`), maybe a doc note.

- [ ] **Protocol smoke test** (`tests/lsp.rs`, `#[cfg(feature="lsp")]`): drive the server end-to-end via tower-lsp's test harness OR by spawning the `ascript lsp` binary and speaking LSP JSON-RPC over its stdio: send `initialize` → assert the server returns capabilities (completion/hover/definition/documentSymbol providers + sync); send `textDocument/didOpen` with a doc containing a parse error → assert a `textDocument/publishDiagnostics` notification with 1 diagnostic; (optionally) a `textDocument/documentSymbol` request → assert symbols. (tower-lsp doesn't ship an easy in-process test harness; the robust path is to spawn `CARGO_BIN_EXE_ascript lsp` as a subprocess, write framed `Content-Length`-prefixed JSON-RPC to its stdin, read responses from stdout. Implement a minimal LSP client in the test. If that's too heavy, at MINIMUM keep the thorough unit tests of the analysis layer (Tasks 1-3) AND add a test that `run_server` wiring + `initialize` capability construction is correct via a direct call to the capability-building fn. Report which.)
- [ ] **Doc:** a brief note (code comment in `src/lsp/mod.rs` + a line in the README/roadmap hand-off) on running the LSP (`ascript lsp` over stdio) + the editor-config sketch. No example `.as` file needed (the LSP isn't a language feature).
- [ ] **Conformance:** unaffected (no new `.as` syntax). FINAL verification: `cargo test` (default) + `cargo test --no-default-features` + `cargo clippy --all-targets` (both configs) + `cargo build --no-default-features` + `cargo build` (default) + `cargo run -- lsp` smoke (it should start + wait on stdin — kill it; OR a quick echo of an initialize through it). All green/clean/compile.
- [ ] Commit `test: lsp protocol smoke test + docs`.

---

## Definition of Done

- `cargo test` (default) passes (incl. the lsp analysis unit tests + the protocol/wiring test); `cargo clippy --all-targets` clean; `cargo test --no-default-features` passes + `cargo build --no-default-features` compiles (lsp + the `ascript lsp` subcommand cfg out).
- `ascript lsp` starts a tower-lsp server over stdio providing: inline diagnostics (lex/parse errors), document symbols, completion (keywords/builtins/stdlib), hover, go-to-definition — reusing the interpreter's lexer/parser/AST (no second front-end), per spec §10.
- The LSP is static-analysis-only (no interpreter), so it's `Send+Sync`-clean; the pure analysis layer is unit-tested; char-offset↔UTF-16-Position mapping is correct (incl. multibyte/astral).
- Nothing in M16's §10 capability scope deferred (cross-file/rename/incremental documented as future enhancements).

## ✅✅ AFTER M16: PHASE 2+ COMPLETE — the ENTIRE spec (§§2–16) is implemented. ✅✅
Update `roadmap.md`: mark M16 ✅ and Phase 2 (+ the whole spec) COMPLETE. The AScript language (§§2–9), tooling (§10: run/repl/fmt/test/**lsp** + diagnostics + the conformance-tested Tree-sitter grammar), and the full standard library (§11: data/text, time/locale, system, async I/O incl. the modern §11.5 HTTP client, terminal UI) are all implemented, unit- and example-tested, clippy-clean across both feature configs, and merged to `main` — with the §11.5 deferrals (HTTP/3 feature-gated, trailers best-effort, SOCKS feature) and the documented pragmatic subsets (icu, ratatui) the only non-default-on items, all justified + owner-noted. Nothing else remains.
