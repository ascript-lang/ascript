# SP4 — Checker & tooling expansion — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the static checker + LSP project-aware: apply safe autofixes (`ascript check --fix`),
extend `call-arity` across modules and to method/constructor calls (zero false positives), fix
cross-module diagnostic span provenance, and build a persistent cross-file workspace index powering
go-to-def / symbols / references / rename.

**Architecture:** Four phases (A–D) plus a closing docs+holistic phase (E). Each phase is TDD, ends
green on BOTH feature configs + clippy (both) + the standing zero-FP corpus guard, and gets an
independent review before the next. The checker core stays feature-INDEPENDENT (`--no-default-features`
must build+pass); the LSP stays static-analysis-only (never instantiates the interpreter; stays
`Send + Sync`).

**Tech Stack:** Rust. CST front-end (`src/syntax/{lexer,parser,tree_builder,resolve}`) feeds the
checker (`src/check/`) and the LSP (`src/lsp/`). `ascript check` renders via `src/check/render.rs`;
runtime errors via `src/diagnostics.rs`. LSP = tower-lsp over stdio.

**Spec:** `docs/superpowers/specs/2026-06-04-sp4-checker-tooling-design.md`.

**Branch:** `feat/sp1-engine-parity` (current) — create `feat/sp4-checker-tooling` off it (or off
`main` once SP1 merges; SP4's §2 composes with SP1's static methods / default params, so prefer
branching after SP1 lands).

---

## Conventions for every task

- **Checker unit-test pattern:** rules test via `crate::check::analyze(src)` and filter
  `.diagnostics` by `code` (see `src/check/rules/call_arity.rs` tests for the exact `count`/`has`
  helpers — reuse that style). The `--fix` applicator is a pure function with direct unit tests.
- **CLI integration:** `tests/cli.rs` spawns the built binary (`env!("CARGO_BIN_EXE_ascript")`);
  write fixtures to a `tempfile::TempDir`, run `ascript check ...`, assert stdout/exit.
- **LSP integration:** `tests/lsp.rs` (existing) — follow its harness for the new providers.
- **Gate after each phase (paste tails):**
  `cargo test 2>&1 | tail` (0 failures, all binaries) ·
  `cargo test --no-default-features 2>&1 | tail` (0 failures) ·
  `cargo clippy --all-targets 2>&1 | tail` AND `cargo clippy --no-default-features --all-targets 2>&1 | tail` (clean) ·
  `target/debug/ascript check examples/*.as examples/advanced/*.as` (0 diagnostics — zero-FP guard).
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **No `unsafe`, no `#[allow]`, no `#[ignore]`, no stubs.** Every deferral is a documented scope cut,
  never a silent drop.

---

## Phase A — `ascript check --fix` autofix application

**Files:** `src/check/rules/unused.rs` (correct the import fix range), `src/check/fix.rs` (NEW —
applicator), `src/check/mod.rs` (re-export `fix`), `src/main.rs` (`--fix`/`--fix-dry-run` + post-fix
exit). Tests: `src/check/fix.rs` unit, `tests/cli.rs`.

### Task A1: overlap-safe edit applicator (pure core)

- [ ] **Step 1 — Failing unit tests.** Create `src/check/fix.rs` with a `#[cfg(test)] mod tests` and
  write tests FIRST (no impl yet):
  - `apply_edits` splices a single edit by byte range correctly.
  - descending-order: two non-overlapping edits both apply, output correct regardless of input order.
  - overlap: an edit nested inside another → the inner (or later-sorted) edit is dropped, output is
    well-formed and equals applying only the surviving edit.
  - adjacent (touching, `[0,3)` + `[3,6)`) edits both apply.
  - empty edit list → input returned unchanged.
