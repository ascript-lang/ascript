# Structural Analyzer Rules — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add five conservative, zero-false-positive `ascript check` lint rules that statically catch guaranteed runtime errors (`call-arity`, `unknown-enum-variant`, `duplicate-member`, `super-misuse`, `field-default-type`).

**Architecture:** Each rule is a `fn check(tree: &ResolvedNode, resolved: &ResolveResult, src: &str) -> Vec<AsDiagnostic>` in `src/check/rules/`, registered in `ALL` (`src/check/rules/mod.rs`) and `RULE_CODES` (`src/check/config.rs`), default `Severity::Warning`. They read the CST + resolver bindings, exactly like the existing `contract.rs`, `range_step.rs`, `unresolved_import.rs` rules. They flow to both `ascript check` and the LSP automatically.

**Tech Stack:** Rust, the `cstree` CST (`src/syntax/`), the checker framework (`src/check/`), `cargo test`.

**Spec:** `docs/superpowers/specs/2026-06-04-structural-analyzer-rules-design.md`

---

## Conventions (every task)

- **Templates to mirror:** `src/check/rules/contract.rs` (literal-vs-type + reads fn decls), `src/check/rules/range_step.rs` (walks CST nodes, reads literals), `src/check/rules/unresolved_import.rs` (small focused rule). Helpers: `code_range(&node)` (`src/check/rules/mod.rs`), `Severity::Warning`, `AsDiagnostic { range, severity, code, message, fix }`.
- **Registration:** add `pub mod <name>;` + `<name>::check,` to `ALL` (`src/check/rules/mod.rs`), and the code string to `RULE_CODES` (`src/check/config.rs`).
- **Conservatism is mandatory:** skip the node on any ambiguity (non-literal, unresolved, shadowed, multiple decls). ZERO spurious fires on `examples/*.as` + `examples/advanced/*.as` is a hard gate (verified in Task 7).
- **Gates per task:** `cargo build`; the rule's tests pass (RED first); `cargo test` full + `--no-default-features` green; `cargo clippy --all-targets` + `--no-default-features --all-targets` clean.
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Test style:** mirror how `contract.rs`/`range_step.rs` inline tests call the checker (`analyze(src)` / `diagnostics(src)`) and assert the returned codes. Find the exact entry (`src/check/analyze.rs` / `src/check/mod.rs`) and reuse it.

---

## Task 1: Consolidate the `is_expr_kind` helper (prep cleanup)

**Files:**
- Modify: `src/check/rules/mod.rs` (add shared helper), `src/check/rules/range_step.rs` (use it)

The spec asks to consolidate the duplicated `is_expr_kind` rather than add a third copy. `range_step.rs` has a local copy mirroring `compile/mod.rs`.

