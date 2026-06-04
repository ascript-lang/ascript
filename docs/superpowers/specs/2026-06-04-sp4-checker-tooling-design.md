# SP4 — Checker & tooling expansion — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Sub-project of** the post-cutover gap program (companion to SP1 engine-parity; SP2–SP10 follow).

**Goal:** Turn the static checker + LSP from "single-file, no-autofix, intra-file arity" into a
project-aware toolchain: apply safe autofixes (`ascript check --fix`), extend `call-arity` across
module boundaries and to method/constructor calls, fix cross-module diagnostic span provenance, and
build a persistent cross-file workspace index powering go-to-def / symbols / references / rename.

**Architecture:** Four focused changes, all in the **static** layers — the checker core
(`src/check/`), the CLI binary (`src/main.rs`, `src/lint_config_toml.rs`), the diagnostics renderer
(`src/diagnostics.rs` / `src/error.rs`), and the LSP (`src/lsp/`). Two hard invariants from the
existing code carry through every change:

- **The checker core (`src/check/`) is feature-INDEPENDENT** — it must build and pass under
  `--no-default-features`. The neutral diagnostic model (`src/check/diagnostic.rs`) is deliberately
  serde-free and front-end/interpreter-free; autofix application must not pull in any feature-gated
  dependency.
- **The LSP (`src/lsp/`) is static-analysis-only** — it lexes/parses/resolves and **never
  instantiates the interpreter** (`src/lsp/mod.rs:10-12`). The workspace index reuses the resolver
  (`src/syntax/resolve`) and the pure `analysis` functions; it introduces no `Rc`/`RefCell`/`Value`
  into the `Send + Sync` server.

**Tech stack:** Rust. CST front-end (`lexer` → `parser` → `tree_builder` → `resolve`) feeds both the
checker and the LSP. `ascript check` renders via `src/check/render.rs` (ariadne / hand-rolled JSON);
runtime errors render via `src/diagnostics.rs` (ariadne). LSP is tower-lsp over stdio.

---

## Non-goals (explicitly out of SP4)

- **Type inference / a type checker.** SP4 stays at the checker's current bar: surface
  *guaranteed* runtime errors statically, **zero false positives**. No flow types, no generics
  solving. `contract-mismatch` stays literal-only.
- **Autofix for risky rules.** `--fix` v1 fixes ONLY `unused-import` (and any rule whose textual fix
  is provably non-destructive). `unused-binding` removal is explicitly deferred (a `let x = sideEffect()`
  removal would drop the side effect — unsafe). See §1.
- **Conflating `--fix` with `ascript fmt`.** The formatter is a separate command and is not invoked
  by `--fix`; fixes are range edits on the original source, after which `fmt` may be run separately.
- **Cross-file `contract-mismatch` / typed-field checks.** SP4 extends `call-arity` across modules
  (arity is a structural fact); it does NOT extend literal-type contract checking across modules.
- **Multi-root workspaces / LSP `workspace/didChangeWatchedFiles` from disk daemons.** The index
  discovers files under the workspace root(s) the client provides and re-indexes on `didChange`/
  `didOpen`; a full filesystem-watch daemon is a documented follow-up.
- **Rename of imported/exported symbols across the *whole* graph in v1's first cut** — rename lands
  last and is scoped (see §4, sub-phase L4) to the symbol's defining file + direct importers.

---

## §1 — `ascript check --fix` autofix application (closes the unwired fix path)

### Current behavior (verified)

The diagnostic model **already carries** fixes but the apply path is unwired:

- `src/check/diagnostic.rs:32-43`: `TextEdit { range: ByteSpan, replacement: String }` and
  `Fix { title: String, edits: Vec<TextEdit> }`. `AsDiagnostic.fix: Option<Fix>`
  (`diagnostic.rs:52`).
- The ONLY rule that currently produces a `Fix` is `unused` (`src/check/rules/unused.rs:34-49`): for
  `unused-import` AND `unused-binding` it emits a `Fix` with a single `TextEdit` over
  `ByteSpan::from(b.decl_range)` replacing with the empty string. `call-arity`, `contract`,
  `unresolved-import`, etc. all set `fix: None`.
- `ByteSpan` is a half-open **byte** range `[start, end)` (`diagnostic.rs:8-11`), produced from
  cstree `TextRange` (byte offsets). The checker pipeline (`analyze_with_config`,
  `src/check/analyze.rs:32`) is byte-offset throughout — no char/byte conversion needed.
