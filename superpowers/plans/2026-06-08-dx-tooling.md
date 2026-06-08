# DX — Doc-gen, Test Framework, LSP Completion & Diagnostics — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; reviewer runs the commands and probes edges). Steps use `- [ ]`. DX is **continuous
> infrastructure** — four independently-landable sub-deliverables (D1–D4); each task group can ship on
> its own. Format mirrors the NUM template.

**Spec:** `superpowers/specs/2026-06-08-dx-tooling-design.md`. **Branch:** `feat/dx-tooling` off `main`.
**Depends on:** nothing hard (stands up alongside NUM). *Fed by* TYPE (inlay/hover types — D3.3) and by
every construct spec (completion/symbol/token items land in those PRs, Gate 8). **Builds on** the shipped
`src/worker/` isolate pool (D2 parallel). **Not breaking** — additive CLI flags, a new `ascript doc`
subcommand, a `///` doc-comment convention that is pure CST-trivia reinterpretation (no grammar change,
no new `SyntaxKind`, no tree-sitter regen, no `.aso`/`ASO_FORMAT_VERSION` bump).

**Architecture:** D1 = a new feature-gated `src/doc/` module (CST walk → doc model → HTML/Markdown), a
`Doc {…}` `main.rs` subcommand, and a shared `///`-run trivia extractor in `src/syntax` reused by the LSP
hover. D2 = `ascript test` flags (`--parallel/--update-snapshots/--coverage/--watch/--filter`), parallel
test-FILE dispatch over `src/worker/` (each isolate encodes its `TestSummary` as a `Value::Object` across
the airlock and the parent decodes it back; deterministic input-order aggregation), snapshot completion,
line coverage on the **unified `Vm.instrument` seam** (coordinated with DBG, cross-cutting #6; Gate-12
coverage-off bench), watch, structural diffs, new assertions, filter. D3 = a NEW file-qualified
`GlobalBindingId` ((FileId, TextRange) for locals/upvalues; (definer-FileId, exported-name) for globals)
on a net-new `FileId` interner, replacing the name-coarse `collect_uses` join so cross-file refs/rename
share one identity with in-file navigation; frame-precise + member completion; richer inlay. D4 =
`report_all(&[AsError])` multi-error, a shared `suggest::closest` did-you-mean, and the SP1 1-column
caret-offset audit/fix. DX owns the campaign-wide README/landing scripting→general-purpose **prose
repositioning** (each construct spec owns its own page). Engines: **none changed** — `vm_differential` is
untouched; the coverage hook is `None`-gated and never observed by program output.

**Tech stack:** Rust; `src/doc/` (new, gated like `lsp`/`pkg`); `src/main.rs`; `src/syntax/` (trivia
extractor); `src/lib.rs` (`run_tests*` parallel path); `src/worker/{pool,dispatch,serialize}.rs`;
`src/vm/{run,chunk}.rs` (instrument seam); `src/stdlib/assert_mod.rs`; `src/lsp/workspace.rs` +
`src/lsp/providers/{navigation,completion,inlay,docs,hover,lens,code_action}.rs`; `src/diagnostics.rs`;
`src/check` (`suggest::closest`); `src/lsp/convert.rs`; `docs/`, `README.md`, `examples/advanced/`.

---

## Shared API Contract (pinned to current code)

**Existing (verified):**
- `Command` enum `src/main.rs:15` (`Run`/`Build`/`Repl`/`Fmt`/`Check`/`Test`/`Lsp` + pkg — **no `Doc`**);
  `Command::Test { files, locked }` `src/main.rs:484`.
- `TestSummary { passed, failed, failures }` `src/interp.rs:780-784` (a plain Rust struct — NOT `Value`,
  not `Serialize`); `run_registered_tests` `src/interp.rs:2269`; `test` registration `src/interp.rs:4705`;
  `BUILTIN_NAMES` `src/interp.rs:132`.
- `run_tests`/`run_tests_with_packages` `src/lib.rs:125`/`:131` (serial today; calls
  `interp.run_registered_tests()` `:155`).