- [ ] **Step 1:** In `src/check/rules/mod.rs`, add a `pub(crate) fn is_expr_kind(k: SyntaxKind) -> bool` (copy the body from `range_step.rs`'s local copy). Re-export `SyntaxKind` use as needed.
- [ ] **Step 2:** In `src/check/rules/range_step.rs`, delete the local `is_expr_kind` and call `super::is_expr_kind` (or `crate::check::rules::is_expr_kind`).
- [ ] **Step 3:** Run `cargo test --lib check::` and `cargo build` → green; both clippy configs clean.
- [ ] **Step 4: Commit** `git commit -am "refactor(check): hoist is_expr_kind into rules::mod (DRY)"`

---

## Task 2: `call-arity` rule

**Files:**
- Create: `src/check/rules/call_arity.rs`
- Modify: `src/check/rules/mod.rs` (`ALL`), `src/check/config.rs` (`RULE_CODES`)
- Test: inline in `call_arity.rs` + `tests/cli.rs`

- [ ] **Step 1: Write failing tests.** Inline tests (mirror `range_step.rs` test style) asserting `analyze(src)` codes:

```
// flagged:
"fn f(a, b) { return a }\nf(1, 2, 3)"     -> one "call-arity"
"fn f(a, b) { return a }\nf(1)"           -> one "call-arity"
// NOT flagged (conservative):
"fn f(a, b) { return a }\nf(1, 2)"        -> none
"fn f(a, ...rest) { return a }\nf(1,2,3)" -> none   // rest param
"let g = fn(a){a}\ng(1,2)"                -> none   // not a uniquely-resolved top-level fn (be conservative)
"f(1,2,3)"                                 -> none   // unresolved callee
"obj.m(1,2,3)"                             -> none   // method call
"fn f(a,b){a}\nf(...xs)"                   -> none   // spread args
```

(If AScript has no default-param syntax, note it; if it does, add a `fn f(a, b = 1)` → not-flagged case.)

- [ ] **Step 2: Run** the tests → FAIL (rule absent). `cargo test --lib call_arity`.
- [ ] **Step 3: Implement** `src/check/rules/call_arity.rs`. Walk `CallExpr` nodes. For each: get the callee; if it's a plain `NameRef`, use `resolved` (`ResolveResult.uses`/bindings) to find its binding; proceed ONLY if it resolves to exactly one local/in-scope **function declaration** node (a `FnDecl` that is NOT a method — i.e. not inside a `class`, or handle by checking the binding kind). Read the fn's parameter list; if it has a rest param (`...name`) or any default value, SKIP. Count the call's positional args; if any arg is a spread, SKIP. If `args != params`, push `AsDiagnostic { range: code_range(&call), severity: Warning, code: "call-arity", message: format!("{name} expects {params} argument(s) but is called with {args}"), fix: None }`. Register in `ALL` + `RULE_CODES` (`"call-arity"`).
- [ ] **Step 4: Run** → PASS. Full suite + both clippy green.
- [ ] **Step 5:** Add a `tests/cli.rs` test: `ascript check` on a file with `fn f(a,b){a}\nf(1,2,3)` shows `call-arity`; `--allow call-arity` suppresses it.
- [ ] **Step 6: Commit** `git commit -am "feat(check): call-arity rule (wrong arg count to a known function)"`

---

## Task 3: `unknown-enum-variant` rule

**Files:**
- Create: `src/check/rules/unknown_enum_variant.rs`
- Modify: `src/check/rules/mod.rs`, `src/check/config.rs`
- Test: inline + `tests/cli.rs`

- [ ] **Step 1: Failing tests.**

```
"enum Color { Red, Green }\nprint(Color.Reddd)"  -> one "unknown-enum-variant"
"enum Color { Red, Green }\nprint(Color.Red)"     -> none
"enum Color { Red, Green }\nprint(other.Reddd)"   -> none   // receiver not a known enum
"let Color = 5\nprint(Color.x)"                   -> none   // name shadowed, not an enum
```

- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.** Collect every `enum` declaration in the file → map enum name → set of variant names (read the enum decl's variant list from the CST; study how the compiler reads enum decls in `src/compile/mod.rs`). Walk member-access nodes `<recv>.<member>`; if `<recv>` is a plain `NameRef` whose binding is exactly one of those enum declarations (use `resolved` to confirm it's not shadowed), and `<member>` is not in that enum's variant set, flag it: message `enum {E} has no variant '{V}'`. Register (`"unknown-enum-variant"`). Conservative: skip if the name resolves to anything other than a unique enum decl.
- [ ] **Step 4: Run** → PASS. Gates green.
- [ ] **Step 5:** `tests/cli.rs` check test + `--allow` suppression.
- [ ] **Step 6: Commit** `git commit -am "feat(check): unknown-enum-variant rule"`

---

## Task 4: `duplicate-member` rule

**Files:**
- Create: `src/check/rules/duplicate_member.rs`
- Modify: `src/check/rules/mod.rs`, `src/check/config.rs`
- Test: inline + `tests/cli.rs`

- [ ] **Step 1: Failing tests.**

```
"class C {\n  x: number\n  x: string\n}"               -> one "duplicate-member"  (field/field)
"class C {\n  fn m() {}\n  fn m() {}\n}"                -> one "duplicate-member"  (method/method)
"class C {\n  x: number\n  fn x() {}\n}"                -> one "duplicate-member"  (field/method)
"class C {\n  x: number\n  y: string\n  fn m(){}\n}"    -> none
```

- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.** Walk `class` declarations. For each, collect member names (field declarations + method declarations) in order; track seen names in a `HashSet`; on a repeat, flag the LATER occurrence: message `duplicate member '{name}' in class {C}`. Purely structural — no resolver needed. Register (`"duplicate-member"`).
- [ ] **Step 4: Run** → PASS. Gates green.
- [ ] **Step 5:** `tests/cli.rs` check test + `--allow`.
- [ ] **Step 6: Commit** `git commit -am "feat(check): duplicate-member rule"`

---

## Task 5: `super-misuse` rule

**Files:**
- Create: `src/check/rules/super_misuse.rs`
- Modify: `src/check/rules/mod.rs`, `src/check/config.rs`
- Test: inline + `tests/cli.rs`

- [ ] **Step 1: Failing tests.**

```
"class A {\n  fn init() { super.init() }\n}"                       -> one "super-misuse"
"class A {}\nclass B extends A {\n  fn init() { super.init() }\n}" -> none   // B has a superclass
```

- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.** Walk `super` expression nodes (grep the CST kind for `super` — likely `SuperExpr` or a `super` keyword token in a member/call expr; study how the compiler handles `super` in `src/compile/mod.rs`). For each, find the nearest enclosing `class` declaration (via `.ancestors()`); if that class has NO `extends` clause, flag it: message `` `super` used in class {C}, which has no superclass ``. Skip if no enclosing class can be determined. Register (`"super-misuse"`).
- [ ] **Step 4: Run** → PASS. Gates green.
- [ ] **Step 5:** `tests/cli.rs` check test + `--allow`.
- [ ] **Step 6: Commit** `git commit -am "feat(check): super-misuse rule"`

---

## Task 6: `field-default-type` rule

**Files:**
- Create: `src/check/rules/field_default_type.rs`
- Modify: `src/check/rules/mod.rs`, `src/check/config.rs`
- Test: inline + `tests/cli.rs`

- [ ] **Step 1: Failing tests.**

```
"class P { n: number = \"x\" }"        -> one "field-default-type"
"class P { s: string = 5 }"            -> one "field-default-type"
"class P { n: number = 5 }"            -> none
"class P { s: string = \"ok\" }"       -> none
"class P { n: number? = nil }"         -> none   // nil ok for optional
"class P { n: number = nil }"          -> one "field-default-type"   // nil not ok for non-optional
"class P { xs: array<number> = foo() }"-> none   // computed default, skip
```

- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.** Reuse `contract.rs`'s literal-vs-type compatibility (factor its `LitKind`/`Compat` logic into a shared `pub(crate)` helper in `check/rules/mod.rs` if needed, OR call into it — do NOT copy it). Walk class field declarations that have BOTH a type annotation and a literal default; if the default literal's kind is provably incompatible with the declared type (incl. `nil` vs non-`T?`), flag it: message `field '{name}' default is {kind}, which violates its declared type {T}`. Skip computed/non-literal defaults and complex/generic types where compatibility is uncertain. Register (`"field-default-type"`).
- [ ] **Step 4: Run** → PASS. Gates green.
- [ ] **Step 5:** `tests/cli.rs` check test + `--allow`.
- [ ] **Step 6: Commit** `git commit -am "feat(check): field-default-type rule"`

---

## Task 7: Closeout — corpus-clean, docs, final gates

**Files:**
- Modify: `docs/content/cli.md` (checker rule list)
- Verify: full suite + corpus

- [ ] **Step 1: Corpus-clean (hard gate).** Run `target/release/ascript check examples/*.as examples/advanced/*.as` (build release first). Grep the output for the five new codes — there must be ZERO occurrences. If any fires: determine if it's a real latent bug in the example (fix the example + report) or rule over-aggression (fix the rule). Do not proceed until zero spurious fires.
- [ ] **Step 2: Docs.** In `docs/content/cli.md` (the `ascript check` section that already lists `range-step`/`invalid-propagate`/`unresolved-import`), add the five new rules with one-line descriptions + default Warning + configurability. Match the existing style.
- [ ] **Step 3: Final gates.** `cargo test` full + `cargo test --no-default-features` (0 failed); `cargo clippy --all-targets` + `--no-default-features --all-targets` clean; `cargo test --test vm_differential` + `--test aso` green (unaffected, confirm).
- [ ] **Step 4: Commit** `git commit -am "docs(check): document the five structural analyzer rules"`

---

## Self-review

- **Spec coverage:** §3.1→Task 2, §3.2→Task 3, §3.3→Task 4, §3.4→Task 5, §3.5→Task 6, §4 helper-consolidation→Task 1, §5 corpus-clean+docs→Task 7. All five rules + the cleanup + testing covered.
- **Placeholder scan:** test inputs are concrete `.as` strings with exact expected codes; implementation steps name the CST nodes/resolver fields and the template to mirror. The one genuine unknown (default-param syntax existence, super-expr CST kind, enum-decl reading) is flagged as "study the compiler/grep the kind" — a real investigation step, not a hand-wave.
- **Consistency:** rule codes (`call-arity`, `unknown-enum-variant`, `duplicate-member`, `super-misuse`, `field-default-type`) used identically across each rule's registration, tests, and Task 7 docs/corpus check.
