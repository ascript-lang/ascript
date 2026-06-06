# SP10 — Static Gradual Type Checker — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`)
> syntax for tracking.

**Goal:** Add an advisory, local-bidirectional, intra-procedural gradual type checker to AScript that
emits default-Warning `type-mismatch`/`type-error`/`possibly-nil` diagnostics through the existing
`src/check/` machinery, predicting likely runtime contract violations ahead of time. It **subsumes**
`contract-mismatch` + `field-default-type`, is **orthogonal** to `call-arity` + the control/effect
lints, and changes **no** language/runtime/VM/`value.rs`/grammar — zero byte-identity risk.

**Architecture:** Five tasks (T1–T5), each TDD, each ending green on **both** feature configs +
clippy (both configs) + the **whole-corpus zero-new-`type-*`-diagnostic differential**, with an
independent review before the next. The pass lives in `src/check/infer/` and integrates as a single
`infer::check(tree, resolved, src) -> Vec<AsDiagnostic>` (same signature as a `Rule`) wired into
`src/check/analyze.rs` after the `rules::ALL` loop.

**Tech Stack:** Rust. CST front-end (`src/syntax/{lexer,parser,tree_builder,kind,cst}.rs`) →
resolver (`src/syntax/resolve`, yielding `ResolveResult { uses, bindings, frames }`) → SP10 pass.
Diagnostics: `src/check/diagnostic.rs` (`AsDiagnostic`, `Severity`, `ByteSpan`). Surface types:
`ast::Type` (`src/ast.rs:127`). No interpreter, no new dependency.

**Spec:** `docs/superpowers/specs/2026-06-04-sp10-type-checker-design.md`.

**Branch:** `feat/sp1-engine-parity` (SP10 lands on the post-cutover gap-program branch alongside its
spec; create a child topic branch `feat/sp10-type-checker` off it if isolating per the team's
worktree workflow).

---

## Conventions for every task

- **Corpus zero-FP differential (the safety net, run every task):**
  `cargo test --test check corpus 2>&1 | tail` MUST be green — extended in T1 with a test that counts
  `type-mismatch`/`type-error`/`possibly-nil` across `examples/*.as` + `examples/advanced/*.as` and
  asserts **0**, in BOTH feature configs. A new corpus `type-*` diagnostic = a bug in
  `assignable`/`synth`; **relax the guard, never the differential** (same rule as the VM three-way
  differential).
- **Gate after each task (paste tails):** `cargo test 2>&1` (0 failures, all binaries);
  `cargo test --no-default-features 2>&1` (0 failures); `cargo clippy --all-targets` AND
  `cargo clippy --no-default-features --all-targets` (clean); `cargo test --test check 2>&1 | tail`;
  and — to prove SP10 didn't perturb the engines — `cargo test --test vm_differential 2>&1 | tail`
  (must be UNCHANGED green; SP10 runs no code).
- **No front-end / VM / `value.rs` / grammar / formatter change.** SP10 touches only `src/check/`
  (+ a thin `src/lsp/analysis.rs` hover hook in T5). If a task seems to need a front-end change, stop —
  it's out of scope (see the spec non-goals).
- **Three-valued discipline:** `assignable` returns `Compat3 { Yes, No, Unknown }`; **only `No`
  emits.** Every new code path that could diagnose must default to `Unknown`/silent.
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Never** edit a passing test or weaken the corpus assertion to make the checker pass.

---

## Task T1 — Lattice + assignability + corpus differential machinery (land before load-bearing)

**The V13-T1 move:** build the pure, fully-tested type machinery AND wire the (currently no-op) pass
into the driver, emitting NOTHING yet, so the corpus differential is green-by-construction and locks
in safety before any diagnostic exists.

**Files:** NEW `src/check/infer/{mod.rs,ty.rs,table.rs}`; modify `src/check/mod.rs` (`pub mod infer`),
`src/check/analyze.rs` (call `infer::check`), `src/check/config.rs` (`RULE_CODES`). Test `tests/check.rs`.