- Worker airlock: `check_sendable` `src/worker/serialize.rs:103`, `encode(v: &Value)` `:360`,
  `decode(bytes, interp)` `:517`; pool cap `$ASCRIPT_WORKERS`/`num_cpus` `src/worker/pool.rs:46-50`;
  `pool::dispatch` `:113`, graceful inline-degrade `:85`/`:112`, `in_isolate()` `:128`; dispatch
  code-slice machinery `src/worker/dispatch.rs`.
- `assert.snapshot` `src/stdlib/assert_mod.rs:36` (`snapshot_impl(dir,name,serialized,update)`,
  `__snapshots__/` store `:54`, mismatch diff `:63`, registered `:99`); `deep_equal` import `:20`.
- `Vm { specialize }` `src/vm/run.rs:51`/`:104` (the `None`/bool kill-switch pattern to mirror);
  `with_specialize` `:175`; the hot loop `run` `src/vm/run.rs:558`, `loop {` `:582`, op fetch `:591`,
  `match op` `:598`.
- `Chunk` code-offset→`Span` table (`spans`, header `src/vm/chunk.rs:3-5`); `Span { start, end }` byte
  interval `src/span.rs`; binary-search `Chunk::span_at` (spec §6.3 cites `chunk.rs:635`). **No line
  table exists** — derive line from the byte `Span` start (shared with DBG, cross-cutting #6).
- LSP: `WorkspaceIndex { files: HashMap<PathBuf, FileIndex> }` `src/lsp/workspace.rs:95-97`;
  `FileIndex.exports` `:80`, `.imports: Vec<ImportEdge>` `:86`; `ResolvedTarget::{LocalDef,Imported,…}`
  `:44`; `ImportEdge` `:68`; `importers` reverse edges `:103`; `decl_kind` walk `:823`; `collect_uses`
  name-walk `:904` (the join to REPLACE). `BindingId::Local(TextRange)|Global(String)`
  `src/lsp/providers/navigation.rs:72-73` (per-file — stays file-local); `Resolution::{Local,Upvalue,
  Global,Unresolved}` `navigation.rs:61-63`. `infer::hover_type_at(src, byte_offset)`
  `src/check/infer/mod.rs:37`; inlay uses it `src/lsp/providers/inlay.rs:55`. `LineComment`/`Newline`/
  `Whitespace` trivia kinds `src/syntax/kind.rs:95-97`, `is_trivia` `:263`. Run-test/run/ref-count
  code lenses already exist `src/lsp/providers/lens.rs:1-27` (callee=="test" `:27`).
- Diagnostics: single-report `report(err)` `src/diagnostics.rs:6` (one ariadne label `:21`);
  `char_to_byte` `src/diagnostics.rs:36`; LSP span conversion `byte_to_char`/`char_to_byte`/
  `byte_span_to_range` `src/lsp/convert.rs:13`/`:23`/`:31`.
- `docs/assets/app.js` `NAV` array `:11` (sidebar + cmd-K derive from it); `docs/index.html` landing;
  `README.md`; `examples/advanced/*.as` (no DX artifact yet).
- Feature set `Cargo.toml:107`; `lsp` gate `:161`, `pkg` gate `:119`, `sys` gate `:138`.

**New names (do not rename):** `src/doc/` module; `Command::Doc { paths, out, format, private, open,
check }`; `syntax`-level `doc_comment_run(decl) -> Option<DocComment>` trivia extractor; `Vm.instrument:
Option<Instrument>` with `enum Instrument { Coverage(CoverageTable) /* DBG adds Debug/Profile */ }` and
`CoverageTable` keyed by `(chunk_id, line)`; `FileId(u32)` + a `PathBuf→FileId` interner on
`WorkspaceIndex`; `GlobalBindingId::{Local(FileId, TextRange), Global(FileId /*definer*/, String)}`;
`diagnostics::report_all(&[AsError])`; `check::suggest::closest(name, &candidates) -> Option<&str>`;
`assert.{matches,deepEq,throwsWith}`.

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH configs (`--all-targets`
  and `--no-default-features --all-targets`).
- **No engine surface:** DX adds no opcode, no `Value` kind, no `.aso` field — `vm_differential` must be
  **unchanged** (Gate 1) and `examples/**` must still emit **zero** `type-*` (Gate 5). A `--coverage` run
  and a normal run produce byte-identical *program* output (the hook is observation-only).
- No `await` across a `RefCell`/resource borrow (Gate 4). `src/doc/` + LSP + diagnostics are **static**
  (CST + checker, never the interpreter) — same `Send`-able runtime-free posture as the LSP.
- New `src/doc/` is feature-gated (mirror `lsp`/`pkg`) so `--no-default-features` need not build it.

---

# D1 — `ascript doc` + the `///` doc-comment convention

## Task 1 — The `///`-run trivia extractor (shared `syntax` addition)
**Files:** `src/syntax/` (new `doc_comment.rs` or in an existing module beside the resolver). **Tests:** inline.
- [ ] Failing tests for the LOCKED attachment rule (spec §2): a contiguous run of `///` `LineComment`
  trivia (`kind.rs:97`) immediately preceding a decl attaches as its doc; **a blank line breaks it**
  where "blank line" = **≥2 consecutive `Newline` trivia** (`kind.rs:96`), intervening `Whitespace`
  indentation ignored; `////` (≥4 slashes) is an ordinary comment, never doc; one leading space after
  `/// ` stripped; first paragraph = summary. Cases: `/// a`⏎`/// b`⏎`fn f` → both; `/// a`⏎⏎`fn f` →
  none; `//! …` at file/block top → module/inner doc.
- [ ] Implement `doc_comment_run(decl_node) -> Option<DocComment>` walking CST leading trivia backward
  from the decl's first token (no re-tokenize, no new `SyntaxKind`). Recognize optional structured tags
  (`@param`/`@returns`/`@example`/`@deprecated`/`@see`) as a Markdown overlay (undocumented `@foo`
  renders literally). Pure over `&str`/byte ranges — `Send`-able, no interpreter.
- [ ] Green both configs; clippy. Review (greps that no `SyntaxKind` was added; confirms blank-line =
  2×`Newline` boundary; confirms `////` exclusion). Commit.

## Task 2 — `src/doc/` doc model + CST extraction
**Files:** `src/doc/mod.rs` (+ submodules); `Cargo.toml` (feature gate). **Tests:** `src/doc/` inline.
- [ ] Failing tests: the doc model captures the public-API decl kinds (spec §3.2) — functions (incl.
  `async`/`worker`/`static`/`fn*` modifiers), classes (fields w/ types+defaults, `init`, methods,
  `static`, inheritance), enums (variants), constants — each with its `///` doc (Task 1) and its
  signature from CST node text. "Public" = exported per `FileIndex.exports` (`workspace.rs:80`); reuses
  the `decl_kind` walk (`workspace.rs:823`) and the `symbols`/`docs` provider machinery — **no second
  parse** (spec §6 rejected). `--private` includes non-exported.
- [ ] Add the feature `doc = [...]` (mirror `lsp`) to `Cargo.toml:107`/`:161`; declare `#[cfg(feature)]
  pub mod doc;`. Path discovery reuses `workspace::discover_as_files` (resolves imports for a whole
  project). Signature rendering = CST node text as written; **where unannotated AND TYPE has landed**,
  append the inferred type via `infer::hover_type_at` (`src/check/infer/mod.rs:37`) in a distinct style
  (spec §3.3) — before TYPE only declared annotations render (no inference column, guarded so it is a
  no-op pre-TYPE).
- [ ] Green both configs; clippy. Review (confirms no second parse; reuses `symbols`/`docs`; static —
  never instantiates `Interp`). Commit.

## Task 3 — `ascript doc` CLI subcommand + HTML/Markdown emitters
**Files:** `src/main.rs`, `src/doc/{html,markdown}.rs`. **Tests:** `tests/cli.rs` (golden).
- [ ] Failing tests: `ascript doc [PATHS] [--out DIR] [--format html|md] [--private] [--open] [--check]`
  (spec §3.1). `--format md` over the documented module → a **pinned Markdown golden** (signatures, `///`
  bodies, cross-links); `--format html` → index-structure golden reusing `docs/assets/styles.css` + a
  cmd-K-style self-contained index (spec §3.4); default `--out target/doc/`. `--check` exits **non-zero**
  on a deliberately-undocumented public symbol (reports the symbols), zero when all documented.
- [ ] Add `Command::Doc { paths, out, format, private, open, check }` to `src/main.rs:15` + dispatch
  (mirror the `Check` arm `:342`). HTML/Markdown emitters; cross-links resolve via the workspace index's
  `ResolvedTarget` cross-file targets. `--open` is `sys`-gated best-effort. **NAV gotcha:** default emits
  a **separate self-contained `target/doc/` tree** (no `NAV` dependency, never orphans); folding into
  `docs/` is an explicit opt that would require a `NAV` slug edit in `docs/assets/app.js:11` (spec §3.4).
- [ ] Green both configs; clippy. Review (runs `ascript doc --check` on an undocumented symbol; confirms
  separate-tree default needs no `NAV` edit). Commit.

## Task 4 — LSP hover renders the user's `///` doc
**Files:** `src/lsp/providers/{docs,hover}.rs`. **Tests:** `tests/lsp.rs`.
- [ ] Failing test: hover over a documented decl shows the **user's `///` doc body** (via the Task-1
  extractor), not just the kind label `docs.rs` shows today (`doc_at` `src/lsp/providers/docs.rs:22`).