- The CLI `Command::Check` (`src/main.rs:173-267`) reads each file, seeds a `LintConfig` from
  `ascript.toml` + CLI flags, runs `analyze_with_config`, and renders. It never inspects `d.fix`.

**The key correctness gap in the existing fix:** `unused.rs` replaces `b.decl_range` — the
binding's **name span**, not the whole statement. For `import { abs } from "std/math"` the
`decl_range` is the `abs` token only; deleting just `abs` yields `import {  } from "std/math"` — a
**syntax error**, not a removed import. SP4 must compute a **statement-level** removal range for
import/binding fixes, not reuse the name span. This is a real bug the spec corrects.

### Target

`ascript check --fix <files>` applies the safe, unambiguous autofixes in place; `--fix-dry-run`
prints the unified diff (or, with `--json`, the planned edits) without writing. Re-running `--fix`
is **idempotent** (a fixed file produces no further edits).

**Autofixable in v1 (decided per-rule):**

| Rule | v1 autofix? | Reasoning |
|---|---|---|
| `unused-import` | **Yes** | Removing an unused import is non-destructive (no runtime effect; imports are pure bindings). Fix = delete the whole `ImportStmt` line, OR a single clause from a multi-name `import { a, b }` list. |
| `unused-binding` | **No (deferred)** | `let x = f()` — removing the binding drops `f()`'s side effect. Unsafe without effect analysis. The `Fix` is still *emitted* (LSP code-action can offer it interactively) but `--fix` does NOT apply it. |
| all others | No | No safe, unambiguous textual fix (an arity/contract fix is a human edit). |

**The unit of `--fix` application** is: collect every diagnostic whose `code` is in the
**fix-allowlist** (`FIXABLE_CODES`, initially `["unused-import"]`) AND has `Some(fix)`, gather their
edits, and apply. The allowlist — not merely "has a fix" — gates application, so `unused-binding`'s
emitted fix is offered to editors but never auto-applied.

### Implementation

- **Correct the fix range for imports** (`src/check/rules/unused.rs`): for `BindingKind::Import`,
  the `TextEdit` must cover a *removable* unit:
  - **Whole-statement import** (`import * as t from "..."`, or `import { a } from "..."` where `a`
    is the only clause): delete the entire `ImportStmt` node range **plus its trailing newline**
    (so no blank line remains). The rule has the binding's `decl_range`; to get the statement range
    it walks up to the enclosing `ImportStmt` via the tree (the rule already receives `&ResolvedNode`
    tree root — match the import whose `ImportList` contains `decl_range`). This requires passing the
    tree, which `unused::check` already takes as `_tree` (`unused.rs:8`); un-underscore it.
  - **One clause of a multi-name list** (`import { a, b }` with `a` unused): delete just `a` **and
    the adjacent comma** (the `, ` before or after) so the list stays well-formed:
    `import { b } from "..."`. Compute the comma-inclusive span from the `ImportList`'s tokens.
  - `unused-binding` keeps its name-span fix (LSP-only; not in `FIXABLE_CODES`).