### T1.1 — `CheckTy` lattice + three-valued assignability (pure, unit-tested)

- [ ] **Step 1 — Read** `src/ast.rs:127` (`Type`), `src/check/rules/mod.rs:151-253` (the existing
  `Compat`/`type_compat`/`is_type_kind`/`literal_kind` SP10 generalizes), `src/syntax/resolve/types.rs`
  (`Resolution`, `Binding`, `ResolveResult`). Note: the resolver exposes NO typed class table — T1.2
  builds one.
- [ ] **Step 2 — Write failing unit tests** in `src/check/infer/ty.rs` (`#[cfg(test)]`): `assignable`
  returns `Compat3::Yes/No/Unknown` for representative pairs —
  - gradual escape: `assignable(Any, Number) == Yes`, `assignable(Number, Any) == Yes`;
  - primitives: `assignable(Number, Number) == Yes`, `assignable(Number, String) == No`;
  - nil/optional: `assignable(Nil, Union[Number,Nil]) == Yes`, `assignable(Nil, Number) == No`;
  - constructors: `assignable(Array(Number), Array(String)) == No`, `Array(Number)→Array(Any) == Yes`;
  - unions: `assignable(Number, Union[Number,String]) == Yes`, `assignable(Bool, Union[Number,String]) == No`;
  - default/uncertain: `assignable(Object, Class(c)) == Unknown`;
  - `join(Number, Nil) == Union[Number,Nil]`, `join(Number, Any) == Any`;
  - normalize: nested/dup/unsorted unions canonicalize; a >8-member union collapses to `Any`; depth >8
    collapses to `Any`.
- [ ] **Step 3 — Implement** `src/check/infer/ty.rs`: the `CheckTy` enum (spec §1), `Compat3`,
  `CheckTy::from_type_node(&ResolvedNode, &Table) -> CheckTy` (lower a type-annotation CST node;
  unknown name → `Any`), `normalize` (flatten/dedup/nil-canonicalize/sort + width cap 8 + depth cap 8),
  `assignable(&self, dst) -> Compat3` (spec §1.2 rules 1–11, in order; class-chain walk takes the
  `&Table` + a visited-set), `join`/`meet` (spec §1.3). `ClassId`/`EnumId` are `usize` table indices.
- [ ] **Step 4 — Run** `cargo test --lib infer::ty 2>&1 | tail` → green.
- [ ] **Step 5 — Commit:** `feat(check): CheckTy lattice + three-valued assignable/join (no diagnostics yet)`.

### T1.2 — Class/enum symbol table (pure, unit-tested)

- [ ] **Step 6 — Read** `src/check/rules/contract.rs:14-27` (the `by_name` FnDecl-map pattern to
  generalize), `src/syntax/resolve/mod.rs:740` (`resolve_class`), the CST kinds (`ClassDecl`,
  `FieldDecl`, `MethodDecl`, `EnumDecl`, `EnumVariant`) in `src/syntax/kind.rs`.
- [ ] **Step 7 — Failing tests** in `src/check/infer/table.rs`: building the table from a tree gives
  each class an id, resolves `extends` to the parent id (with a visited-set so a cyclic `extends`
  terminates), records field name→`CheckTy` and method name→return `CheckTy`, and records each enum's
  variants; an unknown superclass name → no parent (chain ends).
- [ ] **Step 8 — Implement** `src/check/infer/table.rs`: `Table::build(tree, resolved) -> Table`
  walking every `ClassDecl`/`EnumDecl` once; `Table::is_subclass(child, ancestor) -> bool`
  (visited-set bounded). Field/method types lower via `CheckTy::from_type_node` (so the table and `ty`
  are mutually consistent — build the table first with names, then resolve types in a second pass to
  allow forward class references).
- [ ] **Step 9 — Run** `cargo test --lib infer::table 2>&1 | tail` → green. Commit:
  `feat(check): class/enum symbol table for the type checker`.