- [ ] Wire `doc_comment_run` into `doc_at`/hover so the shared extractor feeds both `ascript doc` and the
  LSP (one source of truth).
- [ ] Green both configs; clippy. Review. Commit.

---

# D2 — Test framework depth

## Task 5 — Parallel test-FILE dispatch over `src/worker/` (TestSummary as a `Value::Object`)
**Files:** `src/lib.rs` (`run_tests*`), `src/worker/dispatch.rs`, `src/main.rs` (`--parallel[=N]`).
**Tests:** `tests/cli.rs` + `src/worker/` inline.
- [ ] Failing tests: a multi-file corpus produces an **identical `TestSummary` + identical printed
  output** at `--parallel=1` and `--parallel=N` (the §7 determinism contract); a failing test in one file
  is reported with the right file/name regardless of completion order; a single file degrades to the
  serial/no-isolate path. **Round-trip test:** the isolate encodes its `TestSummary` as a `Value::Object`
  `{passed: number, failed: number, failures: array<{name, message}>}` (all leaves sendable kinds), ships
  it via `encode` (`serialize.rs:360`), and the parent `decode`s + reconstructs a `TestSummary`
  (`interp.rs:780`) — assert the reconstruction is lossless. (`TestSummary` is a plain struct, not a
  `Value`, and the airlock crosses `Value` only via `check_sendable` `serialize.rs:103` — this is the one
  airlock-shaping detail; it adds **no new sendable kind**.)
