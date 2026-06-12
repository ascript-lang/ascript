# Stdlib Signature Table + LSP Enrichment + Audit Hardening (SIG) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Build the drift-tested stdlib signature table (`src/check/std_sigs.rs`) covering
every `STD_MODULES` export + the global builtins; wire it into THREE consumers — signature
help (stdlib member calls, typed-receiver methods, cross-file imported fns, builtins, plus the
kept same-file path), completion (real kind/detail/docs, cached + deprioritized auto-import),
and hover on stdlib members; subsume `src/check/std_arity.rs` into the table; and land the
eight remaining audit-hardening items (C1–C8). LSP/checker-static only — zero engine surface,
`vm_differential` untouched and re-run once as proof.

**Spec:** `superpowers/specs/2026-06-12-lsp-stdlib-signatures-design.md` (SIG). Read it first;
section references (§) below are into it.

**Architecture:** Unit A is a leaf static-data module `src/check/std_sigs.rs` (beside
`std_arity.rs`, feature-independent, builds under `--no-default-features`) + two drift-test
families (export completeness both directions; docs-notation consistency over
`docs/content/stdlib/*.md`). Unit B consumers: `providers/signature.rs` grows a resolution
ladder over an extended `enclosing_call` (MemberExpr callees) + a `WorkspaceIndex::
exported_fn_signature` name-bearing walk; `providers/completion.rs` C2/resolve/auto-import
enrichment; `providers/hover.rs` member branch. Unit C touches `server.rs` (yield, folders,
snippet capability, poisoned-lock log), `workspace.rs` (fs-canonicalize), `completion.rs`
(partial-identifier, string/comment suppression), `model.rs` + `check/infer` (the
`OnceLock<InferCache>`).

**Tech stack:** Rust; tower-lsp 0.20 over stdio on a current_thread runtime (handlers
interleave only at `.await` — never add blocking loops without yields); cstree CST +
`syntax::resolve`; `check::infer` (static-only, never runs code); tests via provider
`#[cfg(test)]` units + the `tests/lsp.rs` spawn-the-binary JSON-RPC harness; clippy both
feature configs.

**Hard sequencing gate (Phase 0):** this plan executes AFTER the PERF engine waves per
`goal-perf.md` ("SIG (DX track) — owner-sequenced after the engine waves") AND after the
2026-06-12 LSP reliability fixes (pending-flush, fold race, STD_MODULES derivation, did_close
purge — uncommitted at spec time) are merged. Task 0.1 verifies both and re-greps every cited
line number.

**Binding execution standards (production-grade mandate):** any bug found while working — ours
or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first regression
guard, never stepped around (goal.md Gate 14). No placeholders, no silent deferrals. Commit per
task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `src/check/std_sigs.rs` — `StdParam`/`StdSig`/`MemberKind`, the `sig!` macro, the curated
  table, `std_sig`/`builtin_sig`/`module_members`, completeness drift tests.
- `tests/std_sigs_docs.rs` — the docs-notation parser + docs-consistency drift test (reads
  `docs/content/stdlib/*.md` via `env!("CARGO_MANIFEST_DIR")`; integration-test binary so the
  doc files are read from the repo, not embedded).

**Modified files:**
- `src/check/std_arity.rs` — becomes a thin derivation over `std_sigs` (API unchanged).
- `src/lsp/providers/signature.rs` — MemberExpr callees + the resolution ladder + LabelOffsets
  + docs + variadic clamp.
- `src/lsp/providers/completion.rs` — C2 kind/detail/data, resolve docs, cached + sorted
  auto-import, partial-identifier alias (C1), string/comment suppression (C8), shared
  `namespace_import_module` promotion.
- `src/lsp/providers/hover.rs` — stdlib-member branch.
- `src/lsp/server.rs` — signature handler passes index+path; workspace_diagnostic yield +
  model reuse (C2); folder-removal unindex (C4); snippet capability capture (C7);
  poisoned-lock one-time log (C6 half); typeHierarchy decision comment (C6 half).
- `src/lsp/workspace.rs` — `exported_fn_signature` (names+annotations), fs-canonicalize (C5).
- `src/lsp/model.rs` + `src/check/infer/mod.rs` — `OnceLock<InferCache>` + cache-aware entry
  points (C3); hover size-class gate.
- `tests/lsp.rs` — integration tests per the §5 matrix.
- `docs/content/tooling/lsp-capabilities.md`, `CLAUDE.md`, `superpowers/roadmap.md`,
  `goal-perf.md` — final docs/status task.

---

## Phase 0 — Preflight: sequencing gate + citation re-verification

### Task 0.1: verify the dependencies and re-grep the cited lines

**Files:** none (verification only; findings recorded in the task log).

- [ ] **Step 1 — sequencing gate:** confirm with the owner record that the PERF engine waves
  this plan is sequenced after have reached their merge points per `goal-perf.md`'s status
  table (SIG is technically independent — if the owner green-lights early execution, record
  that decision here and proceed). **If neither holds, STOP and escalate.**
- [ ] **Step 2 — reliability-fix gate:** on the branch base, verify the 2026-06-12 fixes are
  MERGED (not just in a working tree):
  `git log --oneline -5 -- src/lsp/server.rs` shows the reliability-fix commit(s);
  `grep -n "flush_pending_for" src/lsp/server.rs` (expect the helper ~`:112` and call sites in
  the hover/completion/signature handlers); `grep -n "ONE \`pending\` critical section\|one
  .pending. critical section" src/lsp/server.rs` (the did_change invariant comment);
  `grep -n "use crate::stdlib::STD_MODULES" src/lsp/providers/completion.rs`;
  `grep -n "pending.lock().await.remove" src/lsp/server.rs` (did_close purge). **If any is
  missing, STOP — this plan is blocked; escalate.**
- [ ] **Step 3 — citation re-grep:** re-locate every spec §8 citation that later tasks edit
  (line numbers may have drifted since spec time):
  `grep -n "SyntaxKind::NameRef" src/lsp/providers/signature.rs`;
  `grep -n "fn member_access_alias\|fn namespace_import_module" src/lsp/providers/completion.rs`;
  `grep -n "fn exported_fn_arity\|fn canonicalize" src/lsp/workspace.rs`;
  `grep -n "fn required_args" src/check/std_arity.rs`;
  `grep -n "std_arity::std_fn_arity" src/check/rules/call_arity.rs`;
  `grep -n "fn workspace_diagnostic\|fn did_change_workspace_folders\|async fn initialize"
  src/lsp/server.rs`;
  `grep -n "pub fn hover_type_at" src/check/infer/mod.rs`.
  Record the ACTUAL current numbers and use THEM throughout (the spec's numbers are
  reconciled here, never assumed).
- [ ] **Step 4 — baseline runs:** `cargo test --test lsp` green; `cargo test --test
  vm_differential` green (this is the BEFORE half of the "vm_differential untouched" proof);
  `cargo clippy --all-targets` and `cargo clippy --no-default-features --all-targets` clean.
  Create the feature branch `feat/lsp-stdlib-signatures` off `main`.

### Task 0.2: Phase 0 review

- [ ] **Step 1:** Independent reviewer re-runs Steps 2–4 and confirms the recorded line
  numbers match the tree. Any mismatch is corrected in the task log before Phase 1.

---

## Phase 1 — Unit A: the signature table

### Task 1.1: `std_sigs.rs` skeleton + the `sig!` macro + the first three modules