### T1.3 — No-op pass wired into the driver + corpus differential locked in

- [ ] **Step 10 — Implement** `src/check/infer/mod.rs`: `pub fn check(tree: &ResolvedNode, resolved:
  &ResolveResult, _src: &str) -> Vec<AsDiagnostic>` that builds the `Table` and returns
  **`Vec::new()`** (emits nothing — load-bearing later). Add `pub mod infer;` to `src/check/mod.rs`.
  Wire into `src/check/analyze.rs` after the `rules::ALL` loop:
  `diagnostics.extend(crate::check::infer::check(&tree, &resolved, src));`.
- [ ] **Step 11 — Add the three codes** to `RULE_CODES` in `src/check/config.rs:27`
  (`"type-mismatch"`, `"type-error"`, `"possibly-nil"`) so `--deny`/config validation accepts them
  now (no-op until T2 emits them).
- [ ] **Step 12 — Write the corpus differential test** in `tests/check.rs` (sibling to
  `corpus::checker_is_clean_on_the_corpus`): walk `examples/` recursively, run
  `ascript::check::analyze(&src)`, count diagnostics whose code is in
  `{type-mismatch, type-error, possibly-nil}`, assert the total is **0**. (Green by construction now;
  becomes the real gate in T2–T4.)
- [ ] **Step 13 — Run** the full gate set (both feature configs + clippy both + `vm_differential`
  unchanged). The corpus test passes (pass emits nothing).
- [ ] **Step 14 — Commit:** `feat(check): wire no-op infer pass + corpus zero-new-diagnostic gate + type-* codes`.

---

## Task T2 — Annotated-signature checking + synthesis (emits `type-mismatch`/`type-error`)

Make the pass load-bearing for the **annotated** surface (no narrowing, no return inference yet). This
already subsumes `contract-mismatch` + `field-default-type`.

**Files:** `src/check/infer/{env.rs (NEW),pass.rs (NEW),mod.rs}`. Test `tests/check.rs`,
`src/check/infer/pass.rs` inline.

### T2.1 — Synthesis + the inferred-binding environment

- [ ] **Step 1 — Failing tests** (`tests/check.rs`): `let x: string = "a"; x - 1` → `type-error`
  (arithmetic on a provably-`string`); `let n: number = "x"` → `type-mismatch`; `fn f(n: number)
  { return n }\nf("x")` → `type-mismatch` (the `contract-mismatch` superset, now non-literal too:
  `let s = "x"; f(s)` also fires); `class P { n: number = "x" }` → `type-mismatch` (subsumes
  `field-default-type`). NEGATIVE: `let x = foo(); x - 1` (foo unknown → `Any`) → silent;
  `fn g(x) { return x }\ng("x")` (unannotated param) → silent; `let a: any = 1; a - "x"` → silent.
- [ ] **Step 2 — Implement** `src/check/infer/env.rs`: an `Env` mapping a `BindingKey` (derived from
  `Resolution` — `Local(slot)`/`Upvalue(slot)`/`Global(name)` within the current frame) to a
  `CheckTy`, with push/pop scopes. `src/check/infer/pass.rs`: `synth(expr) -> CheckTy` (spec §2
  synthesis rules; default `Any`) and `check(expr, expected)` (compute `assignable(synth(expr),
  expected)`; on `No` push `type-mismatch`). Walk `let`/`const`/param/`return`/field-default and call
  args against annotated params (reuse the `param_types` shape from `contract.rs`).
- [ ] **Step 3 — `type-error`** (operation provably ill-typed regardless of a slot): in `synth` for
  `BinaryExpr`, if an arithmetic operator has a provably-non-numeric (and non-string-for-`+`) operand,
  push `type-error`; for `CallExpr` on a provably non-callable; for `IndexExpr` on a provably
  non-indexable. Each gated on a **provable** (`No`-class) fact — never on `Unknown`.