- [ ] Add a test-file dispatch entry on `src/worker/` (load file + run its registered tests in its own
  `Interp` + return the `Value::Object` across the airlock), reusing the isolate bootstrap + pool
  (`pool::dispatch` `pool.rs:113`, graceful inline-degrade `:85`/`:112`). Add the parallel path to
  `run_tests*` (`src/lib.rs:125`/`:131`): granularity **per file** (the isolation boundary matching the
  airlock); **deterministic aggregation** in stable order (input file order, then registration order)
  before printing so the summary + exit code are byte-identical regardless of completion order. A test
  file that itself dispatches `worker fn`s runs them **inline** (the workers nested-inline rule,
  `pool::in_isolate` `pool.rs:128`) — no deadlock. `--parallel[=N]` defaults N to `num_cpus`, capped by
  `$ASCRIPT_WORKERS` (`pool.rs:46`); serial stays the default for a single file.
- [ ] Green both configs; clippy. Independent review (runs the corpus at `--parallel=1` vs `=N`, diffs
  the printed output byte-for-byte; confirms no new sendable kind; confirms `Value::Object` decode is
  lossless). Commit.

## Task 6 — The unified `Vm.instrument` seam + line coverage (coordinate with DBG)
**Files:** `src/vm/run.rs`, `src/vm/chunk.rs` (line-from-`Span` derivation), `src/main.rs`
(`--coverage`). **Tests:** `tests/cli.rs`, `vm_differential.rs` (observation-only assertion).
- [ ] Failing tests: a known program yields the **expected line-hit set** (line derived from the
  per-instruction byte `Span` start via `Chunk::span_at`, NOT a line table — spec §6.3); a `--coverage`
  run's **program stdout is byte-identical** to a non-coverage run (hook is observation-only — feeds the
  Gate-1 invariant); parallel coverage **merge** sums per-isolate tables in stable key order
  (order-independent). Output formats `text` (default), `lcov`, `html` (reusing the `ascript doc` HTML
  style).