**Files:** create `src/check/std_sigs.rs`; modify `src/check/mod.rs` (declare `pub mod
std_sigs;` beside the existing `pub mod std_arity;` at `mod.rs:11`).

- [ ] **Step 1: Write the failing test** — in `src/check/std_sigs.rs` (committed with the
  implementation; run first against an empty table to see it fail):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// §2.3 drift (a), direction 1: every export of every buildable module has a
    /// table row, kind-consistent with the export's Value kind.
    #[test]
    fn every_export_has_a_table_row_with_consistent_kind() {
        for module in crate::stdlib::STD_MODULES {
            let Some(exports) = crate::stdlib::std_module_exports(module) else {
                continue; // feature-gated out of THIS build — covered by the other config
            };
            let members = module_members(module).unwrap_or_else(|| {
                panic!("std_sigs has no member list for {module}")
            });
            for (name, value) in &exports {
                let kind = members
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, k)| k)
                    .unwrap_or_else(|| panic!("{module}::{name} export missing from std_sigs"));
                let is_fn = matches!(value, crate::value::Value::Builtin(_));
                match kind {
                    MemberKind::Fn => {
                        assert!(is_fn, "{module}::{name}: table says Fn, export is a constant");
                        assert!(
                            std_sig(module, name).is_some(),
                            "{module}::{name}: MemberKind::Fn but no StdSig row"
                        );
                    }
                    MemberKind::Const(_) => {
                        assert!(!is_fn, "{module}::{name}: table says Const, export is a Builtin");
                    }
                }
            }
        }
    }

    /// §2.3 drift (a), direction 2: every table key is a real export (inherited
    /// from std_arity's drift guard; handle-method rows are flagged and skipped).
    #[test]
    fn every_table_row_is_a_real_export() {
        for (module, members) in all_modules() {
            let Some(exports) = crate::stdlib::std_module_exports(module) else {
                continue;
            };
            for (name, kind) in *members {
                if matches!(kind, MemberKind::HandleMethod) {
                    continue; // documented receiver method, not a module export (§2.4)
                }
                assert!(
                    exports.iter().any(|(n, _)| n == name),
                    "std_sigs lists {module}::{name} but it is not an export"
                );
            }
        }
    }
}
```

- [ ] **Step 2: Run — expect FAIL** (`module_members` returns `None` for everything).
- [ ] **Step 3: Implement the skeleton** — the types from spec §2.1 (`StdParam`, `StdSig`,
  `MemberKind { Fn, Const(&'static str), HandleMethod }`), the lookup fns, an
  `all_modules()` iterator over per-module `&'static` slices, and the authoring macro:

```rust
//! SIG §2 — the curated stdlib signature table. The MACHINE source of truth for
//! std fn signatures (the docs pages are the social source; the drift tests in
//! this file + `tests/std_sigs_docs.rs` bind the two). Feature-INDEPENDENT pure
//! data: builds under `--no-default-features`; carries ALL modules regardless of
//! the binary's compiled features (the `STD_MODULES` philosophy). Consumed by the
//! LSP (signature help / completion / hover) and by `std_arity` (the call-arity
//! lint's min-arity derivation — SIG §2.5).

/// `sig_entry!(name, (a: "t", b?: "t", ...rest: "t") -> "ret", "Doc line.")`
/// expands to a `(&str, StdSig)` row. `?` marks an optional trailing param,
/// `...` a variadic collector, `= "lit"` a rendered default.
macro_rules! sig_entry { /* … param-list muncher … */ }
```

  Fill in the first three modules transcribed from `docs/content/stdlib/collections.md`
  (Style 1 — read the page section per fn, copy the first sentence as `doc`):
  **`std/math`** (incl. `("pi", MemberKind::Const("float"))`, `("e", …)`,
  `sig_entry!(pow, (base: "number", exp: "number") -> "float", "Raise a base to an
  exponent.")`, `sig_entry!(min, (...nums: "number") -> "float", "Return the smallest of one
  or more arguments.")`), **`std/string`** (incl. `slice` with `end?: "number"`), and
  **`std/array`** (incl. `map: (arr: "array", f: "fn(item)") -> "array"`).
  Temporarily scope the completeness test to the three filled modules via an
  `IMPLEMENTED_MODULES` list the test iterates — **with a companion test asserting
  `IMPLEMENTED_MODULES.len() < STD_MODULES.len()` is ONLY allowed while a
  `// SIG Task 1.2 fills the remainder` marker exists** (the marker is deleted in Task 1.2,
  flipping the test to full coverage — no silent partial table can survive the phase).
- [ ] **Step 4: Run — expect PASS** for the three modules; clippy both configs clean
  (`std_sigs` must compile under `--no-default-features` — the test's `std_module_exports`
  calls are themselves feature-aware so no cfg gymnastics are needed).
- [ ] **Step 5: Commit** — `git commit -m "feat(check/sigs): std_sigs skeleton + sig! macro + math/string/array rows (SIG §2.1)"`.
- [ ] **Step 6: Independent review** — reviewer verifies the three modules against the docs
  page line-by-line (names, optionality, returns, doc sentences), probes the macro with an
  optional-before-required param (must not compile or must be rejected by a debug assertion),
  and confirms the partial-coverage marker mechanism cannot be left in silently.

### Task 1.2: fill the full table (all ~56 modules + the 10 builtins)

**Files:** modify `src/check/std_sigs.rs`.

- [ ] **Step 1: Flip the failing test** — delete the `IMPLEMENTED_MODULES` scoping + marker
  so `every_export_has_a_table_row_with_consistent_kind` iterates ALL of `STD_MODULES`; add
  the builtin test:

```rust
#[test]
fn global_builtins_have_sigs() {
    for b in ["print", "len", "type", "assert", "range", "Ok", "Err", "recover", "test", "exit"] {
        assert!(builtin_sig(b).is_some(), "builtin {b} missing");
    }
    let len = builtin_sig("len").unwrap();
    assert_eq!(len.params.len(), 1);
    assert_eq!(len.params[0].name, "value");
}
```

- [ ] **Step 2: Run — expect FAIL** (≈50 modules missing).
- [ ] **Step 3: Implement** — module by module, transcribing from the §2.2 page map
  (collections/data/system/net/db/log/schema/cli are Style-1 bullets; stream/assert/async/
  time are Style-2 backticked headings; utilities/caps/ffi/shared/tui/bench/workflow/
  telemetry/ai are prose/tables — for those, read the module's `src/stdlib/<mod>.rs`
  `exports()` + `call()` arg handling as the authority and the prose for the doc line).
  Handle/receiver methods documented in tables (db `conn.*`, lru/events/template handles,
  ffi `symbol`/`call`) get `MemberKind::HandleMethod` rows + `StdSig`s keyed under their
  module (§2.4). Builtin sigs sourced from the language-guide docs + `interp.rs` builtin
  arg handling. **This is the bulk-labor task — budget it as such; correctness over speed;
  every doc line is a real first sentence, never invented.**
- [ ] **Step 4: Run — expect PASS** in BOTH feature configs (`cargo test std_sigs` and
  `cargo test --no-default-features std_sigs`) — the completeness test under
  `--no-default-features` exercises the core-module subset; under default it exercises all
  default-gated modules. (`http3`-only surface: none — reqwest H3 adds no exports.)
- [ ] **Step 5: Commit** — `git commit -m "feat(check/sigs): full curated table — all STD_MODULES exports + global builtins (SIG §2.4)"`.
- [ ] **Step 6: Independent review** — reviewer spot-audits 3 modules of their choosing
  end-to-end against docs AND `src/stdlib/<mod>.rs` arg handling (esp. optionals: an
  `arg(args, i)`-is-nil-tolerant trailing param must be `optional: true`), greps for any
  empty `doc: ""`, and runs both feature-config test passes.

### Task 1.3: the docs-consistency drift test (`tests/std_sigs_docs.rs`)

**Files:** create `tests/std_sigs_docs.rs`.

- [ ] **Step 1: Write the failing test** — the parser + matcher per spec §2.3:

```rust
//! SIG §2.3 drift (b): the docs pages and the curated table may never contradict.
//! Style-1 entries (`### module.fn` + `- name: type` bullets + `Returns:`) yield
//! full facts; Style-2 (### `module.fn(a, b?, ...r)`) yield name/optionality facts;
//! Style-3 (prose/tables/bare headings) yield nothing and are TOLERATED. Any
//! extracted fact that contradicts the table fails with page:line + both renderings.

use ascript::check::std_sigs;

struct DocFact {
    page: &'static str,
    line: usize,
    module: String,
    func: String,
    params: Vec<DocParam>, // name, optional, variadic, ty: Option<String>
    ret: Option<String>,
}

fn parse_page(page: &'static str, text: &str) -> Vec<DocFact> { /* §2.2 styles 1+2 */ }

#[test]
fn docs_and_table_never_contradict() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/content/stdlib");
    let mut facts = Vec::new();
    for entry in std::fs::read_dir(dir).expect("docs dir") { /* read each .md, parse_page */ }
    assert!(facts.len() > 200, "parser regression: only {} facts extracted", facts.len());
    for f in &facts {
        let Some(sig) = std_sigs::std_sig(&format!("std/{}", f.module), &f.func) else {
            panic!("{}:{}: docs document {}.{} but std_sigs has no row",
                   f.page, f.line, f.module, f.func);
        };
        // names in order; optional/variadic flags; ret only when both sides state it.
        compare(f, sig);
    }
}

/// §2.3: every Fn member of a Style-1 module must have a docs fact (stale-docs guard).
#[test]
fn style1_modules_are_fully_documented() { /* per the STYLE1_MODULES const */ }

/// Self-test: a deliberately mutated fact MUST trip the comparator (anti-false-green).
#[test]
fn comparator_detects_a_contradiction() { /* flip one optional flag, assert panic */ }
```

  Parser notes grounded in the survey: Style-1 headings are `^### ([a-z_]+)\.([a-zA-Z_]+)$`;
  bullets `^- \x60?(\.\.\.)?([a-zA-Z_]+)\x60?(\??): ?(.*?)( — .*)?$` with `(optional)`
  detection in the tail; `- Returns:` lines; Style-2 headings are
  `` ^### `([a-z_]+)\.([a-zA-Z_]+)\(([^)]*)\)`$ `` with `?`-suffix optionals and `...`
  variadics in the inline list. The module string maps `std/<module>`; `net.md`'s `tcp.`/
  `udp.`/`ws.` prefixes map to `std/net/tcp` etc. (a small alias table in the test).
- [ ] **Step 2: Run — expect FAIL** (parser exists, table/doc mismatches surface — fix the
  TABLE where the docs are right; fix the DOCS where the code is right, in the same commit,
  per the production-grade mandate; a docs fix is a normal docs edit, no NAV change).
- [ ] **Step 3: Iterate to green.** Record any genuine docs bugs found (wrong param name in
  a docs page vs the code) in the commit message.
- [ ] **Step 4: Run — expect PASS**; fact-count floor holds (>200).
- [ ] **Step 5: Commit** — `git commit -m "test(check/sigs): docs-notation drift test — table ⇄ docs/content/stdlib (SIG §2.3)"`.
- [ ] **Step 6: Independent review** — reviewer mutates one docs bullet (`s: string` →
  `str: string`) locally and confirms the test trips with a useful message; confirms the
  fact-count floor and the comparator self-test; verifies the parser tolerates every Style-3
  page without facts (no panic, no false fact).

### Task 1.4: subsume `std_arity.rs` (one source of truth)

**Files:** modify `src/check/std_arity.rs`.

- [ ] **Step 1: Write the failing test** — in `std_arity.rs` tests, BEFORE changing the
  implementation, pin the legacy surface:

```rust
/// SIG §2.5: the derivation must reproduce the legacy curated arities EXACTLY
/// (no call-arity behavior change for previously-covered fns).
#[test]
fn derivation_matches_every_legacy_entry() {
    let legacy: &[(&str, &str, usize)] = &[
        ("std/math", "abs", 1), ("std/math", "floor", 1), ("std/math", "ceil", 1),
        ("std/math", "round", 1), ("std/math", "trunc", 1), ("std/math", "sign", 1),
        ("std/math", "sqrt", 1), ("std/math", "pow", 2), ("std/math", "floordiv", 2),
        ("std/math", "divmod", 2), ("std/math", "ceildiv", 2), ("std/math", "popcount", 1),
        ("std/math", "leading_zeros", 1), ("std/math", "trailing_zeros", 1),
        ("std/math", "rotl", 2), ("std/math", "rotr", 2),
        ("std/caps", "has", 1), ("std/caps", "list", 0), ("std/caps", "drop", 1),
        ("std/caps", "dropAll", 0), ("std/shared", "freeze", 1), ("std/shared", "isShared", 1),
        ("std/ffi", "open", 1), ("std/ffi", "struct", 1), ("std/ffi", "cstr", 1),
        ("std/ffi", "read_cstr", 1), ("std/ffi", "alloc", 1), ("std/ffi", "get", 3),
        ("std/ffi", "set", 4), ("std/ffi", "symbol", 3), ("std/ffi", "call", 1),
        ("std/task", "pipe", 2), ("std/string", "codepoints", 1),
        ("std/string", "from_codepoints", 1), ("std/string", "code_at", 2),
        ("std/assert", "deepEq", 2), ("std/assert", "matches", 2),
        ("std/assert", "throwsWith", 2),
    ];
    for (m, n, min) in legacy {
        let a = std_fn_arity(m, n).unwrap_or_else(|| panic!("{m}::{n} lost its arity"));
        assert_eq!(a.min, *min, "{m}::{n} min drifted");
        assert_eq!(a.max, None, "{m}::{n} must keep max=None (zero-FP contract)");
    }
}
```

- [ ] **Step 2: Run — expect PASS against the legacy table** (it pins today), then…
- [ ] **Step 3: Implement** — replace `required_args` with the derivation:

```rust
/// SIG §2.5: min-arity DERIVED from the curated signature table — the single
/// source of truth. `min` = the leading run of non-optional, non-variadic params;
/// `max = None` ALWAYS (the zero-false-positive contract above is unchanged:
/// native fns ignore surplus args, so only too-few is a guaranteed panic).
pub(crate) fn std_fn_arity(module: &str, name: &str) -> Option<Arity> {
    let sig = crate::check::std_sigs::std_sig(module, name)?;
    let min = sig
        .params
        .iter()
        .take_while(|p| !p.optional && !p.variadic)
        .count();
    Some(Arity { min, max: None })
}
```

  Delete the hardcoded list + the old `every_entry_is_a_real_export` test (superseded by the
  strictly-stronger Task 1.1 completeness pair — note this in the commit message, Gate 7:
  coverage moved, not deleted). Keep the module-doc zero-FP contract text.
- [ ] **Step 4: Run — expect PASS:** the legacy-parity test, `cargo test call_arity`, AND
  the Gate-5 corpus check: `cargo run -- check` over `examples/**` (both feature configs)
  emits **zero new diagnostics** — the derivation widens lint coverage from ~36 to every
  curated fn, and the corpus is the FP tripwire. A new corpus diagnostic = a wrong
  `optional` flag in the table → fix the table row (it will also trip the docs drift test).
- [ ] **Step 5: Commit** — `git commit -m "refactor(check): std_arity derives from std_sigs — one source of truth (SIG §2.5)"`.
- [ ] **Step 6: Independent review** — reviewer probes the widened coverage adversarially:
  picks 5 newly-covered fns with optional params (e.g. `string.slice` end?), writes
  throwaway `.as` snippets calling each with the minimal arg count (must NOT flag) and one
  below it (MUST flag), runs `ascript check` on them; re-runs Gate 5 on `examples/**`.

### Task 1.5: Phase 1 holistic review

- [ ] **Step 1:** Holistic subagent over the combined Unit A diff: table internally
  consistent (no `optional` after-required violations — add a debug assertion or test if the
  macro doesn't enforce it), drift tests genuinely bidirectional, std_arity behavior pinned,
  clippy + full `cargo test` + `--no-default-features` green, Gate 5 zero corpus diagnostics.
- [ ] **Step 2:** Findings fixed in-phase before Phase 2.

---

## Phase 2 — Unit B: the three consumers

### Task 2.1: signature help — the resolution ladder (`signature.rs`)

**Files:** modify `src/lsp/providers/signature.rs`, `src/lsp/providers/completion.rs`
(promote `namespace_import_module` to `pub(crate)`), `src/lsp/server.rs` (handler passes
index + path), `src/lsp/workspace.rs` (`exported_fn_signature`).

- [ ] **Step 1: Write the failing unit tests** — in `signature.rs` tests (the existing
  `model(src)` helper):

```rust
#[test]
fn stdlib_member_call_shows_signature_with_docs() {
    let src = "import * as math from \"std/math\"\nmath.pow(2, \n";
    let m = model(src);
    let off = src.rfind("pow(").unwrap() + "pow(".len();
    let help = signature_help(&m, off, None, None).expect("help");
    let sig = &help.signatures[0];
    assert_eq!(sig.label, "math.pow(base: number, exp: number) -> float");
    assert_eq!(help.active_parameter, Some(0));
    assert!(matches!(&sig.parameters.as_ref().unwrap()[0].label,
        ParameterLabel::LabelOffsets([s, e]) if e > s));
    assert!(format!("{:?}", sig.documentation).contains("Raise a base"));
    // comma advances:
    let off2 = src.rfind(", ").unwrap() + 2;
    let help2 = signature_help(&m, off2, None, None).expect("help");
    assert_eq!(help2.active_parameter, Some(1));
}

#[test]
fn builtin_call_shows_signature() {
    let src = "print(1)\n";
    let m = model(src);
    let off = src.find("print(").unwrap() + "print(".len();
    let help = signature_help(&m, off, None, None).expect("builtin help");
    assert!(help.signatures[0].label.starts_with("print("));
}

#[test]
fn method_on_typed_receiver_shows_named_params() {
    let src = "class C {\n  fn m(count: int, label) { return count }\n}\nlet c = C()\nc.m(\n";
    let m = model(src);
    let off = src.rfind("c.m(").unwrap() + "c.m(".len();
    let help = signature_help(&m, off, None, None).expect("method help");
    assert_eq!(help.signatures[0].label, "m(count: int, label)");
}

#[test]
fn variadic_active_param_clamps_to_rest() {
    let src = "import * as math from \"std/math\"\nmath.min(1, 2, 3\n";
    let m = model(src);
    let off = src.rfind('3').unwrap();
    let help = signature_help(&m, off, None, None).expect("variadic help");
    assert_eq!(help.active_parameter, Some(0), "clamped to ...nums");
}

#[test]
fn unknown_member_returns_none() {
    let src = "import * as math from \"std/math\"\nmath.nosuch(1\n";
    let m = model(src);
    let off = src.rfind("nosuch(").unwrap() + "nosuch(".len();
    assert!(signature_help(&m, off, None, None).is_none());
}

#[test]
fn same_file_fn_still_wins_over_builtin_shadow() {
    let src = "fn print(a) {}\nprint(1)\n"; // user fn shadows the builtin
    let m = model(src);
    let off = src.rfind("print(").unwrap() + "print(".len();
    assert_eq!(signature_help(&m, off, None, None).unwrap().signatures[0].label, "print(a)");
}
```

- [ ] **Step 2: Run — expect FAIL** (signature mismatch: new `signature_help` arity; then
  behavior failures).
- [ ] **Step 3: Implement** —
  - `enclosing_call` returns a `Callee` enum: `Named(String)` (today) or
    `Member { receiver: String, property: String }` (a `MemberExpr` callee whose object is a
    `NameRef` — `call.children().find(MemberExpr)`, object = first expr child, property via
    the same `member_property_name` discipline `call_arity.rs:175` uses; `OptMemberExpr`
    excluded). Keep the innermost-arg-list selection logic verbatim.
  - The ladder (spec §3.1 order): same-file unique FnDecl → `builtin_sig` →
    `exported_fn_signature` via the index (unique file-module import; reuse the
    `file_module_arity` import-mapping discipline) → namespace import + `std_sig` → typed
    receiver (`receiver_class_info`-style resolution; param names from the same-file
    `ClassDecl→MethodDecl→ParamList` walk, annotation text from the CST, `method_sigs` +
    `CheckTy::display` as the fallback type renderer).
  - `workspace.rs`: `exported_fn_signature(module, name) -> Option<ExportedFnSig>` —
    extend the `exported_fn_arity:419-445` walk to collect, per `Param`: the `Ident` text,
    the annotation node's text (if any), `has_default` (→ `optional`), `DotDotDot`
    (→ variadic). `exported_fn_arity` becomes `exported_fn_signature(..).map(|s| s.arity())`.
  - `make_help` rebuilt: label per §3.1 rendering; `ParameterLabel::LabelOffsets` with
    UTF-16 offsets computed while building the label; `documentation` from
    `StdSig.doc` / the user `///` doc line (reuse the `docs` provider's doc-comment
    extraction); variadic clamp on the active index.
  - `server.rs` signature handler: `let idx = self.index.read().ok();` +
    `url_to_canon(&uri)`; pass `idx.as_deref()` + path. (The read guard is held across a
    synchronous provider call only — no await inside; note this in a comment.)
- [ ] **Step 4: Run — expect PASS** (all unit tests incl. the 3 pre-existing ones,
  updated only for the new arity); clippy both configs.
- [ ] **Step 5: Commit** — `git commit -m "feat(lsp/sig): signature-help resolution ladder — stdlib members, builtins, typed receivers, cross-file fns (SIG §3.1)"`.
- [ ] **Step 6: Independent review** — reviewer probes: nested calls
  (`math.pow(math.abs(x), 2)` — inner/outer arg lists pick the innermost), a shadowed
  namespace alias (`let math = 1` after the import — receiver-typed rung must not
  misfire into the namespace rung), `obj.m(` where `m` doesn't exist on the class (None),
  a two-FnDecl ambiguous same-file name falling through to the builtin rung only when the
  name IS a builtin, zero-arg signatures, and `f()? - 1`-style propagate near an arg list.

### Task 2.2: cross-file imported fn signature — integration proof

**Files:** modify `tests/lsp.rs`.

- [ ] **Step 1: Write the failing integration test** — temp-dir workspace, two files, the
  existing harness idiom (`tests/lsp.rs` `LspClient::spawn` + `initialize` with a real
  `rootUri` + `didOpen`):

```rust
/// SIG §3.1(c): signature help for a cross-file imported user fn shows param NAMES
/// + annotations (the index's ParamList walk now returns names, not just arity).
#[test]
fn lsp_signature_help_cross_file_imported_fn() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(
        dir.path().join("util.as"),
        "export fn add(first: number, second: number = 0) { return first + second }\n",
    )
    .unwrap();
    let main_path = dir.path().join("main.as");
    let main_text = "import { add } from \"./util\"\nadd(\n";
    std::fs::write(&main_path, main_text).unwrap();

    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    let root = url::Url::from_directory_path(dir.path()).unwrap();
    client.request(1, "initialize",
        json!({ "processId": null, "rootUri": root.as_str(), "capabilities": {} }));
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));
    // (allow the initial index warm-up to run; the didOpen below also reindexes.)

    let uri = url::Url::from_file_path(&main_path).unwrap();
    client.notify("textDocument/didOpen", json!({
        "textDocument": { "uri": uri.as_str(), "languageId": "ascript",
                           "version": 1, "text": main_text }
    }));
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    client.request(2, "textDocument/signatureHelp", json!({
        "textDocument": { "uri": uri.as_str() },
        "position": { "line": 1, "character": 4 } // inside add(
    }));
    let resp = client.read_response(2, overall);
    let label = resp["result"]["signatures"][0]["label"].as_str().expect("label");
    assert_eq!(label, "add(first: number, second?: number)",
        "names + annotations, not just arity: {resp}");
}
```

  (Check whether `tempfile`/`url` are already dev-dependencies — `tests/` uses
  `ascript-test://` URIs today; if `tempfile` is absent, add it as a dev-dependency, or
  reuse `std::env::temp_dir()` + a unique subdir as other integration suites do — match the
  existing house idiom found in `tests/modules.rs`/`tests/pkg.rs`.)
- [ ] **Step 2: Run — expect FAIL** until the rendering matches; reconcile the exact
  optional-param rendering (`second?: number` — defaulted params render as optional, the
  default value itself is omitted for user fns in v1; record the choice in the provider doc).
- [ ] **Step 3: Run — expect PASS.**
- [ ] **Step 4: Commit** — `git commit -m "test(lsp/sig): cross-file imported-fn signature integration proof (SIG §3.1c)"`.
- [ ] **Step 5: Independent review** — reviewer probes: a STALE index (edit util.as on disk
  without a watcher → signature reflects last-indexed text, acceptable + documented),
  an ambiguous import (two modules exporting `add` imported under aliases), a parse-broken
  util.as (→ None, no panic).

### Task 2.3: completion enrichment (kind/detail/docs + auto-import cache/sort_text)

**Files:** modify `src/lsp/providers/completion.rs`, `src/lsp/server.rs`
(`completion_resolve` drops the throwaway model); extend `tests/lsp.rs`.

- [ ] **Step 1: Write the failing unit tests** — in `completion.rs` tests:

```rust
#[test]
fn member_items_carry_real_kind_detail_and_resolve_docs() {
    let it = items("import * as math from \"std/math\"\nlet y = math.");
    let pi = it.iter().find(|i| i.label == "pi").expect("pi offered");
    assert_eq!(pi.kind, Some(CompletionItemKind::CONSTANT), "pi is a constant");
    assert_eq!(pi.detail.as_deref(), Some("float"));
    let pow = it.iter().find(|i| i.label == "pow").expect("pow offered");
    assert_eq!(pow.kind, Some(CompletionItemKind::FUNCTION));
    assert_eq!(pow.detail.as_deref(), Some("(base: number, exp: number) -> float"));
    // resolve fills the doc from the `data` payload — no model needed.
    let mut pow = pow.clone();
    resolve_completion_static(&mut pow);
    let Some(Documentation::MarkupContent(mk)) = &pow.documentation else {
        panic!("resolve should fill stdlib docs")
    };
    assert!(mk.value.contains("Raise a base"));
}

#[test]
fn auto_import_items_are_deprioritized_and_cached() {
    let a = items("ab\n");
    let abs = a.iter()
        .find(|i| i.detail.as_deref() == Some("auto-import from std/math") && i.label == "abs")
        .expect("abs auto-import");
    assert!(abs.sort_text.as_deref().unwrap_or("").starts_with("zz"),
        "auto-import must sort after locals: {:?}", abs.sort_text);
    // The candidate source is the cached static list (identity across calls).
    assert!(std::ptr::eq(auto_import_entries(), auto_import_entries()));
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** — per spec §3.2: the C2 branch maps kind from the export
  `Value` (cross-checked against `module_members` — a mismatch is unreachable thanks to the
  Task 1.1 drift test), detail from `std_sig` rendering (share ONE `render_sig(&StdSig)`
  helper with signature.rs/hover.rs — write it once in `std_sigs.rs` or a small
  `providers/sig_render.rs`), `data: json!({"module": path, "name": name})`;
  `resolve_completion` reads `data` first (a static path — `completion_resolve` in
  `server.rs:676-690` stops building the empty `SemanticModel`); `auto_import_entries()` is
  a `OnceLock<Vec<AutoImportEntry>>` built from `module_members` (NO `std_module_exports`
  call — pure static data), each item with `sort_text: format!("zz{label}")` + correct kind.
- [ ] **Step 4: Run — expect PASS**; the existing completion unit battery stays green
  (the C2 member tests asserting FUNCTION-kind for `sqrt`/`abs` remain true; the old
  bare-label expectations updated where they asserted absence of detail).
- [ ] **Step 5: Integration test in `tests/lsp.rs`** (didOpen with the dot in the text, the
  `lsp_completion_member_on_typed_instance` idiom): `math.` → `pi` kind 21 (CONSTANT) with
  detail `float`, `pow` kind 3 (FUNCTION) with the sig detail; then a
  `completionItem/resolve` round-trip on `pow` returns documentation.
- [ ] **Step 6: Commit** — `git commit -m "feat(lsp/completion): real kind/detail/docs for stdlib members + cached, deprioritized auto-import (SIG §3.2)"`.
- [ ] **Step 7: Independent review** — reviewer probes: a feature-gated module under
  `--no-default-features` lib tests (completion unit tests live in the lib — `std/regex`
  members must still offer from the static table even when the export fn is compiled out —
  verify the C2 branch's fallback uses `module_members` when `std_module_exports` is None,
  or record + test the chosen behavior), resolve on an item with no `data` (builtin/keyword
  path intact), and the auto-import edit text unchanged (existing tests).

### Task 2.4: hover on stdlib members

**Files:** modify `src/lsp/providers/hover.rs`, `src/lsp/providers/completion.rs` (share the
member-at-offset scanner); extend `tests/lsp.rs`.

- [ ] **Step 1: Write the failing unit tests** — in `hover.rs` tests:

```rust
#[test]
fn hover_on_stdlib_member_shows_signature_and_doc() {
    let src = "import * as math from \"std/math\"\nlet y = math.sqrt(2)\n";
    let m = model(src);
    let off = src.rfind("sqrt").unwrap() + 1;
    let h = hover(&m, off).expect("hover on math.sqrt");
    let HoverContents::Markup(mk) = h.contents else { panic!() };
    assert!(mk.value.contains("math.sqrt("), "sig line: {}", mk.value);
    assert!(mk.value.contains("Returns") || mk.value.contains("square root")
        || mk.value.contains("->"), "doc/ret: {}", mk.value);
}

#[test]
fn hover_on_stdlib_constant_shows_type() {
    let src = "import * as math from \"std/math\"\nlet y = math.pi\n";
    let m = model(src);
    let off = src.rfind("pi").unwrap();
    let h = hover(&m, off).expect("hover on math.pi");
    let HoverContents::Markup(mk) = h.contents else { panic!() };
    assert!(mk.value.contains("math.pi: float"), "{}", mk.value);
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** — per spec §3.3: a `stdlib_member_at(model, offset) ->
  Option<(module, member)>` scanner (identifier containing the offset, preceding `.`,
  leading alias identifier, alias → `namespace_import_module`); render via the shared
  `render_sig` into a fenced `ascript` block + the doc line, pushed as the FIRST hover part
  (before the `hover_type_at` type and `doc_at` parts, `---`-joined as today).
- [ ] **Step 4: Run — expect PASS** + an integration hover test in `tests/lsp.rs` (the
  existing hover request idiom) asserting the `math.sqrt` hover contains the signature.
- [ ] **Step 5: Commit** — `git commit -m "feat(lsp/hover): stdlib-member signature + doc hover (SIG §3.3)"`.
- [ ] **Step 6: Independent review** — reviewer probes: hover on the ALIAS itself (`math` —
  must not render a member sig), a non-import alias (`foo.bar` — falls through), hover on a
  member of a commented-out import line (the text-scan import detection fires — confirm
  acceptable + consistent with completion's same documented behavior).

### Task 2.5: Phase 2 holistic review

- [ ] **Step 1:** Holistic subagent over the combined Unit B diff: ONE shared
  `render_sig`/`namespace_import_module`/member-scanner (no triplicated logic), the
  signature ladder order matches spec §3.1 exactly (same-file first), all three consumers
  read the SAME table, `tests/lsp.rs` + lib units green, clippy both configs.
- [ ] **Step 2:** Findings fixed in-phase before Phase 3.

---

## Phase 3 — Unit C: audit hardening

### Task 3.1 (C1 + C8): partial-identifier member completion + string/comment suppression

**Files:** modify `src/lsp/providers/completion.rs`; extend `tests/lsp.rs`.

- [ ] **Step 1: Write the failing unit tests:**

```rust
#[test]
fn member_completion_with_partial_identifier_after_dot() {
    // C1: manual invoke at `math.sq|` must offer members (filtered client-side).
    let it = items("import * as math from \"std/math\"\nlet y = math.sq");
    let sqrt = it.iter().find(|i| i.label == "sqrt").expect("sqrt offered at math.sq|");
    assert_eq!(sqrt.filter_text.as_deref(), Some("sq"), "typed prefix as filter_text");
}

#[test]
fn no_completion_inside_strings_and_comments() {
    // C8: a plain string body and a comment body yield NO items…
    assert!(items("let s = \"hel").is_empty());
    assert!(items("// com").is_empty());
    assert!(items("/* blo").is_empty());
    // …but the import-path context still completes,
    assert!(!items("import { x } from \"std/").is_empty());
    // and a template INTERPOLATION completes normally.
    assert!(labels(&items("let n = 1\nlet s = `v: ${n")).contains(&"print"));
}
```

- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** — C1: `member_access_alias` (`completion.rs:420-442`) backtracks
  over trailing `is_ident_char` chars from the cursor to find the dot, returning
  `(alias, typed_prefix)`; all four member contexts (C2/C3/C4 + the new prefix) set
  `filter_text = typed_prefix` on their items when non-empty. C8: at `completions` entry,
  locate the cursor token in `model.tokens` (binary search by range); if its kind ∈
  {`Str`, `TemplateStr`, `TemplateStart`, `TemplateMiddle`, `TemplateEnd`, `LineComment`,
  `BlockComment`} and `in_import_path_string` is false → return `Vec::new()`. (Template
  interpolation interiors are ordinary tokens — untouched by construction.)
- [ ] **Step 4: Run — expect PASS** + the full existing completion battery (garbage
  robustness etc.) green; integration test: manual-invoke completion at `math.sq` (no
  trigger char) offers `sqrt`.
- [ ] **Step 5: Commit** — `git commit -m "feat(lsp/completion): partial-identifier member context + string/comment suppression (SIG §4 C1/C8)"`.
- [ ] **Step 6: Independent review** — reviewer probes: `math.` (empty prefix — unchanged),
  `x.y.z` chains (alias = `y`? the scanner takes the ident immediately before the LAST dot
  before the prefix — confirm + test the chain behavior), cursor exactly at a string's
  closing quote, an unterminated template at EOF.

### Task 3.2 (C2 + C4): workspace_diagnostic yielding/model-reuse + folder-removal unindex

**Files:** modify `src/lsp/server.rs`; extend `tests/lsp.rs`.

- [ ] **Step 1: Write the failing tests** — integration, in `tests/lsp.rs`:
  (a) `lsp_workspace_diagnostic_yields`: open a temp workspace with ~30 generated files,
  fire `workspace/diagnostic`, and INTERLEAVE a `textDocument/hover` request — assert the
  hover response arrives BEFORE the workspace report (the yield makes interleaving
  possible; with no yield the hover is starved until the report completes). Use distinct
  request ids and assert arrival order via the harness's message stream.
  (b) `lsp_workspace_folder_removal_unindexes`: two temp roots A and B each with a uniquely
  named fn; initialize with A+B (or add B via `didChangeWorkspaceFolders`), assert
  `workspace/symbol` finds B's fn; send `didChangeWorkspaceFolders` removing B; assert B's
  fn is GONE and A's remains.
- [ ] **Step 2: Run — expect FAIL** ((a) hover starved / order inverted; (b) stale symbol).
- [ ] **Step 3: Implement** — C2 (`server.rs:1012-1050`): inside the per-file loop, first
  consult the open-document store (`canon_to_url` → `store.get` → reuse `m.lsp_diagnostics()`
  when `m.text == file.text`), else build; `tokio::task::yield_now().await` after EACH file
  (drop the store lock before yielding — never hold a Mutex guard across the await; clippy
  `await_holding_lock` discipline). C4 (`server.rs:1666-1692`): before re-warm, for each
  removed root collect `idx.files` keys under it (and not under a surviving root) and
  `fully_unindex` each.
- [ ] **Step 4: Run — expect PASS** both integration tests + the existing
  workspace-diagnostic pull test.
- [ ] **Step 5: Commit** — `git commit -m "fix(lsp/server): workspace_diagnostic yields + reuses open models; folder removal unindexes (SIG §4 C2/C4)"`.
- [ ] **Step 6: Independent review** — reviewer probes: a file under BOTH roots (must
  survive removal of one), cancellation/ordering with a didChange racing the workspace
  pull, and confirms no lock is held across the new awaits (read the diff + clippy).

### Task 3.3 (C3): per-model inference cache + hover size-class gate

**Files:** modify `src/lsp/model.rs`, `src/check/infer/mod.rs`,
`src/lsp/providers/hover.rs`, `src/lsp/providers/completion.rs`, `src/lsp/server.rs`
(hover gate); extend tests.

- [ ] **Step 1: Write the failing tests:**
  (a) in `infer/mod.rs` or `model.rs` tests — a build-count probe:

```rust
#[test]
fn infer_cache_builds_once_per_model() {
    let m = SemanticModel::build("let x: int = 1\nprint(x)\n".into(), None, &LintConfig::default());
    let c1 = m.infer_cache() as *const _;
    let c2 = m.infer_cache() as *const _;
    assert!(std::ptr::eq(c1, c2), "second access must hit the OnceLock");
}
```

  (b) hover behavior parity: every existing `hover.rs` unit test must pass UNCHANGED
  through the cached path (the cache returns the same hover spans `hover_type_at` computes —
  pin one explicit equality test comparing `hover_type_at(&m.text, off)` against the cached
  lookup for a battery of offsets).
  (c) size gate: a `LARGE_FILE_BYTES`-sized doc hovers WITHOUT the type part but WITH the
  builtin/keyword doc part (unit test on the provider with a synthetic large model).
- [ ] **Step 2: Run — expect FAIL** (`infer_cache` missing).
- [ ] **Step 3: Implement** —
  - `check/infer/mod.rs`: factor `hover_type_at`'s body (`mod.rs:38-49`) into
    `pub struct InferArtifacts { pub hovers: Vec<HoverSpan>, pub table: table::Table }` +
    `pub fn build_artifacts(tree, resolved, src) -> InferArtifacts` and
    `pub fn hover_type_in(artifacts: &InferArtifacts, byte_offset) -> Option<String>`
    (the narrowest-span selection moved verbatim). `hover_type_at` remains as a thin
    compose (other callers / tests untouched).
  - `model.rs`: `infer: std::sync::OnceLock<crate::check::infer::InferArtifacts>` on
    `SemanticModel` + `pub fn infer_cache(&self) -> &InferArtifacts` (lazy `get_or_init`
    over the model's OWN `tree`/`resolved` — no re-parse). Confirm `InferArtifacts` is
    `Send + Sync` (plain owned data — `Table` holds `String`/`CheckTy` only; if `cstree`
    types leak in, store rendered data instead — the artifacts must not capture tree nodes).
  - `hover.rs:12` → `hover_type_in(model.infer_cache(), offset)`; `completion.rs:264`
    (`Table::build`) → `&model.infer_cache().table`; `receiver_class_info`'s
    `hover_type_at(&model.text, byte_off)` (`completion.rs:388`) → the cached lookup.
  - `server.rs` hover handler: the inlay-style size-class match (`server.rs:807-835`
    pattern) — `Large`/`Huge` skip the type part (provider takes a `with_types: bool` or
    the handler calls a docs-only variant), logged once for `Huge` like inlay.
- [ ] **Step 4: Run — expect PASS**: the parity battery, all hover/completion/inlay units,
  `tests/lsp.rs`, clippy both configs (watch: `OnceLock` keeps `SemanticModel: Send+Sync` —
  there is a `lsp_does_not_import_legacy_frontend` style guard; run the full lib suite).
- [ ] **Step 5: Commit** — `git commit -m "perf(lsp): per-model OnceLock inference cache + hover size-class gate (SIG §4 C3)"`.
- [ ] **Step 6: Independent review** — reviewer measures: hover twice on a 256 KiB doc —
  second hover must be ~instant (eyeball with a timing probe or the perf.rs harness);
  confirms inlay/semantic-tokens still build their own data correctly; probes a model
  rebuilt by didChange (new model = new empty cache — no staleness possible by
  construction: the OnceLock dies with the model).

### Task 3.4 (C5 + C6 + C7): fs-canonicalize, typeHierarchy decision + poison log, snippet gating

**Files:** modify `src/lsp/workspace.rs`, `src/lsp/server.rs`,
`src/lsp/providers/completion.rs`; extend `tests/lsp.rs`.

- [ ] **Step 1: Write the failing tests:**
  (a) unit, `workspace.rs`:

```rust
#[test]
fn canonicalize_resolves_symlinks_with_lexical_fallback() {
    let dir = std::env::temp_dir().join(format!("ascript-canon-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("real")).unwrap();
    let link = dir.join("link");
    #[cfg(unix)]
    {
        let _ = std::os::unix::fs::symlink(dir.join("real"), &link);
        std::fs::write(dir.join("real/a.as"), "fn f() {}\n").unwrap();
        assert_eq!(canonicalize(&link.join("a.as")), canonicalize(&dir.join("real/a.as")),
            "symlinked and real paths must key identically");
    }
    // Non-existent path: lexical fallback still normalizes `..`.
    assert_eq!(canonicalize(Path::new("/x/y/../z.as")), PathBuf::from("/x/z.as"));
    let _ = std::fs::remove_dir_all(dir);
}
```

  (b) integration: initialize WITHOUT `snippetSupport` (`"capabilities": {}` — the existing
  default!) → completion items contain NO `${` anywhere; then a second test initializing
  WITH `{"textDocument":{"completion":{"completionItem":{"snippetSupport":true}}}}` →
  the `fn` snippet present (NOTE: the existing tests initialize with empty capabilities and
  some assert snippets — those assertions MOVE to the snippet-enabled test; this is a
  deliberate, spec-mandated behavior change for capability-less clients, recorded in the
  commit message).
  (c) unit: the poison log — poison `index` via `catch_unwind` while holding a write guard,
  call a read site, assert the one-time flag flipped (and a second call doesn't re-log).
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** — C5: `canonicalize` (`workspace.rs:977-990`) tries
  `std::fs::canonicalize(path)` first, falls back to the existing lexical loop (keep the
  doc-comment updated: determinism for non-existent paths, fs-truth for real ones). C6:
  the decision comment block at `server.rs:283-287` (typeHierarchy stays `experimental`;
  tower-lsp 0.20 pins lsp-types 0.94; framework replacement out of scope — link spec §7) +
  `index_poisoned_logged: AtomicBool` and a `fn index_read(&self)` /`index_write` pair that
  wraps every `self.index.read()/write()` site, logging via `eprintln!`/`tracing` ONCE on
  `Err`. C7: `initialize` captures `snippet_support` into an `AtomicBool` on `Backend`;
  threaded into `completions` (a `CompletionCtx { snippet_support, .. }` arg or a field on
  the model call — pick the smaller diff); `snippet_completions` emits plain bodies
  (`"fn name() {\n  \n}"` without tab-stops) and `variant_completion_item` emits
  `Variant(` plain inserts when false.
- [ ] **Step 4: Run — expect PASS**; full `tests/lsp.rs` (existing snippet assertions
  relocated per (b)); clippy both configs.
- [ ] **Step 5: Commit** — `git commit -m "fix(lsp): fs-canonical index keys, typeHierarchy decision + poison log, snippetSupport gating (SIG §4 C5/C6/C7)"`.
- [ ] **Step 6: Independent review** — reviewer probes: case-variant URI on macOS
  (open `FILE.AS` vs `file.as` — keying consistent), the Windows-irrelevant symlink test
  cfg-gating, every `index.read()` site routed through the wrapper (grep), and a
  plain-text-client end-to-end completion containing zero `${`.

### Task 3.5: Phase 3 holistic review

- [ ] **Step 1:** Holistic subagent over the combined Unit C diff: each of C1–C8 has a test
  that FAILED before its fix (verify via the commit history's red→green narrative), no lock
  held across a new await, no behavior regressed in the pre-existing battery.
- [ ] **Step 2:** Findings fixed in-phase before Phase 4.

---

## Phase 4 — Gates, docs, Definition of Done

### Task 4.1: docs + status updates

**Files:** modify `docs/content/tooling/lsp-capabilities.md`, `CLAUDE.md`,
`superpowers/roadmap.md`, `goal-perf.md`.

- [ ] **Step 1:** `docs/content/tooling/lsp-capabilities.md`: update the signature-help row
  (stdlib members / methods / cross-file / builtins; active-param + docs), the completion
  row (kind/detail/docs, auto-import ranking, partial-identifier, string/comment
  suppression, snippet gating), the hover row (stdlib member signatures), and a
  typeHierarchy client-visibility note (the C6 decision). Existing page, existing NAV slug —
  NO `NAV` change (the orphan gotcha doesn't apply). Serve-check the page
  (`cd docs && python3 -m http.server` + load) per the docs rule.
- [ ] **Step 2:** `CLAUDE.md`: a condensed SIG note (the `std_sigs` table as the machine
  source of truth + the two drift-test families + "adding a stdlib fn = export + docs entry
  + table row or CI fails"; std_arity now derives from it; the LSP signature ladder; the
  per-model `OnceLock` inference cache).
- [ ] **Step 3:** `goal-perf.md`: flip SIG's status to ✅ with a one-line result summary;
  `superpowers/roadmap.md`: the milestone record entry.
- [ ] **Step 4: Commit** — `git commit -m "docs(sig): lsp-capabilities page + CLAUDE.md/roadmap/goal-perf status (SIG Gate 13)"`.

### Task 4.2: full matrix + Definition of Done (goal.md Gates 1–14 + goal-perf 15–18 where applicable)

**Files:** none (verification; fixes spawn tracked in-branch tasks).

- [ ] **Gate 1 (four-mode byte-identity) + the no-engine-surface proof:**
  `git diff main --stat -- src/vm src/interp.rs src/compile src/syntax src/value.rs
  src/gc.rs` is EMPTY (zero engine files touched); `cargo test --test vm_differential` green
  in BOTH feature configs (run once — the AFTER half of the Phase-0 proof);
  `ASO_FORMAT_VERSION` unchanged (`grep ASO_FORMAT_VERSION src/vm/aso.rs` matches Phase 0).
- [ ] **Gate 2 (clippy):** `cargo clippy --all-targets` AND
  `cargo clippy --no-default-features --all-targets` clean.
- [ ] **Gate 3 (tests):** `cargo test` AND `cargo test --no-default-features` green —
  including `cargo test --test lsp`, `cargo test --test std_sigs_docs`, the lib unit suites.
- [ ] **Gate 4 (no borrow across await):** the new awaits (workspace_diagnostic yield)
  audited lock-free; clippy `await_holding_refcell_ref` + eyeball of `Mutex`/`RwLock`
  guards in the diff.
- [ ] **Gate 5 (zero `type-*`/lint corpus FPs):** `ascript check` over `examples/**` emits
  zero new diagnostics in both configs — re-run explicitly (the std_arity widening from
  Task 1.4 is the live risk; any new diagnostic = a table-row bug, fix the row).
- [ ] **Gate 6 (no placeholders/silent deferrals):** the v1 narrowings (handle-method
  signature help, complex-receiver methods, schema/ctx hooks) are spec §7 documented
  decisions; grep the diff for TODO/unimplemented.
- [ ] **Gate 7 (coverage migrated, never deleted):** the old std_arity drift test's coverage
  lives on in the Task 1.1 completeness pair (verified in Task 1.4's commit).
- [ ] **Gate 8 (continuous infra):** the two drift-test families run in plain `cargo test`
  (CI-default) — no special invocation needed; confirmed green.
- [ ] **Gates 9/10 (examples + unit tests, happy AND edge):** no new language surface ⇒ no
  new `.as` examples required (Gate 9 satisfied by the unchanged corpus, stated explicitly);
  unit+integration edges per the spec §5 matrix all present (negatives: unknown member,
  ambiguous receiver, variadic clamp, poisoned lock, symlink, folder removal, plain-text
  client, string/comment, empty prefix).
- [ ] **Gate 11 (tooling parity, confirmed working):** this spec IS the tooling work —
  proof is `cargo test --test lsp` green end-to-end; grammar/fmt/REPL untouched (no surface
  change), `tests/treesitter_conformance.rs` + `tests/frontend_conformance.rs` re-run green
  (untouched).
- [ ] **Gate 12/17 (zero perf regression):** no engine code touched (Gate-1 diff proof);
  the Gate-12 VM bench is structurally unaffected — re-run `tests/vm_bench.rs` once to
  confirm the floor (≥2× spec/tw) still holds on this branch. LSP-side: the C3 cache and
  §3.2 auto-import cache are measured improvements (record the before/after hover timing
  from Task 3.3 review in the PR description).
- [ ] **Gate 13 (docs):** Task 4.1 landed; the docs-consistency drift test doubles as the
  permanent docs-staleness tripwire for stdlib signatures.
- [ ] **Gate 14 (production-grade, zero lingering bugs):** every bug found en route (incl.
  any docs-page errors surfaced by Task 1.3) fixed in-branch failing-test-first; reviewers'
  findings all closed; no swallowed error in the new code paths (poison log, None-ladders).
- [ ] **Gates 15/16/18:** N/A — no new engine configuration, no engine A/B, no engine
  memory surface (recorded here so the inapplicability is explicit, not skipped silently).
  The Gate-16 spirit is honored LSP-side by the Task 3.3 same-session hover timing note.
- [ ] **Final holistic review (whole-effort):** a fresh subagent over the ENTIRE branch
  diff — spec §1–§8 coverage table (every Unit A/B/C item → its task + test), every plan
  checkbox ticked, the brief-vs-code corrections honored (domain-grouped docs pages;
  `method_sigs` not `ClassInfo.methods`; tower-lsp pin), cross-feature composition probe
  (one editor session driving: didOpen → `math.` completion with details → `.sq` partial →
  signature help on `math.pow(` advancing on comma → hover on `math.sqrt` → cross-file
  `add(` — all in `tests/lsp.rs` as one end-to-end scenario test if not already present).
- [ ] **Merge:** `--no-ff` to `main` once everything above is green.

---

## Self-review (author pass)

- **Spec coverage:** §2.1 (placement/shape) → Task 1.1; §2.2/§2.3 (survey, authoring
  decision, drift tests) → Tasks 1.1–1.3; §2.4 (full coverage) → Task 1.2; §2.5 (std_arity
  subsumption) → Task 1.4; §3.1 (signature ladder a–e) → Tasks 2.1–2.2; §3.2 (completion)
  → Task 2.3; §3.3 (hover) → Task 2.4; §4 C1–C8 → Tasks 3.1–3.4; §5 matrix → distributed +
  the final end-to-end scenario; §6/§7 → Task 4.2 gates.
- **No placeholders:** every code step is grounded in read sources (`signature.rs:31-57`
  NameRef gate, `completion.rs:420-442` alias scan, `workspace.rs:419-445` ParamList walk,
  `std_arity.rs:40-99` legacy entries — transcribed verbatim into Task 1.4's pin test,
  `server.rs:1012-1050`/`1666-1692` handlers, `infer/mod.rs:38-49` hover_type_at, the real
  docs entries for math.pow/string.slice/math.min/array.map).
- **Order of operations:** the table (Phase 1) lands before any consumer; the std_arity
  pin test is written against the LEGACY table before the swap; C7's behavior change for
  capability-less clients is explicit and test-relocated, never silent.
- **Known reconciliation points:** exact current line numbers re-grepped in Task 0.1;
  the optional-param rendering for defaulted user-fn params (`second?: number`) decided in
  Task 2.2 and recorded; the C2 feature-gated-module completion fallback decided + tested
  in Task 2.3 review; tempdir idiom matched to the existing test-suite house style in
  Task 2.2.