- [ ] **Step 4 — Run** `cargo test --test check 2>&1 | tail` → the T2 positives fire, negatives silent.

### T2.2 — Legacy subsumption + de-dup + corpus gate

- [ ] **Step 5 — De-dup tests:** assert that for a literal case both the legacy
  `contract-mismatch`/`field-default-type` AND `type-mismatch` would describe the same span — and that
  the pass **suppresses its own `type-mismatch` at a span already covered by the legacy rule** (span-
  keyed de-dup in `mod.rs::check`, comparing against the legacy diagnostics — pass the `&[AsDiagnostic]`
  collected so far, OR re-run the two legacy `type_compat` checks internally and skip those spans).
  Keep both legacy rules in `rules::ALL` unchanged (one-release overlap, spec §6).
- [ ] **Step 6 — CRITICAL: corpus gate.** `cargo test --test check corpus 2>&1 | tail` → the
  type-diagnostic count is still **0** on the whole corpus, in BOTH feature configs. If any corpus
  program lights up, the `synth`/`assignable` guard is too aggressive — make it more `Unknown`/silent
  (e.g. an unfamiliar expression must synth `Any`), never edit the corpus or the assertion. Iterate
  until 0.
- [ ] **Step 7 — Full gate set** (both feature configs + clippy both + `vm_differential` unchanged).
- [ ] **Step 8 — Commit:** `feat(check): type-mismatch/type-error for annotated slots (subsumes contract-mismatch + field-default-type)`.

---

## Task T3 — Local return-type inference + nil-guard narrowing + `possibly-nil`

**Files:** `src/check/infer/{env.rs,pass.rs}`. Test `tests/check.rs`, inline.

### T3.1 — Local return-type inference (in-file)

- [ ] **Step 1 — Failing tests:** `fn id(x: number) { return x }\nlet y = id(1)\nlet z: string = y`
  → `type-mismatch` (the inferred return `number` flows to the `string` slot). NEGATIVE: a recursive
  fn, or one returning from many branches with mixed types, infers a `join` (or `Any`) and does not
  false-positive.
- [ ] **Step 2 — Implement** in `pass.rs`: a function's inferred return type = `join` of all its
  `return` expression synths (and `Nil` if it can fall off the end). Use the inferred return at call
  sites **within the same file** when the callee resolves to a local/in-file `fn`. Cross-module callees
  stay `Any` (spec non-goal). Bound any recursion (a function under inference resolves to `Any` for its
  own recursive calls — no fixpoint loop).
- [ ] **Step 3 — Run** → green; corpus still 0.

### T3.2 — nil-guard narrowing + `possibly-nil`

- [ ] **Step 4 — Failing tests:** `fn f(x: number?) { return x + 1 }` → `possibly-nil` (deref of a
  provable `T?` without a guard). NARROWED-SILENT: `fn f(x: number?) { if (x != nil) { return x + 1 }
  return 0 }` → silent (then-branch narrows `x` to `number`); `if (x == nil) { return 0 }\nreturn x +
  1` → silent (early-return merge narrows the tail); `let y = x ?? 0\nreturn y + 1` → silent; `if (x)
  { return x + 1 }` → silent (truthiness narrows away `Nil`). NEGATIVE-SILENT: an `any`-typed or
  unknown receiver → no `possibly-nil`.
- [ ] **Step 5 — Implement** the narrowing overlay in `env.rs` (a `HashMap<BindingKey, CheckTy>`
  pushed/popped at branch boundaries) and the nil-guard forms in `pass.rs` (spec §4 form 1:
  `!= nil`/`== nil` then/else, early-return negation merge, `??`, truthiness-narrows-`Nil`-only). Emit
  `possibly-nil` ONLY when a receiver is **provably** `Union[..,Nil]` AND no narrowing applies (spec
  §6). `NameRef` synth consults the narrowing overlay first.