- [ ] Add **one** `Vm.instrument: Option<Instrument>` field beside `specialize` (`run.rs:104`); the hot
  loop (`run.rs:582`/`:598`) performs **exactly one** `None`-gated, predictably-not-taken check — when
  `None` the loop is byte-identical to today (the `Vm.specialize` pattern). `enum Instrument {
  Coverage(CoverageTable) }` — the feature dispatch happens **only after** the single gate is taken (cold
  path), so DBG later *adds a variant* (`Debug`/`Profile`), NOT a second field (cross-cutting #6 / spec
  §6.3.1). `CoverageTable` keyed by `(chunk_id, line)`, incremented as instructions retire; line derived
  from the byte `Span` (shared derivation with DBG). **Whichever of DX/DBG merges first introduces
  `Vm.instrument`; the second adds its variant.** Coverage runs on the **VM only** (tree-walker is the
  oracle, not instrumented — documented asymmetry, like SP3 VM-only caps). Runtime-only table — **no
  `.aso`/`ASO_FORMAT_VERSION` change.** `--coverage[=text|lcov|html]` plumb in `main.rs`.
- [ ] Green both configs; clippy; `vm_differential` **unchanged**. Review (greps the hot loop for exactly
  ONE added gate; confirms `None` branch is the untouched path; confirms tree-walker not instrumented).
  Commit.

## Task 7 — The coverage-OFF zero-cost benchmark (Gate 12, REQUIRED)
**Files:** `bench/` (or `tests/` bench harness). **Tests:** the bench parity assertion.
- [ ] Three configs over an IC/arithmetic-heavy corpus (spec §6.3.2): (1) pre-DX `main` baseline, (2)
  post-DX **coverage off** (`Vm.instrument == None`), (3) `--coverage` on. **Acceptance:** config (2)
  shows **no measurable steady-state regression** vs (1) (the single predictably-not-taken check is in
  the noise — assert the parity bound), and config (3)'s overhead is **reported** (the attached path is
  expected to cost). Run in **both** feature configs. This is the Gate-12 proof the seam adds no hot-loop
  tax.
- [ ] Bench green (parity bound holds); review (re-runs the bench, confirms (1)≈(2)). Commit.

## Task 8 — Snapshot completion: `--update-snapshots`, obsolete detection, structural diff
**Files:** `src/stdlib/assert_mod.rs`, `src/interp.rs` (run-level update mode), `src/main.rs`. **Tests:**
`src/stdlib/assert_mod.rs` + `tests/cli.rs`.
- [ ] Failing tests (spec §6.2): first run writes; second run passes; a mutated value fails with a
  **structural diff**; `--update-snapshots` re-baselines the whole run without editing source; an
  **orphaned** snapshot file (no matching assertion touched this run) is reported (removable with
  `--update-snapshots`).
- [ ] Add an `Interp`-level "update mode" the `snapshot_impl` (`assert_mod.rs:36`) reads for
  `--update-snapshots`; track touched snapshot names per run for obsolete detection; reuse the §6.5
  structural diff on mismatch (replacing the raw stored/new dump `assert_mod.rs:63`). `--update-snapshots`
  flag in `Command::Test` (`main.rs:484`).
- [ ] Green both configs; clippy. Review. Commit.

## Task 9 — Structural diff + new assertions
**Files:** `src/stdlib/assert_mod.rs`, `src/check/std_arity.rs`, `docs/content/stdlib/assert.md`.
**Tests:** `src/stdlib/assert_mod.rs`.
- [ ] Failing tests (spec §6.5): a shared **structural diff** for `assert.eq`/snapshot mismatch (per-key
  Object add/remove/change, per-index Array change) over `deep_equal`'s traversal (`assert_mod.rs:20`),
  replacing `expected X got Y`. New assertions: `assert.matches(value, regex)`, `assert.deepEq` (alias),
  `assert.throwsWith(fn, substr)` (message assertion on the existing async `assert.throws`).
