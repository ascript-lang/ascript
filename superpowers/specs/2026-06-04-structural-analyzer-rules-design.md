# Structural Analyzer Rules (the "flow-typed tier", feasible subset) — Design

- **Date:** 2026-06-04
- **Status:** Design approved; ready for implementation plan
- **Scope:** Five new `ascript check` lint rules that statically catch *guaranteed* runtime errors detectable without value-type inference. Extends the analyzer batch shipped on `feat/ranges-step-analyzer`.
- **Branch:** `feat/ranges-step-analyzer` (continues the same branch).

---

## 1. Motivation

The original analyzer menu included a "flow-typed tier" (arity, unknown field/method, enum variants, class-shape checks) that the ranges/step spec §10 deferred. This addendum implements the **feasible, false-positive-free subset**: checks that are either purely **structural** or resolve a **directly-known callee/type**, so they need no value-type tracking and each mirrors a *guaranteed* runtime panic.

## 2. Key constraint — why "unknown member" checks are excluded

AScript instances are **open**: `p.extra = 5` assigns an undeclared field, and reading an unknown field (`p.nope`) returns **`nil`, not an error** (verified empirically). Therefore a static "unknown field/method on a typed instance" check (menu D4) and "unknown `self.field`" (E3) would **flag valid programs** — a false-positive class incompatible with the project's conservative, zero-false-positive lint bar. **D4 and E3 are excluded by design**, not deferred for effort. E2 (required field never assigned in `init`) is also excluded for V1 — murky under dynamic fields + defaults.

Every rule below instead targets a construct that the runtime **always rejects**, so flagging it statically can never be wrong on a valid program.

## 3. The five rules

All follow the existing checker pattern: a `fn check(tree, resolved, src) -> Vec<AsDiagnostic>` in `src/check/rules/`, registered in `ALL` (`src/check/rules/mod.rs`) and `RULE_CODES` (`src/check/config.rs`), default `Severity::Warning`, configurable via `--allow`/`--deny`/`ascript.toml [lint]`, flowing to both `ascript check` and the LSP. Each is **conservative**: if anything needed is non-literal/unresolved/ambiguous, the node is skipped.

### 3.1 `call-arity` (menu D2)
Flag a call with the wrong number of arguments to a **directly-named, uniquely-resolved** function.
- **Mirrors:** `<name> expected <N> argument(s), got <M>` (runtime panic).
- **Detection:** a `CallExpr` whose callee is a `NameRef` that the resolver binds to exactly ONE function declaration (top-level or in-scope `fn`) with a **fixed** parameter list. Flag when `args.len() != params.len()`.
- **Conservatism (skip the node if any hold):** callee is not a plain name; the name resolves to a global/import/parameter/multiple-decls/shadowed binding; the function has a **rest parameter** or **default parameter values** (skip — arity is a range, not exact); the call uses **spread** args (`f(...xs)`); the callee is a method call (`x.m(...)`) or any non-direct callee. (Constructors/methods are out of scope for V1.)
- **Message:** `<name> expects <N> argument(s) but is called with <M>`.

### 3.2 `unknown-enum-variant` (menu D7)
Flag access of a non-existent variant on a statically-known enum.
- **Mirrors:** `enum <E> has no variant '<V>'` (runtime panic).
- **Detection:** a member access `<Name>.<variant>` where `<Name>` resolves to an **enum declaration visible in the file**, and `<variant>` is not among that enum's declared variants.
- **Conservatism:** only when the receiver is directly an identifier bound to a known `enum` declaration. Skip if the name is shadowed/reassigned or not an enum.
- **Message:** `enum <E> has no variant '<V>'`.

### 3.3 `duplicate-member` (menu E1)
Flag two members with the same name in one class body.
- **Detection:** within a single `class` declaration, two field declarations, two methods, or a field and a method sharing a name → flag the second (and subsequent) occurrence(s).
- **Purely structural** (no runtime dependency); a duplicate is always a mistake (one silently shadows the other).
- **Message:** `duplicate member '<name>' in class <C>`.

### 3.4 `super-misuse` (menu E4)
Flag `super` used in a class with no superclass.
- **Mirrors:** `no superclass method '<m>' (no superclass)` (runtime panic).
- **Detection:** a `super` expression (`super.<m>(...)`) whose nearest enclosing `class` declaration has **no `extends` clause**.
- **Conservatism:** only when the enclosing class is unambiguous and lacks `extends`.
- **Message:** `\`super\` used in class <C>, which has no superclass`.

### 3.5 `field-default-type` (menu E6)
Flag a class field whose literal default contradicts its declared type.
- **Mirrors:** `type contract violated: expected <T>, got <kind>` (runtime panic at construction).
- **Detection:** a field declaration `<name>: <T> = <literal>` where `<literal>` is a numeric/string/bool/nil literal provably incompatible with `<T>`. Reuse the literal-vs-type compatibility logic from `src/check/rules/contract.rs` (the existing `contract-mismatch` rule already solves "literal vs annotation"; share or mirror it). `nil` is incompatible with a non-`T?` type.
- **Conservatism:** only literal defaults vs primitive/optional types; skip computed defaults, generic/complex types where compatibility is uncertain.
- **Message:** `field '<name>' default is <kind>, which violates its declared type <T>`.

## 4. Architecture

No new infrastructure — five new files under `src/check/rules/` (`call_arity.rs`, `unknown_enum_variant.rs`, `duplicate_member.rs`, `super_misuse.rs`, `field_default_type.rs`), five `ALL` entries, five `RULE_CODES` entries. The rules read the CST (`ResolvedNode`) plus the resolver's `ResolveResult` (for name→declaration binding, needed by `call-arity` and `unknown-enum-variant`). Where `contract.rs`'s literal-vs-type helper is reused, factor it into a shared helper in `check/rules/mod.rs` rather than copy it (the codebase already has two copies of `is_expr_kind`; do not add a third — consolidate while here).

## 5. Testing

- **Inline unit tests** per rule (mirror `contract.rs`/`range_step.rs`): positive cases (each flagged), negative/conservative cases (valid programs NOT flagged — esp. rest-param calls, shadowed names, dynamic fields, computed defaults), and message assertions.
- **`tests/cli.rs`**: `ascript check` fires each code; `--allow <code>` suppresses; `--deny` errors.
- **Corpus-clean (critical):** `ascript check examples/*.as examples/advanced/*.as` must show **zero** new spurious fires. Any fire on the existing corpus is either a real latent bug (report it) or over-aggression (fix the rule).
- **Both clippy configs** clean; full suite + `--no-default-features` green.

## 6. Non-goals / explicitly excluded

- **D4 / E3** (unknown field/method on instance or `self`) — false-positive-prone under open instances + nil-returns. Excluded permanently unless the language gains sealed classes.
- **E2** (required field unassigned in `init`) — deferred (murky under dynamic fields/defaults).
- Method/constructor arity (`call-arity` is direct-named-function only in V1).
- Cross-module analysis (e.g. arity of an imported function) — intra-file only, consistent with the rest of the checker.