- [ ] **Step 6 — CRITICAL corpus gate** (`possibly-nil` is the noisiest code): the corpus type-
  diagnostic count stays **0**, both feature configs. If a corpus `T?` deref legitimately lights up,
  extend narrowing or tighten the gate — never relax the differential. This is the acceptance gate for
  shipping `possibly-nil` enabled-by-default.
- [ ] **Step 7 — Full gate set + commit:** `feat(check): local return inference + nil-guard narrowing + possibly-nil`.

---

## Task T4 — `instanceof` (SP2-gated) + `match` narrowing + early-return flow merge

**Files:** `src/check/infer/{env.rs,pass.rs}`. Test `tests/check.rs`, inline.

> **SP2 dependency (spec §4 / §9):** `instanceof` narrowing requires SP2's `x instanceof C` operator.
> **Before starting T4, verify SP2 has landed** (`grep -rn instanceof src/syntax src/ast.rs` shows the
> operator). **If SP2 has NOT landed,** ship T4 with `match`-narrowing + early-return-merge ONLY and
> move `instanceof` narrowing to an SP2-follow-up task (note it in the commit + docs). The corpus gate
> and the rest of T4 are independent of SP2.

### T4.1 — match-pattern narrowing + flow merge

- [ ] **Step 1 — Failing tests:** a `match` over an enum where each arm narrows the subject to its
  variant (`EnumVariant`); a `match` with a `nil` arm narrowing to `Nil` and the other arm to `T`; an
  `if/else` with no early exit merging narrowed facts by `join` at the join point. Assert no false
  positive when a narrowed subject is used per-arm.
- [ ] **Step 2 — Implement** `match`-pattern narrowing (spec §4 form 3 — reuse the `ast::Pattern`
  shape) and the join-point merge (spec §4 form 4). Exhaustive enum `match` narrows the fall-through to
  `Never` but emits no diagnostic (spec §1.1).
- [ ] **Step 3 — Run** → green; corpus still 0.

### T4.2 — instanceof narrowing (iff SP2 landed)

- [ ] **Step 4 — (SP2-gated) Failing tests:** `if (x instanceof Dog) { /* x : Dog */ x.bark() }` →
  silent (narrowed); the else-branch subtracts `Dog` from a class union. Skip this step entirely if
  SP2 has not landed (and record the deferral).
- [ ] **Step 5 — Implement** `instanceof` narrowing (spec §4 form 2): then-branch narrows to
  `Class(C)`; else-branch `meet`-subtracts `Class(C)` from a union of classes.
- [ ] **Step 6 — CRITICAL corpus gate** (0, both configs) + full gate set.
- [ ] **Step 7 — Commit:** `feat(check): match + instanceof narrowing, early-return flow merge`
  (note in the body if `instanceof` was deferred for no-SP2).

---

## Task T5 — LSP surface: hover types

**Files:** `src/lsp/analysis.rs` (the only file outside `src/check/` SP10 touches). Test inline in
`src/lsp/analysis.rs`.

- [ ] **Step 1 — Read** `src/lsp/analysis.rs:325` (`hover`) and `:73` (`analyze` reuse). The new
  `type-*` diagnostics ALREADY surface in the editor for free (the LSP calls `crate::check::analyze`,
  verified `:73`) — confirm with a test that a `type-mismatch` appears in the published diagnostics.
- [ ] **Step 2 — Failing hover test:** hovering an annotated/inferred binding's name shows its
  `CheckTy` (e.g. hovering `n` in `let n: number = 1` shows `number`; hovering `y` in
  `let y = id(1)` where `id` returns `number` shows `number`). Hover on an `any`/unknown shows `any`.
- [ ] **Step 3 — Implement** a hover hook: when the hovered token is a binding name with a known
  inferred/declared `CheckTy`, append the type to the hover markup. Run the SP10 pass (or a thin reuse
  of its `Env`) for the document; reuse `Display for CheckTy` (widen internal artifacts — spec §1).
  No interpreter. Keep existing hover behavior (keyword/builtin docs) intact.