- [ ] Implement; register each in `std_arity.rs` (curated arity) and in the assert exports
  (`assert_mod.rs:99`); document in `docs/content/stdlib/assert.md` (NAV unchanged — append to existing
  page).
- [ ] Green both configs; clippy. Review. Commit.

## Task 10 — Test filtering + watch mode
**Files:** `src/main.rs`, `src/lib.rs`, watch (`sys`-gated). **Tests:** `tests/cli.rs`.
- [ ] Failing tests (spec §6.4/§6.6): `--filter PATTERN` (substring or `/regex/`) prunes both which files
  to load and which registered tests to run; `--watch` re-runs affected tests on file change, scoping by
  the **workspace import graph** (`ImportEdge`/`importers` `workspace.rs:86`/`:103`) so only files whose
  import closure touched the change re-run (falls back to "run all" if the graph is unavailable), reusing
  the parallel runner.
- [ ] Implement the flags on `Command::Test`; `--watch` is `sys`-gated file watching driving re-runs.
  (`test.only`/`test.skip` source markers are an explicit deferral — spec §6.6 / §10.)
- [ ] Green both configs; clippy. Review. Commit.

---

# D3 — LSP semantic-resolver unification + completion + inlay

## Task 11 — `FileId` interner + file-qualified `GlobalBindingId` (the identity unification)
**Files:** `src/lsp/workspace.rs`. **Tests:** `tests/lsp.rs`.
- [ ] Failing tests for the LOCKED unification (spec §4.1, review finding [A]): rename an export `x`
  defined in file A → its uses in importers rename, but a **same-named local `let x` in an importer is
  NOT touched**; two locals named `x` in different files at the **same byte range stay distinct**;
  cross-file references include the frame-precise local *and* its cross-file uses uniformly. These are
  exactly the edges the bare `navigation.rs` `BindingId` (`Local(TextRange)` collides across files;
  `Global(String)` is name-only — `navigation.rs:72-73`) gets wrong.