- [ ] **Step 2 — Implement** in `src/check/fix.rs` (feature-independent — no feature-gated imports):
  ```rust
  use crate::check::{Analysis, TextEdit};
  pub const FIXABLE_CODES: &[&str] = &["unused-import"];
  pub fn collect_fixes(analysis: &Analysis) -> Vec<TextEdit> { /* codes ∈ FIXABLE_CODES && fix.is_some() */ }
  pub fn apply_edits(src: &str, edits: &[TextEdit]) -> String { /* sort desc by start; drop overlaps; splice */ }
  pub fn render_diff(path: &str, before: &str, after: &str) -> String { /* hand-rolled unified line diff */ }
  ```
  Sort a copy of `edits` by `range.start` descending; track the lowest applied `start` to detect
  overlap; splice `replacement` into a `String` by byte slice. `render_diff` is a minimal line-based
  unified diff (serde-free, matching the hand-rolled JSON renderer's posture).
- [ ] **Step 3 — Run** `cargo test --lib check::fix 2>&1 | tail` → green. Add `pub mod fix;` to
  `src/check/mod.rs` and re-export `pub use fix::{apply_edits, collect_fixes, FIXABLE_CODES};`.
- [ ] **Step 4 — Commit:** `feat(check): overlap-safe autofix applicator (fix.rs)`.

### Task A2: correct the unused-import fix range (statement/clause-level)

- [ ] **Step 5 — Failing tests** (`src/check/rules/unused.rs` tests): assert applying the
  `unused-import` fix to `import { a, b } from "std/math"` (with `a` unused, `b` used) yields
  `import { b } from "std/math"` that **re-analyzes with zero `syntax-error` and zero
  `unused-import`**; and that `import * as t from "std/task"\nprint(1)\n` (t unused) → `print(1)\n`
  (no leading blank line). These fail today because `unused.rs:34-49` replaces only the name token
  (`b.decl_range`), producing `import {  } from ...` (a syntax error).
- [ ] **Step 6 — Implement.** Un-underscore the `_tree` param (`unused.rs:8`). For
  `BindingKind::Import`, compute the removal range from the tree:
  - find the enclosing `ImportStmt` whose `ImportList` contains `b.decl_range`;
  - if the import binds a SINGLE name (whole-statement) → edit range = the `ImportStmt` node range
    extended to include the trailing newline; replacement = `""`.
  - if a multi-name `import { a, b }` → edit range = the unused name token + its adjacent comma
    (prefer the following `, `, else the preceding); replacement = `""`.
  Keep `unused-binding`'s name-span fix as-is (LSP-only; NOT in `FIXABLE_CODES`).
- [ ] **Step 7 — Run** the unused tests + `cargo test --lib check 2>&1 | tail` → green.
- [ ] **Step 8 — Idempotence test** (`src/check/fix.rs`): for a few import-bearing programs,
  `apply_edits(src, collect_fixes(&analyze(src)))` re-analyzed yields no fixable diagnostics; applying
  again is a no-op (byte-identical).
- [ ] **Step 9 — Commit:** `fix(check): unused-import fix removes the statement/clause, not just the name`.

### Task A3: CLI `--fix` / `--fix-dry-run`

- [ ] **Step 10 — Failing CLI tests** (`tests/cli.rs`): write a temp `f.as` with an unused import +
  used code; `ascript check --fix f.as` rewrites the file (import gone, rest intact) and exits 0;
  `ascript check --fix-dry-run f.as` prints a diff and leaves the file byte-identical; `--fix` twice
  == once.
- [ ] **Step 11 — Implement** in `src/main.rs` `Command::Check`: add `#[arg(long)] fix: bool` and
  `#[arg(long = "fix-dry-run")] fix_dry_run: bool`. After `analyze_with_config` per file: if `fix ||
  fix_dry_run`, `let edits = check::fix::collect_fixes(&analysis)`; `fix_dry_run` → print
  `render_diff` (or JSON edits under `--json`); `fix` → `let after = apply_edits(&src, &edits)`, write
  back via `std::fs::write` only if changed, print `fixed N issue(s) in <file>`, then **re-analyze
  `after`** for exit-status purposes (a file whose only issue was a fixed import exits clean). Reject
  `--fix` + `--fix-dry-run` together with a usage error (exit 2).
- [ ] **Step 12 — Run** `cargo test --test cli 2>&1 | tail` → green; manual smoke on a temp file.
- [ ] **Step 13 — Phase-A gate** (full gate set, BOTH configs — `fix.rs` must compile+pass under
  `--no-default-features`).
- [ ] **Step 14 — Commit:** `feat(cli): ascript check --fix / --fix-dry-run (unused-import, idempotent)`.

---

## Phase B — Cross-module / method / constructor `call-arity`

**Files:** `src/check/rules/mod.rs` (shared `resolves_to_unique`, `Arity`/`arity_of`),
`src/check/rules/call_arity.rs` (three new callee shapes), `src/check/std_arity.rs` (NEW — curated
std-fn arity table), `src/check/rules/contract.rs` (de-dup the shared resolver helper). Tests inline +
`tests/cli.rs`. (File-module cross-file arity is wired in Phase D once the index exists.)

### Task B1: factor the shared resolver helper + Arity range

- [ ] **Step 1 — Implement (refactor, behavior-preserving).** `call_arity.rs` and `contract.rs` have
  byte-identical `resolves_to_fn`. Move a generalized
  `resolves_to_unique(callee, name, decl_range, kind: BindingKind, resolved) -> bool` into
  `src/check/rules/mod.rs`; have both call it with `BindingKind::Fn`. Add
  `pub(crate) struct Arity { pub min: usize, pub max: Option<usize> }` and `arity_of(param_list:
  &ResolvedNode) -> Arity` (rest param ⇒ `max = None`; today AScript has no default params, so
  `min == max == fixed count` for non-rest — SP1 default params later widen `max`). Replace
  `fixed_param_count` usage with `arity_of`, keeping current behavior (no rest ⇒ exact).
- [ ] **Step 2 — Run** `cargo test --lib check 2>&1 | tail` → all existing call-arity/contract tests
  green (pure refactor). Commit: `refactor(check): shared resolves_to_unique + Arity range`.

### Task B2: constructor-call arity

- [ ] **Step 3 — Failing tests** (`call_arity.rs`): `class C { fn init(a, b) {} }\nC(1)` → 1
  `call-arity`; `class C {}\nC(1)` → 1 (default ctor is 0-arg); `C(1, 2)` for the 2-arg init →
  none; imported/shadowed class → none.
- [ ] **Step 4 — Implement.** In the `CallExpr` loop, when the callee `NameRef` resolves to a unique
  `BindingKind::Class` `C` (`resolves_to_unique(.., BindingKind::Class, ..)`): find `C`'s `ClassDecl`,
  look for a `MethodDecl` named `init` → its `arity_of(ParamList)`; no `init` ⇒ `Arity{min:0,max:Some(0)}`.
  Flag `argc < min || (max.is_some() && argc > max)`. Skip spread args (existing rule).
- [ ] **Step 5 — Run** → green. Commit: `feat(check): call-arity for constructor calls C(args)`.

### Task B3: method-call arity (receiver class statically known)

- [ ] **Step 6 — Failing tests:** `let c = C()\nc.m(1,2,3)` where `class C { fn m(x) {} }` → 1 flag;
  `c.m(9)` → none; `self.m(...)` inside a method of `C`; inherited method via
  `class B extends A { } ... B().m(...)`; **must-not-flag:** reassigned receiver (`let c = C(); c = other; c.m(1)`),
  receiver from a return (`let c = make(); c.m(1)`), `?.` optional call, computed member.
- [ ] **Step 7 — Implement.** For a `CallExpr` whose callee is a `MemberExpr` `recv.m`:
  - Determine the receiver's class with CERTAINTY only when: `recv` is `self` inside a method of a
    unique class `C`; OR `recv` is a `NameRef` to a `let`/`const` whose initializer is **directly**
    `C(...)` for a unique class `C` AND the binding is never reassigned (`Binding.mutated == false`
    via `ResolveResult.bindings`). Any other receiver → skip.
  - Resolve `m` to a unique `MethodDecl` on `C` (or up the `extends` chain); `arity_of` its ParamList;
    flag with the range rule. Skip `OptMemberExpr` callees and spread args.
- [ ] **Step 8 — Run** → green (especially the must-not-flag cases). Commit: `feat(check): call-arity for method calls on a statically-known receiver`.

### Task B4: imported std-function arity

- [ ] **Step 9 — Create `src/check/std_arity.rs`** (feature-independent): a curated table
  `pub fn std_fn_arity(module: &str, name: &str) -> Option<Arity>` covering fixed-arity std fns
  (e.g. `("std/math","abs") => Arity{min:1,max:Some(1)}`); variadic/overloaded fns return `None`
  (skip). Add a `#[test]` asserting every keyed `(module,name)` is a real export of that module per
  `crate::stdlib::std_module_exports` (drift guard — answers owner Q2 with the test option).
- [ ] **Step 10 — Failing tests** (`call_arity.rs`): `import { abs } from "std/math"\nabs(1, 2)` → 1
  flag; `abs(-1)` → none; an unlisted/variadic std fn → none.
- [ ] **Step 11 — Implement.** When the callee resolves to `BindingKind::Import`, find the
  `ImportStmt` binding that name, read its specifier; if `std/*` and `std_fn_arity(module, name)` is
  `Some`, apply the range rule. File-module imports are left to Phase D (skip here).
- [ ] **Step 12 — Run** → green. **Zero-FP corpus guard** must stay 0. Commit: `feat(check): call-arity for imported std functions (curated arity table)`.
- [ ] **Step 13 — Phase-B gate** (full set, both configs) + adversarial FP hunt: confirm no example
  in the corpus is newly flagged.

---

## Phase C — Cross-module diagnostic span provenance

**Files:** `src/error.rs` (`span_source`), `src/diagnostics.rs` (render against the span's source),
`src/interp.rs` (bind source at raise time; retain `SourceInfo` on `ModuleEntry`). Tests
`tests/modules.rs` / `tests/cli.rs`.

### Task C1: failing two-file provenance test

- [ ] **Step 1 — Write the repro** (`tests/cli.rs` or `tests/modules.rs`): a temp dir with `a.as`
  defining a fn that panics at a known site and `b.as` that `import`s + calls it. Run `ascript run
  b.as` (capture stderr); assert the rendered report names **`a.as`** and shows `a.as`'s offending
  line (NOT `b.as`'s text at a misattributed offset). This fails today: the caret renders against
  the wrong module (the span belongs to A, the attached `source` may be B).
- [ ] **Step 2 — Confirm the failure** and note exactly which module's text/path is rendered, so the
  fix targets the real raise/attach path.

### Task C2: bind span to its source at raise time

- [ ] **Step 3 — Implement** in `src/error.rs`: add `pub span_source: Option<Rc<SourceInfo>>` to
  `AsError` (default `None`); add a constructor/setter `at_in(message, span, src)` that sets both
  `span` and `span_source` together. Keep `with_source` (outer/context source) as the fallback.
- [ ] **Step 4 — Audit + fix the raise path** (`src/interp.rs`): when a panic is raised while
  executing a LOADED module's body, attach that module's `SourceInfo` as the `span_source` at THAT
  frame (before the error crosses the import boundary). `load_module` (`interp.rs:866-913`) already
  builds the module `src_info`; ensure the module-body execution frame binds it to runtime panics,
  not just lex/parse errors. Store `Rc<SourceInfo>` on `ModuleEntry` (additive) so the executing
  frame can attach it.
- [ ] **Step 5 — Render against the span's source** (`src/diagnostics.rs`): in `report`, prefer
  `err.span_source` over `err.source` for the caret (fall back to `source` when `span_source` is
  `None`, preserving single-module behavior). Single-module errors set both to the same `SourceInfo`,
  so they are unchanged.
- [ ] **Step 6 — Run** the Task-C1 repro → caret renders in `a.as`. Add a **regression** test: a
  panic in a standalone single file still renders byte-identically to before (message + caret).
- [ ] **Step 7 — Phase-C gate** (full set, both configs) + commit: `fix(diagnostics): cross-module span provenance — caret renders in the span's own module`.

---

## Phase D — Cross-file LSP workspace index

The largest phase; build in sub-phases L1→L4. **Files:** `src/lsp/workspace.rs` (NEW), `src/lsp/analysis.rs`
(cross-file def), `src/lsp/server.rs` (index field + providers + capabilities), `src/lsp/mod.rs`.
Tests: `src/lsp/workspace.rs` unit + `tests/lsp.rs`. The path-aware checker arity from Phase B's
file-module deferral is wired here (D-arity).

### Task D-L1: index core (no new providers)

- [ ] **Step 1 — Failing unit tests** (`src/lsp/workspace.rs`): build a `WorkspaceIndex` over a
  3-file in-memory fixture (`a` defines + exports `f`; `b` imports + uses `f`; `c` unrelated) and
  assert `defs_by_name`, per-file `exports`, `import_edges`, and `importers` are correct; an edit to
  `c` leaves `a`/`b` indices untouched; a syntax error typed into `b` retains b's last-good index.
- [ ] **Step 2 — Implement** `src/lsp/workspace.rs` (gated under `lsp`, no interpreter; only
  `String`/`PathBuf`/`ByteSpan` — assert `Send + Sync`): the structs from the spec sketch
  (`WorkspaceIndex`, `FileIndex`, `SymbolDef`, `ImportEdge`, `UseSite`, `ResolvedTarget`). Build a
  `FileIndex` by calling the SAME `tree_builder::build_tree` + `resolve::resolve` the checker uses,
  then projecting `ResolveResult.uses`/`bindings` into `UseSite`/`SymbolDef` and the `export` decls
  into `exports`. Resolve import specifiers statically with the runtime's rule (mirror
  `Interp::resolve_import`, `interp.rs:945-951`: `dir.join(spec)` + `.as`); `std/*` → a `Std` target.
  Provide `build_from_root(root) `, `reindex_file(path, text)` (incremental delta update of
  `defs_by_name`/`import_edges`/`importers`), and `exported_fn_arity(module, name)` (for D-arity).
- [ ] **Step 3 — Wire into `Backend`** (`src/lsp/server.rs`): add `index: RwLock<WorkspaceIndex>`;
  capture workspace roots in `initialize`; build the index in `initialized` (walk roots for `*.as`);
  `did_open`/`did_change` call `reindex_file`. No provider behavior change yet.
- [ ] **Step 4 — Run** `cargo test --lib lsp::workspace 2>&1 | tail` + `cargo test --test lsp 2>&1 | tail` → green. Commit: `feat(lsp): cross-file workspace index core (warm, incremental)`.

### Task D-L2: cross-file go-to-definition

- [ ] **Step 5 — Failing test** (`tests/lsp.rs`): open `a.as` + `b.as`; goto-definition on `b`'s use
  of `a`'s exported `f` returns a `Location` whose `uri` is `a.as` and whose range is `f`'s decl name
  range. (Same-file def still works.)
- [ ] **Step 6 — Implement.** In `goto_definition` (`server.rs:139-157`): if the use at the cursor is
  an imported/cross-file name (the index's `UseSite.target` is `Imported{module,name}`), return the
  target file's `uri` + the export's `name_range`. Else fall back to the existing single-file
  `analysis::definition` (`analysis.rs:592`) returning the same-file `uri`.
- [ ] **Step 7 — Run** → green. Commit: `feat(lsp): cross-file go-to-definition via the workspace index`.

### Task D-L3: workspace symbols + find-references

- [ ] **Step 8 — Failing tests:** `workspace/symbol "f"` returns matches across files;
  find-references on `a`'s `f` finds `b`'s use site(s) (and `a`'s own).
- [ ] **Step 9 — Implement.** Advertise `workspace_symbol_provider` + `references_provider`
  (`server_capabilities`, `server.rs:39`). `workspace/symbol` queries `defs_by_name`. References:
  collect `UseSite`s targeting the def across the def's file + files whose `import_edges` reach the
  def's module (use `importers`).
- [ ] **Step 10 — Run** → green. Commit: `feat(lsp): workspace symbols + find-references (cross-file)`.

### Task D-L4: rename across files

- [ ] **Step 11 — Failing tests:** rename `a`'s exported `f` produces a `WorkspaceEdit` rewriting
  `f`'s decl, `b`'s import clause, and `b`'s use sites; rename is REFUSED if a touched file has a
  parse error or the new name collides with an existing binding in a touched scope.
- [ ] **Step 12 — Implement.** Advertise `rename_provider` (with `prepare` to reject non-renameable
  positions). Build the `WorkspaceEdit` from the reference set (def `name_range` + every referencing
  `UseSite` + import clauses naming it), scoped to the def's file + its direct `importers` (v1 scope).
  Guard against parse-error files and name collisions.
- [ ] **Step 13 — Run** → green. **Compile-time `Send + Sync` assertion** for `Backend`/`WorkspaceIndex`.
- [ ] **Step 14 — D-arity:** wire Phase-B's file-module arity deferral: add a path-aware
  `analyze_file(path, src, &index)` (or feed the index into the LSP diagnostics path) that uses
  `index.exported_fn_arity(resolved_module, name)` for `BindingKind::Import` callees resolving to a
  file module. The path-less `analyze(src)` is unchanged (no cross-file file arity). Test: a 2-arg
  exported fn called with 1 across files → flagged; an import whose target has a parse error → NOT
  flagged.
- [ ] **Step 15 — Phase-D gate** (full set; `lsp` feature on) + adversarial check that NO interpreter
  type leaked into the LSP and the layer stays `Send + Sync`. Commit: `feat(lsp): cross-file rename + index-backed file-module call-arity`.

---

## Phase E — Docs + holistic review

**Files:** `docs/content/*` (tooling page), `README.md`.

### Task E1: docs

- [ ] **Step 1 — Update** the `ascript check` / tooling docs page in `docs/content` (`--fix` /
  `--fix-dry-run`, cross-module/method/constructor `call-arity`, cross-file editor navigation/rename)
  and `README.md`'s CLI section. Verify every documented command against the built binary.
- [ ] **Step 2 — Commit:** `docs: ascript check --fix, cross-module arity, cross-file LSP`.

### Task E2: holistic gate + review

- [ ] **Step 3 — Full gate set** both feature configs + clippy both + zero-FP corpus guard.
- [ ] **Step 4 — Independent review** (re-read spec, re-run gates, adversarial hunt: arity FPs on
  reassigned/return-value receivers; `--fix` overlap + idempotence; span provenance on a 3-module
  chain; rename collision/parse-error refusal; `Send + Sync` of the LSP).
- [ ] **Step 5 — Final commit** if review surfaced fixes; otherwise the phase is complete.

---

## Self-review (author)

**Spec coverage:** §1 `--fix` → Phase A (applicator A1, correct import range A2, CLI A3); §2
cross-module arity → Phase B (refactor B1, constructor B2, method B3, std B4) + the file-module piece
in D-L4; §3 span provenance → Phase C; §4 workspace index → Phase D (L1 core, L2 def, L3 symbols/refs,
L4 rename). All covered.

**Invariant coverage:** feature-independence — `fix.rs` + `std_arity.rs` are in the checker core and
gated by the `--no-default-features` gate every phase; LSP static-only + `Send+Sync` — asserted in
D-L1/D-L4. Per-task commit trailer specified. No `unsafe`/`#[allow]`/`#[ignore]`/stubs — all
deferrals (unused-binding autofix, file-arity-needs-index, rename scope) are documented scope cuts.

**Placeholder scan:** No "TBD". Test programs are concrete AScript; the deferred-to-implementer
detail is exact cstree-node navigation for the import-clause comma span (A2) and the projection of
`ResolveResult` into the index (D-L1) — both reference the exact existing code sites
(`unused.rs:34-49`, `duplicate_member.rs:27`, `analyze.rs:32-79`, `interp.rs:945-951`).

**Sequencing:** B's file-module arity intentionally defers to D-L4 (needs the index); B4's std arity
ships in B (no index needed). C is independent (runtime renderer, not the static checker) and could
run in parallel, but is sequenced after B to keep one reviewer context per phase. Open questions for
the owner are recorded in the design doc (§ Open design questions).