- **New module `src/check/fix.rs`** (feature-independent, in the checker core):
  - `pub const FIXABLE_CODES: &[&str] = &["unused-import"];`
  - `pub fn collect_fixes(analysis: &Analysis) -> Vec<TextEdit>` — gather edits from every diagnostic
    whose code ∈ `FIXABLE_CODES` and `fix.is_some()`.
  - `pub fn apply_edits(src: &str, edits: &[TextEdit]) -> String` — the **overlap-safe applicator**:
    1. Sort edits by `range.start` descending (apply right-to-left so earlier byte offsets stay
       valid as we splice).
    2. **Drop any edit that overlaps a previously-applied edit** (range intersection on the
       half-open `[start,end)`). Overlap is rare for a single allowlisted rule, but the applicator is
       robust by construction. Adjacent (touching, non-overlapping) edits both apply.
    3. Splice each `replacement` into `src` by byte range. (Byte-exact; `ByteSpan` is bytes.)
  - `pub fn render_diff(path: &str, before: &str, after: &str) -> String` — a minimal unified diff
    for `--fix-dry-run` (hand-rolled line diff, serde-free — matches the existing hand-rolled JSON
    renderer's no-dependency posture).
- **Idempotence is structural:** `apply_edits` is a pure function of `(src, edits)`; after applying,
  a re-analyze of the result produces no `unused-import` for the removed import (the import is gone).
  An explicit test asserts `analyze(apply_edits(analyze(src))) ` has no fixable diagnostics.
- **CLI wiring** (`src/main.rs`, `Command::Check`): add `--fix` and `--fix-dry-run` bool args (mutually
  exclusive — `--fix-dry-run` wins / or error if both). When set, after `analyze_with_config`:
  - collect fixes via `check::fix::collect_fixes`;
  - `--fix-dry-run`: print `render_diff` (or, under `--json`, a JSON array of `{path,start,end,replacement}`);
  - `--fix`: `apply_edits`, write back with `std::fs::write` only if the content changed; print a
    one-line summary (`fixed N issue(s) in <file>`).
  - Exit code: `--fix` that successfully applies fixes exits **0** for those (they're resolved);
    diagnostics with NO autofix still drive the normal exit logic (`any_error` / `deny_warnings`).
    Document precisely: after `--fix`, re-evaluate exit status against the *post-fix* analysis so a
    file whose only issue was an auto-fixed import exits clean.

### Tests (`src/check/fix.rs` unit + `tests/cli.rs` integration)

- `apply_edits` overlap safety: two edits, one nested in the other → the inner is dropped, output
  well-formed; adjacent edits both apply; descending-order splice correctness.
- import-clause fix: `import { a, b } from "std/math"` with `a` unused → `import { b } from
  "std/math"` (re-analyzes clean, parses with no error).
- whole-statement import fix: `import * as t from "std/task"\nprint(1)\n` → `print(1)\n` (no blank
  line, re-analyzes clean).
- idempotence: `--fix` twice == `--fix` once; second run reports 0 fixes.
- `unused-binding` is NOT applied by `--fix` (allowlist gate) but its `Fix` is still emitted.
- CLI: `ascript check --fix f.as` rewrites the file and exits 0 when the only issue was the import;
  `--fix-dry-run` prints a diff and leaves the file byte-identical.
- `--no-default-features`: the whole `fix` module + tests compile and pass (feature-independence).

---

## §2 — Cross-module / method / constructor `call-arity`

### Current behavior (verified)

`call-arity` (`src/check/rules/call_arity.rs`) is **intra-file, direct-named-function only**:

- It builds `by_name: HashMap<name, FnDecl>` from `tree.descendants()` of kind `FnDecl`, keeping
  only names declared **exactly once** in the file (`call_arity.rs:34-42`).
- It checks a `CallExpr` whose callee is a plain `NameRef` (`call_arity.rs:48`) — a `MemberExpr`
  callee (`x.m(...)`) is skipped (`method_call_not_flagged` test, `call_arity.rs:206`).
- `resolves_to_fn` (`call_arity.rs:129-157`) requires the callee resolve to a binding that is the
  **unique** `BindingKind::Fn` at exactly the matched `FnDecl`'s `decl_range` — so a shadowing
  `let`/param suppresses the check (zero-FP).
- `fixed_param_count` (`call_arity.rs:104-121`) returns `None` (skip) for a rest param (`...name`),
  so variadics are never flagged. Spread args also skip (`call_arity.rs:77`).
- Imported functions are **never** checked: an imported callee resolves to `BindingKind::Import`,
  which `resolves_to_fn` rejects (it requires `BindingKind::Fn`). Methods and constructors are never
  checked.

### Target

Extend `call-arity` to three new callee shapes, **staying zero-false-positive** — flag only when the
callee signature is statically certain; skip on any ambiguity. Compose with SP1 (static methods,
default params → min/max arity, rest params) by treating arity as a **range** `[min, max]` (where
`max = None` means unbounded due to a rest param), flagging only `arg_count < min || arg_count >
max`.

**(a) Imported functions (`./mod` file modules + std).**
For a callee resolving to `BindingKind::Import`, resolve the import to the **exported function's
signature**:
- **File modules** (`from "./mod"`): the path is relative. Static analysis is currently **path-less**
  (`analyze`/`analyze_with_config` take only source text — see `unresolved_import.rs:14-19`). So
  cross-module file arity REQUIRES the workspace index (§4) OR a path-threaded analysis entry point.
  **Decision:** cross-file file-module arity is gated on the workspace index — it runs in the LSP and
  in a new path-aware `analyze_file` (used by `ascript check` which DOES have the file path). The
  existing path-less `analyze(src)` stays unchanged (and simply does not do cross-file arity).
- **std modules** (`from "std/math"`): std exports are native `Value` functions with no AScript AST,
  so their arity is not derivable from a CST. **Decision:** introduce a small, feature-independent
  **std arity table** (`src/check/std_arity.rs`) mapping `("std/math","abs") -> Arity::Exact(1)`,
  populated from the same authoritative source `unresolved_import` uses
  (`stdlib::std_module_exports` / `STD_MODULES`). Only entries that are *statically certain* (fixed
  arity native fns) get a row; variadic/overloaded std fns get `Arity::Unknown` (skip). This keeps
  the checker feature-independent (the table is data, not a feature-gated call).

**(b) Method calls `obj.m(args)` where the receiver's class is statically known.**
Flag only when the receiver's class is *certain*:
- Receiver is a `NameRef` whose binding is a `let`/`const` initialized **directly** from a constructor
  call `C(...)` of a uniquely-named file-local `class C`, OR is `self` inside a method of class `C`,
  AND `m` resolves to a unique `MethodDecl` named `m` on `C` (or an inherited one, walking `extends`).
  Look up the method's `ParamList` arity the same way `fixed_param_count` does.
- Skip on ANY uncertainty: reassigned receiver, receiver from a function return, dynamic/computed
  member, `C` not uniquely a class, method overridden ambiguously, `?.` optional call, super calls.

**(c) Constructor calls `C(args)` against the class's `init`/auto-init arity.**
- For a `CallExpr` whose callee `NameRef` resolves to a unique `BindingKind::Class` `C`:
  - If `C` declares `fn init(params)`, the constructor arity is `init`'s `ParamList` arity (same
    min/max/rest logic).
  - If `C` has NO `init`, arity is **0** (the default constructor). (SP1's auto-init/records are a
    non-goal here; if SP1 ships field-derived auto-init, this rule reads the same FieldDecl list —
    coordinate, but v1 assumes "no init ⇒ 0 args".)
  - Skip if `C` is imported (file-module → workspace index; std classes → skip), shadowed, or
    ambiguous.

### How zero-false-positive is preserved

The rule's bar is: **only flag a call whose callee signature is statically CERTAIN.** Each extension
keeps the existing `unique` + `resolves_to_*` discipline:
1. **Uniqueness gate** — the callee must resolve to exactly one declaration of the right kind
   (`Fn`/`Class`/`MethodDecl`), no shadowing binding sharing the name (the existing `resolves_to_fn`
   pattern, generalized to `resolves_to_class` / `resolves_to_method`).
2. **Receiver certainty** (methods) — only `self` or a `let`/`const` *directly* bound to `C(...)` is
   accepted; any indirection (return value, reassignment, parameter of unknown type) → skip. This is
   the strongest source of potential FPs, so it is the strictest gate.
3. **Arity as a range** — `Arity { min, max: Option<usize> }`. Rest param ⇒ `max = None`. SP1 default
   params (if/when added) ⇒ `min = required`, `max = total`. A spread arg in the call ⇒ skip
   (count unknown, existing behavior). Flag iff `argc < min || (max.is_some() && argc > max)`.
4. **Import resolution certainty** — std arity only from the curated table (uncertain ⇒ skip);
   file-module arity only via the workspace index where the target file parsed cleanly and exports
   exactly one fn of that name.
5. **Corpus zero-FP guard** — `ascript check examples/*.as examples/advanced/*.as` stays at 0
   diagnostics (the standing guard, e.g. `static_methods`-style examples), plus a new differential:
   any call flagged statically MUST be one the engine would panic on at runtime (a curated
   "should-flag" + "must-not-flag" corpus).

### Implementation

- Refactor the shared "callee resolves to the unique X at this decl range" helper out of
  `call_arity.rs` / `contract.rs` (both have identical `resolves_to_fn`) into
  `src/check/rules/mod.rs` as `resolves_to_unique(callee, name, decl_range, kind, resolved)`.
- Add `Arity { min: usize, max: Option<usize> }` + `arity_of(param_list_node) -> Arity` to
  `rules/mod.rs` (supersedes `fixed_param_count`; rest ⇒ `max=None`).
- `call_arity.rs`: add the three callee-shape branches; method/constructor lookup walks the
  `ClassDecl` body (`MethodDecl`/`FieldDecl`, mirroring `duplicate_member.rs:27`) and the `extends`
  chain for inherited methods/init.
- `src/check/std_arity.rs` (feature-independent): the curated std-fn arity table + `pub fn
  std_fn_arity(module: &str, name: &str) -> Option<Arity>`.
- Cross-file file-module arity is **wired through the workspace index** (§4): the index exposes
  `exported_fn_arity(module_path, name)`. A path-aware `analyze_file(path, src, index)` is the entry
  point `ascript check` uses; the LSP diagnostics path uses the same with its index. The path-less
  `analyze(src)` is unchanged and skips cross-file arity (documented).

### Tests

- Imported std fn wrong arity flagged (`import { abs } from "std/math"\nabs(1,2)` → flag); correct
  arity silent; a variadic/unknown std fn never flagged.
- Constructor: `class C { fn init(a,b) {} }\nC(1)` flagged; `class C {}\nC(1)` flagged (default ctor
  is 0-arg); `C(1,2)` for the 2-arg init silent.
- Method: `let c = C()\nc.m(1,2,3)` flagged when `C.m` takes 1; `self.m(...)` inside a method;
  inherited method via `extends`; reassigned receiver NOT flagged; return-value receiver NOT flagged.
- File-module (index-backed): a 2-arg exported fn imported and called with 1 → flagged; an import
  whose target file has a parse error → NOT flagged (uncertain).
- SP1 composition: static method `C.s(...)` arity; rest-param method ⇒ range, over-min silent.
- Zero-FP corpus guard green in BOTH feature configs.

---

## §3 — Cross-module diagnostic span provenance

### Current behavior (verified)

`AsError` (`src/error.rs:16-20`) carries `message`, `span: Option<Span>`, and `source:
Option<Rc<SourceInfo>>` where `SourceInfo { path, text }` (`error.rs:9-13`). The renderer
`diagnostics::report` (`src/diagnostics.rs:6-26`) renders the caret using **`err.source`'s** path +
text for **`err.span`**.

The bug: `AsError::with_source` only attaches source **if none is already set** ("the innermost
module's source wins" — `error.rs:39-46`). But a deferred error works the *other* way: module A
(`a.as`) defines `fn f` that panics; module B (`b.as`) imports and calls it. Each module is loaded
with its OWN `SourceInfo` (`interp.rs:898-913`, `load_module` builds `src_info` from the module's
path+text and attaches it on lex/parse error). At **runtime**, a panic raised while executing A's
body carries A's span. If the error propagates to B's top-level call site and gets `with_source(B)`
applied first, the caret is rendered against **B's text at A's byte offset** — right message, wrong
file/caret. The span (byte offsets) belongs to A; the source attached is B.

The root cause: **`span` and `source` are independent fields with no guarantee they refer to the same
file.** A span is meaningful only paired with the source it indexes.

### Target

A diagnostic's caret always points at the file the span belongs to. The span and its source-file are
**bound together at the point the error is raised** (in the module whose AST the span indexes), and
never re-sourced to a different module on the way up.

### Implementation

- **Bind source to span at raise time.** `with_source` already prefers the innermost (first-set)
  source — that is correct IF the innermost setter is the module that owns the span. Audit every
  `with_source` call site (`src/lib.rs` ×5, `src/repl.rs` ×2, `src/interp.rs:898`,
  `interp.rs:904/906/913`) and the panic-raising paths in `interp.rs` to ensure that when a panic is
  raised inside a loaded module's body, the module's `SourceInfo` is attached **at that frame**
  (before the error crosses the import boundary back to the caller). `load_module` attaches it on
  lex/parse (`interp.rs:904-913`) but a *runtime* panic inside the loaded module's executed body must
  also be sourced to that module — verify and add the attach at the module-body execution frame.
- **Make the span+source coupling explicit.** Change `AsError.span` to optionally carry its source
  inline so the renderer never pairs a span with the wrong text. Minimal change:
  add a `span_source: Option<Rc<SourceInfo>>` set at raise time, and have `report` prefer
  `span_source` over `source` for the caret (falling back to `source` for legacy single-module
  errors). This is additive and backward-compatible (single-module errors set both to the same
  thing). Document that `span` is only ever rendered against `span_source` (or `source` when they
  coincide), never against an outer module's text.
- **Renderer (`src/diagnostics.rs`):** use the span's bound source for the caret; if a chain of
  modules is involved, the message may note the importing file, but the caret renders in the file the
  span indexes. (ariadne supports multi-file reports if we later want a "called from" secondary
  label — out of scope for v1; v1 just gets the *primary* caret correct.)
- **Module source registry (research finding):** `Interp.modules` (`interp.rs:278`) is a
  `HashMap<PathBuf, ModuleEntry>`; `ModuleEntry` does not currently retain the module's `SourceInfo`
  for later rendering. To support a secondary "defined in / called from" label later, store the
  `Rc<SourceInfo>` on `ModuleEntry` (additive). v1 does not require it if the raise-time binding is
  correct, but it is the clean groundwork.

### Tests (`tests/cli.rs` / `tests/modules.rs`)

- A two-file repro: `a.as` defines a fn that panics at a known span; `b.as` imports + calls it.
  Running `b.as` renders the caret **in `a.as`** at the correct offset, with `a.as`'s source line
  shown (not `b.as`'s text). Assert the rendered report names `a.as` and shows A's line.
- Single-module errors are unchanged (regression): a panic in a standalone file still renders against
  that file (byte-exact, message + caret identical to today).
- A lex/parse error in an imported module still renders in that module (already correct — guard it).

---

## §4 — Cross-file LSP — full workspace index (owner decision)

### Current behavior (verified)

The LSP is single-file:
- `Backend` (`src/lsp/server.rs:15-18`) holds `documents: Mutex<HashMap<Url, String>>` — open-document
  text only, no cross-file structure.
- `analysis::definition(text, offset)` (`src/lsp/analysis.rs:592-616`) is **within-file only**
  (documented at `analysis.rs:589`): it lexes/parses the single buffer, resolves to an enclosing-fn
  local/param or a top-level decl in the same file, and returns a `Range` *in that same file*.
- `document_symbols` (`analysis.rs:142`) and `goto_definition` (`server.rs:139-157`) operate on the
  single open buffer; `goto_definition` returns a `Location` with the **same `uri`** it was queried
  with (`server.rs:153-156`) — it cannot point at another file.
- No `references` / `rename` providers; capabilities (`server.rs:39-52`) advertise only symbol /
  hover / completion / definition.
- The whole layer is `Send + Sync`, interpreter-free (`mod.rs:10-12`).

### Target — a persistent, incremental workspace index

Build a cross-file symbol index that powers: cross-file **go-to-definition**, **workspace symbols**
(`workspace/symbol`) + document symbols, **find-references**, and **rename** across files. Warm and
incremental: re-index a file on `didOpen`/`didChange`; reuse the existing resolver
(`src/syntax/resolve`) and pure `analysis` helpers. Stays static-analysis-only.

### Index design sketch

A new module `src/lsp/workspace.rs` (feature-gated under `lsp`, still no interpreter):

```text
WorkspaceIndex {
    // Per-file parsed + resolved facts, keyed by canonical path.
    files: HashMap<PathBuf, FileIndex>,
    // Symbol name -> every defining (path, range, kind) across the workspace.
    // Drives workspace/symbol and the def-lookup fast path.
    defs_by_name: HashMap<String, Vec<SymbolDef>>,
    // Import graph edges: importer -> [(specifier, resolved_path)].
    // Drives "which files import this module" for references/rename.
    import_edges: HashMap<PathBuf, Vec<ImportEdge>>,
    // Reverse edges: module -> importers (maintained alongside import_edges).
    importers: HashMap<PathBuf, HashSet<PathBuf>>,
}

FileIndex {
    text: String,
    // The file's exported symbol names (from `export` decls) -> def range+kind.
    exports: HashMap<String, SymbolDef>,
    // All top-level + nested decls in this file (the document-symbol set, reused).
    defs: Vec<SymbolDef>,
    // Resolved name-uses in this file: use-range -> Resolution (+ resolved path
    // for an imported name) so go-to-def is a table lookup.
    uses: Vec<UseSite>,
    // Parse/resolve success flag; on failure the file keeps its LAST good index
    // (so editing into a transient parse error doesn't blank out navigation).
}

SymbolDef { name, kind: SymbolKind, path: PathBuf, name_range: ByteSpan }
ImportEdge { specifier: String, resolved: Option<PathBuf>, names: Vec<String> }
UseSite { range: ByteSpan, target: ResolvedTarget }
ResolvedTarget = LocalDef(ByteSpan) | Imported { module: PathBuf, name: String } | Std{..} | Unknown
```

**Module resolution (reusing the runtime's rule statically):** mirror
`Interp::resolve_import` (`interp.rs:945-951`) — `dir.join(specifier)` with `.as` appended if no
extension — but **statically** (no fs read at resolve-time beyond discovery). The importer file's
own path provides the base dir (the LSP knows each document's `Url` → path). `std/*` specifiers
resolve to a synthetic "std module" target (navigable only to a doc stub or skipped), keeping the
checker's existing std-vs-file split (`unresolved_import.rs`).

**Discovery + warmth:** on `initialize`, capture the workspace root folder(s); on first use (or
`initialized`), walk the root for `*.as` files and build a `FileIndex` for each (parse + resolve via
the existing front-end). Keep it warm in `Backend` (a new `RwLock<WorkspaceIndex>` field alongside
`documents`, still `Send + Sync` — `WorkspaceIndex` holds only `String`/`PathBuf`/ranges, no `Rc`).

**Invalidation on edit:** `didChange`/`didOpen` re-index ONLY the changed file (re-parse, re-resolve,
rebuild that `FileIndex`, update `defs_by_name`/`import_edges`/`importers` deltas). A change to a
file's **exports** or **imports** marks its importers/importees for lazy re-resolution of cross-file
use targets (a file's *own* def/use table only depends on its own text + the *export name sets* of
its imports — so cross-file invalidation is bounded to import-edge neighbors, not the whole graph).
On a transient parse error, retain the last-good `FileIndex` so navigation degrades gracefully.

**Resolver reuse:** each `FileIndex` is built by calling the SAME `tree_builder::build_tree` +
`resolve::resolve` the checker uses (`analyze.rs:32-79`), then projecting `ResolveResult.uses` /
`bindings` into `UseSite` / `SymbolDef`. Imported-name uses (`BindingKind::Import`) are linked to the
target module's `exports` table via the import edge — that link is the cross-file def.

### Phasing (this is the largest part)

- **L1 — index core (no new providers).** `src/lsp/workspace.rs`: `WorkspaceIndex`, `FileIndex`,
  discovery, build-from-front-end, incremental re-index on `didChange`, import-edge resolution.
  `Backend` holds the index; `initialize` captures roots; unit tests on a fixture dir (build index,
  assert exports/edges/defs, re-index after an edit updates deltas, parse-error retains last-good).
- **L2 — cross-file go-to-definition.** Replace the single-file `definition` path for imported names:
  when the use at the cursor is an imported/cross-file name, return a `Location` with the **target
  file's `uri`** and the export's `name_range`. Same-file resolution still uses the existing
  `analysis::definition`. `goto_definition` (`server.rs`) returns the cross-file `Location`.
- **L3 — workspace + document symbols, find-references.** `workspace/symbol` over `defs_by_name`
  (advertise `workspace_symbol_provider`); keep `document_symbol` as-is but sourced from the index.
  **References:** for a def, scan `uses` across files whose `import_edges` reach the def's module
  (plus the def's own file) for `UseSite`s targeting it; advertise `references_provider`.
- **L4 — rename across files.** Build a `WorkspaceEdit` from the reference set: rename the def's
  `name_range` + every referencing `UseSite` + every import clause that names it. **Scope (v1):**
  the def's defining file + its direct importers (the `importers` reverse edges). Guard: refuse
  rename if any target file currently has a parse error (the edit would be unsafe), or if the new
  name collides with an existing binding in a touched scope. Advertise `rename_provider` (with
  `prepare` support to reject non-renameable positions). Tests: rename an exported fn updates its
  decl, its importers' import clauses, and their use sites; collision/parse-error refusal.

### Tests (`tests/lsp.rs` + `src/lsp/workspace.rs` unit)

- Index build over a 3-file fixture (a defines, b imports+uses, c unrelated): `defs_by_name`,
  `exports`, `import_edges`, `importers` correct.
- Incremental: edit `a`'s export name → `b`'s cross-file use target updates; edit `c` → `a`/`b`
  indices untouched.
- Parse-error resilience: typing a syntax error into `b` retains b's last-good index (navigation
  still works against the prior text until it parses again).
- Cross-file go-to-def: cursor on `b`'s use of `a`'s exported fn → `Location` in `a.as`.
- `workspace/symbol` returns matches across files; find-references finds uses in importers; rename
  rewrites decl + import clauses + uses, refuses on collision/parse-error.
- `Send + Sync` is preserved (a compile-time assertion that `Backend` and `WorkspaceIndex` are
  `Send + Sync`; no interpreter type leaks in).

---

## Testing & quality bar (whole sub-project)

- **Both feature configs green:** `cargo test` (default, full stdlib) AND `cargo test
  --no-default-features` (core). The checker core + `--fix` + std-arity table must build and pass
  under `--no-default-features`. The LSP is `lsp`-gated; its tests run in the default config.
- **Clippy clean** under `--all-targets` AND `--no-default-features --all-targets`.
- **No `unsafe`, no `#[allow]`, no `#[ignore]`, no stubs.** Every deferral is a documented,
  owner-noted scope cut (per-rule autofix decision; cross-file file-arity gated on the index;
  rename scoped to direct importers in v1) — never a silent drop.
- **Zero-false-positive guard:** `ascript check examples/*.as examples/advanced/*.as` → 0
  diagnostics, in both feature configs (the standing corpus guard; SP4's new arity branches must not
  introduce a single FP).
- **Idempotence guard:** `--fix` applied twice == once.
- **`Send + Sync` guard:** the LSP layer never instantiates the interpreter and never holds
  `Rc`/`RefCell`/`Value`.
- **Per-task commit** with trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
  Independent per-phase review (re-read spec, re-run gates, adversarial FP hunt on the new arity
  surface and rename edits) before sign-off.
- **Docs:** update `docs/content` (the `ascript check` / tooling page: `--fix`, cross-module arity,
  editor cross-file navigation) and `README.md`'s CLI section.

## File-touch map (for the plan)

| Area | Files |
|---|---|
| Diagnostic / fix model | `src/check/diagnostic.rs` (model, present), `src/check/fix.rs` (NEW — apply/collect/diff), `src/check/rules/unused.rs` (correct import fix range) |
| Cross-module arity | `src/check/rules/call_arity.rs`, `src/check/rules/mod.rs` (shared `resolves_to_unique`, `Arity`), `src/check/std_arity.rs` (NEW), `src/check/analyze.rs` (path-aware `analyze_file`) |
| Span provenance | `src/error.rs` (`span_source`), `src/diagnostics.rs` (render against span's source), `src/interp.rs` (raise-time source binding; `ModuleEntry` source) |
| Workspace LSP | `src/lsp/workspace.rs` (NEW — index), `src/lsp/analysis.rs` (cross-file def), `src/lsp/server.rs` (index field, new providers + capabilities), `src/lsp/mod.rs` |
| CLI | `src/main.rs` (`--fix`/`--fix-dry-run`, post-fix exit logic; path-aware check), `src/lint_config_toml.rs` (unchanged unless a `[fix]` table is added — deferred) |
| Tests | `src/check/fix.rs` (unit), `tests/cli.rs`, `tests/modules.rs`, `tests/lsp.rs`, `src/lsp/workspace.rs` (unit) |
| Docs | `docs/content/*` (tooling page), `README.md` |

## Open design questions for the owner

1. **`--fix` exit code semantics.** After `--fix` resolves a file's only issue, should the command
   exit 0 (issue fixed) or still surface the *pre-fix* finding? The spec proposes re-evaluating exit
   against the **post-fix** analysis (exit 0 if nothing remains). Confirm — this differs from some
   linters (e.g. `eslint --fix` exits non-zero if any *unfixable* issue remains, which the proposal
   already honors, but exits 0 when everything was fixable).
2. **Std arity table maintenance.** The curated `std_arity.rs` table is hand-maintained and can drift
   from the real native signatures. Acceptable for v1 (it only ever *skips* on a missing/`Unknown`
   entry, never false-positives), but the owner may prefer a generated table or a `#[test]` that
   cross-checks every listed entry against `std_module_exports`. Which?
3. **File-module arity in path-less `analyze(src)`.** The decision gates cross-file file arity on the
   workspace index / path-aware `analyze_file`, leaving `analyze(src)` (the library entry the LSP
   *diagnostics* and unit tests use) without it. Confirm it is acceptable that bare `analyze(src)`
   does std + intra-file + method/ctor arity but NOT cross-file *file* arity.
4. **Rename scope.** v1 scopes rename to the def's file + direct importers. A transitively-re-exported
   symbol (if AScript supports re-export — verify) would need transitive closure. Confirm direct
   importers is sufficient for v1, or whether re-export chains must be followed.