- [ ] **Step 4 — Run** `cargo test --lib lsp 2>&1 | tail` → green; existing hover tests unchanged.
- [ ] **Step 5 — Full gate set + commit:** `feat(lsp): hover shows inferred/declared types; type-* diagnostics in editor`.

---

## Task T6 — Docs + holistic review (closing)

**Files:** `docs/content/*`, `docs/superpowers/specs/2026-05-29-ascript-design.md` (if a type-checking
mention is warranted), changelog/migration note.

- [ ] **Step 1 — Docs:** add a "type checking" section to the language guide under `docs/content`
  (the three codes, advisory-only, the narrowing forms, the `any`/gradual boundary); document the
  **legacy-code deprecation** (`contract-mismatch`/`field-default-type` deprecated in favor of
  `type-mismatch`, removed from `rules::ALL` in release N+1, codes kept as accepted no-op aliases).
  Verify every documented snippet against the binary (`ascript check`).
- [ ] **Step 2 — Holistic gate:** full gate set both feature configs + clippy both + the corpus
  differential (0) + `vm_differential` unchanged.
- [ ] **Step 3 — Independent review:** re-read the spec; re-run the gates; adversarial **false-positive
  hunt** — feed the reviewer idiomatic untyped snippets and confirm silence; feed annotated mismatches
  and confirm signal; confirm the three-valued `Unknown ⇒ silent` discipline holds at every new
  diagnosing site. Fix any false positive at the root (relax the guard).
- [ ] **Step 4 — Final commit** if review surfaced fixes; otherwise the sub-project is complete.

---

## Self-review (author)

**Spec coverage:** §1 lattice/assignable/join → T1.1; class/enum table (resolver exposes none) →
T1.2; no-op-pass-land-before-load-bearing + corpus differential → T1.3; §2 bidirectional
synthesis/checking + `type-mismatch`/`type-error` + legacy subsumption/de-dup → T2; §2.1 local
return inference + §4 form 1 nil-guards + §6 `possibly-nil` → T3; §4 forms 2–4 instanceof
(SP2-gated)/match/flow-merge → T4; §6 LSP surface → T5; §10 docs + legacy deprecation + holistic
review → T6. All covered.

**Owner decisions honored:** advisory-only (no front-end/VM/value.rs change, asserted via
`vm_differential` unchanged every task); local-bidirectional intra-procedural, params default `any`
(T2/T3); `possibly-nil` default-Warning, narrowing-gated, corpus-validated (T3.2 acceptance gate);
legacy one-release overlap then deprecate (T2.2 + T6); narrowing v1 = nil-guard + instanceof(SP2) +
match + early-return-merge, alias/closure/custom-guard deferred (T3/T4); three-valued `assignable`,
only `No` diagnoses, every task gated on the whole-corpus zero-new-diagnostic differential
(Conventions + T1.3/T2.2/T3.2/T4.2); no new syntax (no grammar/fmt/parser task exists).

**Placeholder scan:** no "TBD / handle edge cases". Test snippets are concrete AScript; the deferred-
to-implementer detail is exact Rust signatures inside `src/check/infer/*` (the implementer reads the
cited line numbers — `ast.rs:127`, `check/rules/mod.rs:151-253`, `resolve/types.rs:77`,
`check/analyze.rs:77`, `check/config.rs:27`, `lsp/analysis.rs:325`). The SP2 dependency for
`instanceof` narrowing has an explicit verify-and-branch instruction (T4).

**Type consistency:** `Compat3 { Yes, No, Unknown }` is the three-valued result; `CheckTy` carries
`ClassId`/`EnumId` as `usize` table indices; the pass integrates as a single `infer::check` with the
`Rule` signature `fn(&ResolvedNode, &ResolveResult, &str) -> Vec<AsDiagnostic>` (verified
`check/rules/mod.rs:25`). The three new codes appear in T1.3 (`RULE_CODES`), emit starting T2, and the
corpus differential counts exactly those three. Consistent.