- [ ] Add a `FileId(u32)` + a `PathBuf→FileId` interner over the existing canonical-`PathBuf` keys
  (`WorkspaceIndex.files` `workspace.rs:97`). Define `GlobalBindingId::{Local(FileId, TextRange),
  Global(FileId /*definer*/, String)}`: a local/upvalue is `(FileId, decl-TextRange)`; a module-global/
  export is `(definer-FileId, exported-name)` — an importer's use resolves through its `ImportEdge`
  (`workspace.rs:68`) to the **definer's** `FileId`. Make the cross-file index a **join over per-file
  `syntax::resolve` results lifted into `GlobalBindingId`**, replacing the name-only `collect_uses`
  (`workspace.rs:904`, which tags by `ResolvedTarget` on bare name `:44`). The in-file `navigation.rs`
  `BindingId` stays **file-local** as the per-file projection (paired with the model's `FileId`) — no
  third resolver, no legacy AST.
- [ ] Green both configs; clippy. Independent review (probes the shadowed-local cross-file rename edge
  and the same-byte-range-different-file distinctness; confirms `navigation.rs` `BindingId` is unchanged
  and only *lifted*). Commit.

## Task 12 — Cross-file references / rename on the unified identity
**Files:** `src/lsp/providers/{navigation,rename}.rs`, `src/lsp/workspace.rs`. **Tests:** `tests/lsp.rs`.
- [ ] Failing tests: `references` and `rename`+`prepareRename` (features 8/9, spec §4.4) match on
  `GlobalBindingId`, so in-file and cross-file results share one identity model — the Task-11 shadowing
  edge holds end-to-end through the providers.
- [ ] Route references/rename through the unified `GlobalBindingId` join (Task 11), removing the
  divergence between frame-precise in-file and name-coarse cross-file.
- [ ] Green both configs; clippy. Review. Commit.

## Task 13 — Completion depth: frame-precise + member items
**Files:** `src/lsp/providers/completion.rs`. **Tests:** `tests/lsp.rs`.
- [ ] Failing tests (spec §4.2): identifier completion offers exactly the bindings **live at the cursor's
  frame** (the resolver's frame/upvalue chain), not a flat name list; **member completion** — `.` after a
  value whose class/shape is known (via `infer`) offers that class's fields/methods, and after a `std/*`
  import offers the module's exports.
- [ ] Implement frame-precise candidates + member completion on the cached `SemanticModel`. (New-construct
  items — `int`/`float` names, enum variants in `match`, `interface` names, generic params — are added by
  each construct spec in its own PR, Gate 8; DX defines the pattern + per-provider test requirement.)
- [ ] Green both configs; clippy. Review. Commit.

## Task 14 — Inlay-hint lockstep + the run-test code lens wired to the runner
**Files:** `src/lsp/providers/{inlay,lens}.rs`. **Tests:** `tests/lsp.rs`.
- [ ] Failing tests: inlay hints (feature 23) render a sensible label for the inferencer's types via
  `infer::hover_type_at` (`inlay.rs:55`) — post-TYPE this gets richer inputs automatically; the DX
  contract is that **every new `CheckTy` kind renders a sensible inlay `Display`** (a per-construct
  provider test, enforced when each construct lands). The "▶ Run test"/"▶ Run all tests" code lenses
  (`lens.rs:1-27`, already present for `test(...)` `:27`) carry commands wired to `ascript test`
  (feature 19) — assert the lens command invokes the runner with the right file/filter.
- [ ] Wire the lens commands to `ascript test`; add the per-construct inlay-`Display` test scaffold.
  (`symbols`/`semantic_tokens` completeness hooks per construct land in those specs' PRs — DX defines the
  pattern.)
- [ ] Green both configs; clippy. Review. Commit.

---

# D4 — Diagnostics quality

## Task 15 — Multi-error reporting (`report_all`)
**Files:** `src/diagnostics.rs`, `src/main.rs` (run/parse path). **Tests:** `tests/cli.rs`.
- [ ] Failing test (spec §5.1): a file with multiple parse errors renders **all** of them together (the
  error-tolerant CST parser builds a tree with error nodes — collect all parse diagnostics) instead of
  one. A single fatal runtime panic stays single-report (a Tier-2 abort, not batchable).
- [ ] Add `report_all(&[AsError])` (ariadne supports multiple labels/reports) beside `report`
  (`diagnostics.rs:6`); wire the run/parse path to collect + batch recoverable parse diagnostics.
  (`ascript check` already emits all diagnostics — keep that.)
- [ ] Green both configs; clippy. Review. Commit.

## Task 16 — "Did you mean" (`suggest::closest`) — CLI + LSP quick-fix
**Files:** `src/check/suggest.rs` (new), `src/diagnostics.rs`, `src/lsp/providers/code_action.rs`.
**Tests:** `tests/check.rs` + `tests/lsp.rs`.
- [ ] Failing tests (spec §5.2): an unresolved name (`Resolution::Unresolved`) suggests the closest
  in-scope binding/builtin/import within edit distance (Levenshtein ≤2 or ≤⌈len/3⌉) and **not beyond it**;
  an unknown member / unknown `std/*` export suggests the closest field/method/export. Rendered as an
  ariadne `help`/note (`unknown name 'lenght' — did you mean 'length'?`) at the CLI **and** as a
  `codeAction` quick-fix (feature 18) in the LSP.
- [ ] Add a shared `suggest::closest(name, &candidates) -> Option<&str>` used by both the CLI renderer
  and the checker rules; candidate sets = the resolver scope chain + `BUILTIN_NAMES` (`interp.rs:132`) +
  imports for names, class members / module exports for members. Surface the quick-fix in
  `code_action.rs`.
- [ ] Green both configs; clippy. Review (probes the within/beyond-distance boundary). Commit.

## Task 17 — The SP1 1-column caret-offset audit + fix
**Files:** `src/lsp/convert.rs`, `src/diagnostics.rs` (span construction); both front-ends. **Tests:**
caret-column golden (CLI + LSP).
- [ ] Failing test: a representative error pins the **caret column** in each front-end and asserts they
  **match** (today's documented "Accepted SP1 trade-off" — 1-column offset between CST and legacy
  front-ends). Now that all static tooling is on the CST front-end (spec §4.1/§5.3), default posture is
  **fix**.
- [ ] **Audit** where the off-by-one originates (CST byte-range→char-offset `Span` vs the legacy lexer's
  char spans; `char_to_byte`/`byte_to_char` `src/lsp/convert.rs:13`/`:23`, `diagnostics.rs:36` — likely an
  inclusive/exclusive or 0/1-based column boundary). **Fix** so the caret lands on the exact column in
  **both** front-ends. If the audit proves it irreducible (a deliberate span convention the runtime
  depends on), **formally accept** it with a one-line owner note + a test pinning the *current* behavior
  (spec §5.3) — but default is fix-with-golden.
- [ ] Green both configs; clippy. Independent review (re-runs the audit, confirms the golden pins equal
  columns OR carries the documented acceptance note). Commit.

## Task 18 — Message-clarity style guide
**Files:** highest-traffic Tier-2 panic messages + checker diagnostics; a short style-guide note (in the
spec / `CLAUDE.md`). **Tests:** the touched message tests.
- [ ] Sweep high-traffic messages for consistency (include the offending value's `type_name`, expected
  shape, a `help` line where applicable — spec §5.4). Write down the **style guide** every construct spec
  follows for its new errors (DX owns the guide; each spec writes its own messages).
- [ ] Green both configs; clippy. Review. Commit.

---

# Cross-cutting (DX-owned, runs/updated as other specs add surface)

## Task 19 — Campaign-wide README/landing prose repositioning + `///` docs page
**Files:** `README.md`, `docs/index.html`, `docs/content/language/` (a `///`/`//!` convention mention +
an `ascript doc` reference page), `docs/assets/app.js` (`NAV`). **Tests:** docs reachability.
- [ ] **DX owns the scripting→general-purpose *prose repositioning*** (spec §8, LOCKED): rewrite
  `README.md`'s intro + `docs/index.html`'s hero + the stdlib/feature **table** listing the new pillars.
  Each construct spec owns its OWN per-construct content page in its own PR (NUM the numeric tower, ADT
  enums, IFACE interfaces, TYPE generics — stated here so the repositioning is neither orphaned nor
  duplicated). Add an `ascript doc` reference page + the `///` convention mention; **if a new page slug is
  added, wire it into the `NAV` array (`docs/assets/app.js:11`)** — sidebar + cmd-K derive from it; a
  page with no `NAV` entry is unreachable (Gate 11).
- [ ] Docs serve + reachable (cmd-K finds new pages); review. Commit.

## Task 20 — The `examples/advanced/` DX artifact (Gate 9)
**Files:** `examples/advanced/documented_library.as` (new); doc-gen + coverage goldens. **Tests:**
conformance + `tests/cli.rs`.
- [ ] A fully-documented, production-shaped module under `examples/advanced/` carrying `///` docs on its
  fns/classes/enums **and** an accompanying `test(...)` suite, so the one file simultaneously (a) is the
  `ascript doc` golden source, (b) exercises the parallel test runner + snapshot/coverage path, and (c)
  stays runnable via `target/release/ascript run`. It must **`run` clean**, **`doc` to the pinned
  golden**, and **`test --coverage` to a known line-hit set** (spec §8/§9, the "DX dogfoods its own
  surface" artifact). Confirm `examples/**` still emits **zero** `type-*` (Gate 5).
- [ ] All three (run/doc/test --coverage) green; review. Commit.

---

## Done when
Every task checked behind an independent review. **Gate 1:** `vm_differential` unchanged (DX adds no
engine surface) — a `--coverage` run and a normal run produce byte-identical program output. **Gate 5:**
`examples/**` emits zero `type-*`. **Gate 9:** the `examples/advanced/` DX artifact `run`s clean, `doc`s
to the golden, and `test --coverage` hits the known line set. **Gate 11:** README/landing repositioned,
new doc pages wired into `NAV`. **Gate 12:** the coverage-off bench shows config (1)≈(2) (no steady-state
regression with `Vm.instrument == None`), reports the on-cost, in both feature configs. The cross-file
identity edges (shadowed-local rename, same-byte-range distinctness) hold; multi-error + did-you-mean +
caret-column goldens pass; clippy + `cargo test` + `--no-default-features` green in BOTH configs. Each
sub-deliverable (D1–D4) may merge independently `--no-ff` to `main`. **Cross-spec:** whichever of DX/DBG
merges first introduces `Vm.instrument` + the single gate; the other adds its variant (cross-cutting #6).
DX then remains the standing surface every later construct spec updates (Gate 8).
