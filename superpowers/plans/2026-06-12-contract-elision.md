# Contract Elision via Static Proof (ELIDE) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** When the TYPE checker can PROVE — under the spec's strict (E)(Y)(A) predicate, which
is deliberately stronger than raw `Compat3::Yes` — that a call site's arguments satisfy the
callee's parameter contracts, that an annotated `let`'s initializer satisfies its annotation,
or that every return of a fn satisfies its declared return type, the runtime check at that
site is elided **identically on both engines** (VM: `Op::CallElided` / skipped `CheckLocal` /
`proto.ret = None`; tree-walker: a per-module AST marking pass), behind a permanent
`--no-elide` kill switch, proven invisible by the elide-on↔elide-off differential cross-axis,
a fuzzer axis, a paranoid proof-violation mode, and count-parity coverage assertions.

**Spec:** `superpowers/specs/2026-06-12-contract-elision-design.md` (ELIDE). **Read it first
and in full** — §0 (why raw `Yes` is unsound), §2 (the predicate), §3 (the classification
table), §4 (mechanism) are the normative design; section references (§) below are into it.

**Architecture:** Unit A (static side): `src/check/infer/elide.rs` — the `ElisionSet` collector
as a diagnostic-neutral mode of the existing pass (the hover-mode precedent), plus the rule-6
bug fix in `ty.rs`. Unit B (VM side): `compile_source` takes `Option<&ElisionSet>`; skipped
`CheckLocal`, `proto.ret = None`, new `Op::CallElided` (ASO bump, verify/disasm/bcanalysis
arms); `check_call_args` gains `elide_contracts: bool`. Unit C (tree-walker side):
`ExprKind::Call.elide_args` field + the marking pass in the module loader + the
`call_value_elided` threading. Unit D (decision + correctness + perf): the measured §5.1
decision, differential/fuzz/paranoid/coverage gates, bench, examples, docs.

**Tech stack:** Rust; the CST checker (`src/check/infer/`, static-only, feature-independent —
must build under `--no-default-features`); the async bytecode VM (`src/vm/`); the legacy
tree-walker (`src/interp.rs`, `!Send`, never add `Send` bounds); tests via `cargo test` in BOTH
feature configs; `tests/vm_differential.rs`; `fuzz/fuzz_targets/differential.rs`;
`tests/vm_bench.rs` (Gate 12/17); `/usr/bin/time -l` for RSS (Gate 18).

**Hard rules carried from the spec:**
- The collector NEVER changes diagnostics (§6.5 — asserted by test) and ignores the lint
  config (§5.4). `run`'s diagnostic gate is NOT changed (§5.3).
- Every lookup is exact-match **fail-safe** (miss ⇒ check kept); soundness never rests on a
  probability (§4.3).
- `ASO_FORMAT_VERSION`: **read the current constant and bump by one** (it was 27 when the spec
  was written, `src/vm/aso.rs:167` — verify, don't assume).
- Line numbers in this plan were verified 2026-06-12; **re-grep every symbol before editing**
  (the named functions are the anchors, not the line numbers).

**Binding execution standards (production-grade mandate):** any bug found while working — ours
or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first regression
guard, never stepped around (goal.md Gate 14). No placeholders, no silent deferrals. Commit per
task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `src/check/infer/elide.rs` — `ElisionSet` + the collector (§4.1) + the (E)(Y)(A) predicate
  helpers (`ElideSafe` over `CheckTy`-lowered nodes, the anchoring walker, the kind table).
- `src/elide_mark.rs` — the tree-walker marking pass (CORE module, no feature gate).
- `tests/elide.rs` — the §3.4 probe pins, per-row positive/negative batteries, count-parity,
  diagnostic-neutrality, kill-switch, paranoid-mode tests.
- `bench/profiling/call_heavy.as` + `bench/profiling/call_heavy_typed.as` — the headline pair
  (§7, §8.2 — created here only if LANE Task 0 hasn't already landed an equivalent; Task 0.1
  decides).
- `bench/run_elide_bench.sh` + `bench/ELIDE_RESULTS.md` — same-session A/B + RSS report.
- `examples/typed_contracts.as` (intro) + `examples/advanced/typed_pipeline.as` (production-
  shaped) — typed happy-path + gradual-boundary edge examples (Gate 9).

**Modified files:**
- `src/check/infer/ty.rs` — rule-6 `Class→Object` fix (`Yes` → `Unknown`).
- `src/check/infer/pass.rs` — `elide: Option<ElideCollect>` mode beside `hover`; recording in
  `walk_let`/`walk_return`/`check_call_args`/`walk_fn`; per-scope anchoring beside `Env` types.
- `src/check/infer/env.rs` — binding entries carry `(CheckTy, anchored: bool)`.
- `src/check/infer/mod.rs` — `pub fn elision_proofs(tree, resolved, src) -> ElisionSet`.
- `src/vm/opcode.rs` — `Op::CallElided` (+ round-trip, operand-width, name tables).
- `src/vm/verify.rs`, `src/vm/disasm.rs`, `src/vm/bcanalysis.rs` — the new arm.
- `src/vm/aso.rs` — `ASO_FORMAT_VERSION` bump (current+1).
- `src/vm/run.rs` — `Op::CallElided` joins the call arm; `elide_contracts` threading; the
  paranoid escalation hook on the contract-failure paths.
- `src/compile/mod.rs` — `compile_source_with_elision(src, Option<&ElisionSet>)`; skip
  `emit_check_local`; `proto.ret = None`; `Op::CallElided` at proven call spans; consumed-count
  accessor for parity tests.
- `src/ast.rs` — `ExprKind::Call { callee, args, elide_args }`.
- `src/parser.rs` — construct `elide_args: false`.
- `src/interp.rs` — `check_call_args(..., elide_contracts: bool)`; `call_value_elided` wrapper
  chain; `Interp` paranoid-set field (failure-path-only).
- `src/lib.rs` + `src/main.rs` — `--no-elide` flag, `ASCRIPT_NO_ELIDE`,
  `ASCRIPT_ELIDE_PARANOID`; collector wiring in `run`/`build`/`test` source paths + the module
  loaders (both engines); `vm_run_source_no_elide` test entry.
- `tests/vm_differential.rs` — elide axis + cross-axis; `fuzz/fuzz_targets/differential.rs` —
  fuzz axis; `tests/vm_bench.rs` — Gate-12/17 re-run.
- `docs/content/language/type-contracts.md` — "Annotations and performance" section.
- `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md` (status flip + the §2.4 stanza
  correction) — final task.

---

## Phase 0 — Preflight: baselines, probes, and the MEASUREMENT that gates the decision

### Task 0.1: bench corpus check + baseline numbers

**Files:** create `bench/run_elide_bench.sh`, `bench/ELIDE_RESULTS.md`; maybe create
`bench/profiling/call_heavy{,_typed}.as`.

- [ ] **Step 1:** `ls bench/profiling/` — if LANE Task 0 landed a call-heavy workload, adopt it
  and write only the **typed** variant; otherwise create both. `call_heavy.as`: a hot loop of
  direct fn calls (`fn add(a, b) { return a + b }`-shaped, 3–5 small fns, ~2M calls, result
  printed; `time.monotonic()` headline). `call_heavy_typed.as`: the SAME workload with
  `int`/`string` annotations on every param, return, and let. Verify both run on both engines:
  `target/release/ascript run bench/profiling/call_heavy_typed.as` and `--tree-walker`.
- [ ] **Step 2:** Write `bench/run_elide_bench.sh` (mirror `bench/run_shared_heap_bench.sh`):
  release build; runs call_heavy / call_heavy_typed / object_churn / json_roundtrip + the
  examples corpus under `/usr/bin/time -l` (wall + max RSS); prints a table. Run on the
  unmodified branch base; paste into `bench/ELIDE_RESULTS.md` under
  `## Baseline (pre-ELIDE, same session)`. Capture one `ascript run --profile cpu` artifact for
  `call_heavy_typed.as` and note the `check_call_args` share (the contract-cost evidence line).
- [ ] **Step 3:** Commit — `bench(elide): baseline harness + pre-ELIDE numbers`.

### Task 0.2: pin the probe battery (failing-test-first for the whole spec)

**Files:** create `tests/elide.rs`.

- [ ] **Step 1:** Write the four §3.4 probes as tests that pass TODAY (they pin current
  semantics the design depends on):

```rust
//! ELIDE — semantics pins + (later) elision batteries. The probes document the
//! exact behaviors the spec's predicate is shaped around (spec §0/§3.4).
use std::process::Command;
fn run_cli(args: &[&str]) -> (String, String, i32) { /* spawn env!("CARGO_BIN_EXE_ascript"), capture */ }

#[test]
fn probe_run_has_no_static_type_gate() {
    // `fn f(p: string){} f(1)`: check blocks (exit != 0), run executes and
    // RUNTIME-panics with the contract message (spec §5.3 — ELIDE must not change this).
}
#[test]
fn probe_reassignment_is_not_contract_checked() {
    // `let x: int = 5  x = "s"  print(x)` runs clean on BOTH engines.
}
#[test]
fn probe_mutated_binding_yes_is_not_a_runtime_guarantee() {
    // `fn f(p: int){...} let x: int = 5  x = "s"  f(x)`: check exit 0, run panics.
    // THE landmine: under ELIDE this site must stay un-elided (asserted again in 3.x).
}
#[test]
fn probe_object_contract_rejects_instances() {
    // `class C{} fn f(p: object){} f(C())` panics at runtime on both engines.
}
```

- [ ] **Step 2:** `cargo test --test elide` green; also `--no-default-features`. Commit —
  `test(elide): semantics-pin probe battery (spec §3.4)`.

### Task 0.3: MEASURE the collector envelope (REQUIRED before Task 4.1's decision)

**Files:** none committed yet (a scratch bin / criterion-style harness is fine, or a temporary
`#[test]` with `--nocapture`); results into `bench/ELIDE_RESULTS.md`.

- [ ] **Step 1:** Measure, same-session: (a) `parse_to_tree + resolve + Table::build +
  pass::run` wall time per file over `examples/**` + `examples/advanced/**` + a generated
  ~5k-line module (concatenated examples are fine) — this is the collector's cost ceiling (the
  collector is the same walk + cheap set inserts); report min/median/max. (b) End-to-end
  `ascript run` of the example corpus (hyperfine or a 20-iteration loop), recording per-file ms.
  Compute the projected regression %.
- [ ] **Step 2:** Record in `bench/ELIDE_RESULTS.md` under `## Collector cost envelope` with
  the spec's budget stated beside it (§5.1: ≤2% corpus geomean AND ≤1 ms absolute for a
  ≤500-line module). **Do not decide here** — Task 4.1 decides against these numbers.
- [ ] **Step 3:** Commit — `bench(elide): collector cost envelope (decision input, spec §5.1)`.

### Task 0.4: Phase 0 review

- [ ] Independent reviewer: probes actually pin live behavior (re-run them by hand against the
  release binary); baseline + envelope numbers are real captured output; bench scripts re-run
  clean; typed bench variant is genuinely fully annotated (grep for unannotated params).

---

## Phase 1 — Unit A: the static side (collector + predicate + rule-6 fix)

### Task 1.1: rule-6 fix — `Class → Object` verdict becomes `Unknown`

**Files:** modify `src/check/infer/ty.rs`; test in `src/check/infer/ty.rs` unit tests +
`tests/check.rs` if it has a rule-6 pin.

- [ ] **Step 1 (failing test):** unit test asserting
  `assignable(Class(c), Object) == Compat3::Unknown` (currently `Yes`) + a `tests/elide.rs`
  integration twin: the probe-4 program stays checker-silent AND runtime-panicking.
- [ ] **Step 2:** Locate the rule-6 arm in `assignable_depth` (grep `Object` arms near
  `ty.rs:398+`); change the `Class(_)`/`ClassApp(..)` → `Object` result from `Yes` to
  `Unknown`, with a comment citing ELIDE §6.6 + the runtime divergence (`interp.rs` `check_type`
  `Object` arm rejects instances). **Do NOT make it `No`** (that could add corpus diagnostics —
  TYPE-follow-up territory; the comment says so).
- [ ] **Step 3:** Gate-5 sweep: `cargo test check` (the `corpus::` tripwire) in BOTH configs —
  zero `type-*` on `examples/**` still holds (an `Unknown` can only remove emissions, but run
  it anyway). Full `cargo test --test check` green.
- [ ] **Step 4:** Commit — `fix(check): instance→object assignability is Unknown, not Yes
  (runtime rejects instances; ELIDE §6.6, Gate-14 in-branch fix)`.

### Task 1.2: `ElisionSet` + the ElideSafe and kind tables

**Files:** create `src/check/infer/elide.rs`; register in `src/check/infer/mod.rs`.

- [ ] **Step 1 (failing tests):** unit tests for the two pure predicates, straight from spec
  §2.2/§2.3 tables: `elide_safe(&CheckTy)` (Int/Float/Number/String/Bool/Nil/Fn/FnSig/Any-free-
  pass/Array(Any)/unions-thereof yes; Object/Named-derived (Class/Enum/Interface)/deep
  Array/Map/Tuple/Result/Future/Error no) and `arith_result_kind(op, lhs_kind, rhs_kind)` —
  the NUM-mirror table (int∘int→int incl. `/`; mixed→float; `+` strings→string; bitwise→int;
  comparisons→bool; `&&`/`||`/`??` → None i.e. never anchored).
- [ ] **Step 2:** Implement `ElisionSet { calls, lets, fn_rets: HashSet<(u32,u32)> }` with a
  `byte_range_to_char_span(src, range)` helper (one pass over the module text, memoized
  prefix array — NOT O(n) per conversion). Keys per §4.1: call-expr extent / initializer
  extent / fn name-token extent, all trivia-trimmed via the pass's existing `code_range`.
- [ ] **Step 3:** Kind-table battery (spec §6.7): a unit test that, for every operand-kind
  pair × operator in the table, runs a tiny program on the VM (`vm_run_source`) computing the
  op and asserts the RUNTIME result kind (via `type(x)` printing) matches
  `arith_result_kind` — synth-vs-runtime pinned exhaustively. (This is a build-time test over
  ~100 micro-programs; keep it in `tests/elide.rs` if compile-time matters.)
- [ ] **Step 4:** Both configs build (`elide.rs` is feature-independent). Commit —
  `feat(check/elide): ElisionSet + ElideSafe/kind tables (ELIDE §2.2-2.3)`.

### Task 1.3: anchoring + collection inside the pass

**Files:** modify `src/check/infer/pass.rs`, `src/check/infer/env.rs`,
`src/check/infer/mod.rs`, `src/check/infer/elide.rs`.

- [ ] **Step 1 (failing tests):** `elision_proofs(src)`-level unit tests (a thin helper that
  parses + resolves + collects), asserting set membership per §2.3 row:
  - `fn f(a: int, b: string) {} f(1, "x")` → 1 call key (literals anchored).
  - `fn f(p: int){} let x: int = 5  f(x)` → let key + call key (unmutated annotated binding).
  - `fn f(p: int){} let x: int = 5  x = 7  f(x)` → **no call key** (mutated ⇒ unanchored; the
    `x = 7` being provably fine doesn't matter in v1), let key still present.
  - probe-3 program → **no call key** (the soundness pin).
  - `fn g(): int { return 1 } fn f(p: int){} f(g())` → call key (anchored via declared
    ElideSafe return) + `g`'s fn_ret key (all returns proven, always-returns).
  - `fn f(p: int){} f(unknown())` / `f(anyTyped)` → no key (rule-1 Yes excluded via anchoring).
  - spread / named args / rest-param callee / async callee / `worker fn` callee / generator
    callee → no key.
  - arity mismatches that `pass.rs` never checks (`f(1,2,3)` on 2-arity) → no key (the
    collector's own arity count).
  - `fn f(p: int?){} if (x != nil) { f(x) }` with `let x: int? = 5` (unmutated) → call key
    (narrowed-from-anchored).
  - ternary / `!` / comparison / arithmetic anchored-composition cases; logical-op exclusion.
- [ ] **Step 2:** Implement: `Env` entries become `(CheckTy, anchored: bool)` (default
  `false`); `bind_params` sets `anchored = elide_safe(ann_ty)` for annotated params (rest ⇒
  false); `walk_let` sets `anchored = (ann.is_some() && elide_safe) || (ann.is_none() &&
  init_anchored)`, BOTH gated on the resolver binding's `mutated == false`
  (`self.resolved.bindings`, match by `decl_range` — the same lookup `binding_key_of_decl`
  uses). Add `Pass.elide: Option<ElideCollect>` (mirrors `hover`); an `anchored_synth(expr,
  env) -> (CheckTy, bool)` used at collection points only — implement as a wrapper that calls
  `synth` and computes anchoring by the §2.3 structural allowlist (literals, paren, unary,
  binary via the kind table, nameref via env-anchored flag, ternary, eligible CallExpr); record
  keys in `walk_let` (ann ElideSafe + verdict Yes + anchored), `check_call_args` (full row-1
  eligibility incl. self-counted arity, per-param (E)(Y)(A), free-pass params), `walk_return` +
  `walk_fn` (per-fn accumulator: declared ElideSafe ret, every return Yes+anchored, AND
  `block_always_returns(body)` OR `Nil` Yes against ret). Re-grep `block_always_returns` for
  its real signature.
- [ ] **Step 3 (diagnostic-neutrality gate, spec §6.5):** test in `tests/elide.rs`: for every
  file in `examples/**`, `analyze(src).diagnostics` is byte-identical whether or not collection
  mode ran (run the pass twice, compare). Both feature configs.
- [ ] **Step 4:** `cargo clippy --all-targets` + `--no-default-features --all-targets` clean.
  Commit — `feat(check/elide): anchoring + proof collection in the pass (ELIDE §2, §4.1)`.

### Task 1.4: Phase 1 review

- [ ] Independent reviewer runs the Step-1 batteries, then ADVERSARIALLY probes: shadowed fn
  names (two `fn f` in different scopes — `resolve_in_file_fn` must bail), a param named like a
  global, captured-and-mutated bindings, a narrowing chain across `else`, `let x: int` with NO
  initializer then `f(x)` (x is nil at runtime! — must NOT be proven: no initializer ⇒
  `walk_let` records nothing and the binding anchors only via its annotation… **reviewer
  explicitly verifies the collector treats uninitialized annotated lets as UNANCHORED** — the
  runtime binds `nil` without checking, `interp.rs:2964-2968`; if the implementer missed this,
  it is a REAL soundness bug to fix now). Gate-5 sweep re-run. Diagnostic-neutrality re-run.

---

## Phase 2 — Unit B: the VM side

### Task 2.1: `Op::CallElided` + format plumbing (inert — nothing emits it yet)

**Files:** modify `src/vm/opcode.rs`, `src/vm/verify.rs`, `src/vm/disasm.rs`,
`src/vm/bcanalysis.rs`, `src/vm/aso.rs`, `src/vm/run.rs`.

- [ ] **Step 1 (failing test):** opcode round-trip test (the existing `opcode.rs` table tests)
  including `CallElided`; a `verify.rs` unit: a hand-built chunk with `CallElided argc=2`
  verifies with the same stack effect as `Call`; disasm renders `CALL_ELIDED 2`.
- [ ] **Step 2:** Add the variant (next free byte — read the enum, don't assume), `u8` operand,
  `operand_width` arm, from-byte round-trip, name table; `verify.rs` `Effect` mirroring `Call`;
  disasm + bcanalysis arms; **bump `ASO_FORMAT_VERSION` by one** (read current; it was 27) with
  the doc-comment line explaining ELIDE. Run-loop: extend the `Op::Call | Op::CallSpread` arm
  (`run.rs:1570`) to `| Op::CallElided`, computing `let elide = matches!(op, Op::CallElided)`
  and threading it to the closure-binding call(s) in that arm (next task gives
  `check_call_args` the param; for THIS commit pass `false` so behavior is unchanged — the
  opcode is dispatchable but semantically identical to `Call`).
- [ ] **Step 3:** Full `cargo test` both configs (aso goldens regenerate if any pin the
  version constant — fix forward). Commit — `feat(vm): Op::CallElided opcode + format plumbing,
  ASO bump (ELIDE §4.2; semantically =Call until wired)`.

### Task 2.2: `check_call_args` elide mode + the run-loop wiring

**Files:** modify `src/interp.rs` (`check_call_args` + every caller), `src/vm/run.rs`.

- [ ] **Step 1 (failing tests):** unit tests on `check_call_args(..., elide_contracts=true)`:
  wrong-typed arg passes through unchecked (returns bound values), arity errors STILL fire
  byte-identically (same message/span), defaults range identical, rest collection identical
  (with element checks skipped).
- [ ] **Step 2:** Add the `elide_contracts: bool` parameter; guard the per-param check loop
  (`interp.rs:7977-7990`) and the rest-element check (`:8018-8026`) — nothing else. Update ALL
  existing callers with `false` (grep `check_call_args(` across `interp.rs` + `run.rs` — ~10
  sites). In the VM `Op::Call|CallSpread|CallElided` arm, pass the computed `elide` ONLY on the
  plain-closure frame-push path (the paths a `CallElided` site can reach: sync closure push +
  async-fn scheduling are excluded by eligibility, but pass the flag through whatever the arm's
  shared binder call is so the defensive story is "flag follows the op"); every other
  `check_call_args` site stays `false`.
- [ ] **Step 3:** Commit — `feat(vm/interp): elide_contracts mode on the shared binder (ELIDE
  §4.4)`.

### Task 2.3: compiler consumption

**Files:** modify `src/compile/mod.rs`, `src/lib.rs`.

- [ ] **Step 1 (failing tests):** compile-level tests (in `tests/elide.rs`): compile a typed
  source with a hand-built `ElisionSet` (from `infer::elision_proofs`) and assert via disasm:
  (a) no `CHECK_LOCAL` for the proven let, still present for an unproven one; (b) `CALL_ELIDED`
  at the proven call site, `CALL` at the gradual one; (c) the proven fn's proto has
  `ret: None`; (d) with `None` elision set, bytecode is byte-identical to today (snapshot
  compare against `compile_source`).
- [ ] **Step 2:** `compile_source_with_elision(src, elide: Option<&ElisionSet>) -> Result<…>`
  (existing `compile_source` delegates with `None`); thread to: `emit_check_local` (skip on
  `set.lets` match of `node_code_span(init)`), the fn-proto builder (`ret = None` on
  `set.fn_rets` match of the name-token span), the three `Op::Call` emission sites (emit
  `CallElided` on `set.calls` match of the call's span — ONLY the plain-call sites; named/
  spread/method paths untouched). Add a `consumed_count` (returned beside the chunk or via a
  counter) for the parity gate.
- [ ] **Step 3:** Add `vm_run_source_elided(src)` test entry in `src/lib.rs` (computes the set,
  compiles with it, runs) — the differential harness consumes this next phase. End-to-end test:
  a typed program runs byte-identically via `vm_run_source` vs `vm_run_source_elided`; a
  gradual-boundary program (probe 3) still panics identically under BOTH.
- [ ] **Step 4:** Commit — `feat(compile): consume ElisionSet — skip CheckLocal, drop proven
  ret, emit CallElided (ELIDE §4.2)`.

### Task 2.4: Phase 2 review

- [ ] Reviewer: disasm-verifies the emission decisions on 5+ hand-written programs; builds an
  `.aso` from a typed file and runs it (`ascript build` path not yet wired — use the lib
  entry); confirms `verify.rs` rejects a truncated `CallElided`; runs the full suite both
  configs; greps that NO caller passes `elide_contracts=true` except the `CallElided` arm.

---

## Phase 3 — Unit C: the tree-walker side

### Task 3.1: `ExprKind::Call.elide_args` + the threading chain

**Files:** modify `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1 (failing test):** a unit test constructing a marked Call AST (parse then set the
  flag by hand) and running it via the tree-walker: wrong-typed arg passes through (check
  skipped); unmarked twin panics. Arity errors fire on both.
- [ ] **Step 2:** Add `elide_args: bool` to `ExprKind::Call` (parser constructs `false`;
  exhaustive-match fallout in `fmt.rs`/`ast.rs` Display is field-pattern only — `..` patterns
  absorb it; fix any that don't). In `interp.rs`: the `ExprKind::Call` evaluator
  (`interp.rs:4122`) reads the flag; introduce `call_value_elided(callee, args, span, elide)`
  as the real body of `call_value` (which delegates `false`); thread through `call_function` →
  `run_body` → `check_call_args(…, elide)` — wrapper pattern, existing callers untouched.
  Eligibility means only plain non-async/non-generator/non-worker fn calls can carry `true`;
  the worker/generator/builtin branches in `call_value` ignore the flag (defensive: they pass
  `false` onward).
- [ ] **Step 3:** Commit — `feat(interp): elide_args flag on Call + threaded binder mode (ELIDE
  §4.3)`.

### Task 3.2: the marking pass + loader wiring

**Files:** create `src/elide_mark.rs`; modify `src/interp.rs` (module loader), `src/lib.rs`.

- [ ] **Step 1 (failing tests):** `mark_program(&mut Vec<Stmt>, &ElisionSet) -> MarkCounts`
  unit tests: a typed program's legacy AST gets exactly the expected marks (call flag set,
  `Stmt::Let.ty` stripped, `Stmt::Fn.ret` stripped); a miss (perturbed key) marks nothing;
  counts returned accurately; nested fns/blocks/class bodies are traversed (methods are never
  keys in v1 but the walk must not panic on them).
- [ ] **Step 2:** Implement the recursive walker (statements + expressions; set
  `ExprKind::Call.elide_args` on extent match, strip `ty`/`ret` on match). Wire into the
  tree-walker module pipeline: find where `load_module`/the module loader parses a module's
  legacy AST (grep `parser::parse` / `parse_program` in `interp.rs`); after parse, when the
  Interp's elide mode is on, compute `infer::elision_proofs` for THAT module's source and mark
  before exec/caching. Entry-module path in `run_file_with_packages` gets the same treatment
  (it flows through `load_module`, verify). Per-module scoping is automatic (§4.3).
- [ ] **Step 3 (count parity, spec §6.4):** test: for each typed test program (and later the
  corpus), `marked(tree-walker) == consumed(VM compiler) == |ElisionSet|`. Expose
  `MarkCounts`/compiler consumed-count through test-visible seams.
- [ ] **Step 4:** Four-mode smoke: typed program byte-identical across tree-walker(marked) /
  spec-VM(elided) / generic-VM(elided) / elide-off. Commit — `feat(interp): per-module AST
  marking pass + loader wiring, count parity (ELIDE §4.3)`.

### Task 3.3: Phase 3 review

- [ ] Reviewer probes the cross-front-end keys hard: multi-line calls, calls with comments
  inside arg lists, template-string args, CRLF/unicode sources (char-offset conversion!),
  nested calls `f(g(x))` where only one is proven, a module imported twice, the REPL (must not
  mark — verify no elision path runs there). Runs count-parity over every typed fixture. If ANY
  legacy↔CST extent mismatch surfaces (the ±1 hazard), it is a front-end span bug: fix it
  in-branch with a regression test (Gate 14), do NOT widen the matcher.

---

## Phase 4 — Unit D: decision, kill switch, paranoid mode, correctness gates

### Task 4.1: the §5.1 DECISION + CLI wiring (`--no-elide`, env vars)

**Files:** modify `src/main.rs`, `src/lib.rs`; record in `bench/ELIDE_RESULTS.md` + the spec.

- [ ] **Step 1:** Re-run Task 0.3's measurement ON the current branch (the collector now
  exists — measure the REAL thing, not the proxy): collector wall time per module over the
  corpus + end-to-end `ascript run` A/B (elide-on vs `--no-elide`) including the 5k-line
  module. Append to `bench/ELIDE_RESULTS.md` under `## Decision measurement`.
- [ ] **Step 2:** **Decide against the spec budget** (≤2% corpus geomean AND ≤1 ms absolute
  typical module): inside → default ON for `run`/`build`/`test` source paths; outside → default
  OFF behind `--elide` (and update spec §5.1's table + this plan's remaining tasks
  accordingly — a RECORDED decision, with the numbers, in both documents). The rest of this
  plan is written for the expected inside-budget outcome.
- [ ] **Step 3 (failing tests):** CLI tests in `tests/cli.rs`: `--no-elide` runs a typed
  program with full checks (probe: a paranoid-style marker… simplest observable: identical
  behavior + the disasm/bytecode golden differs — assert via `ascript build --no-elide` vs
  `ascript build` on a typed file producing different `.aso` bytes, and BOTH running
  identically); `ASCRIPT_NO_ELIDE=1` equivalent; `.aso` built WITH elision runs correctly via
  `ascript run file.aso`.
- [ ] **Step 4:** Wire: `--no-elide` flag on Run/Build/Test + `ASCRIPT_NO_ELIDE`; the
  source-path runners compute per-module sets (VM: in `run_file_on_vm_with_packages` +
  the VM module-import loader, feeding `compile_source_with_elision`; tree-walker: Task 3.2's
  seam, gated). `ascript build` runs the collector under the same default. REPL and worker
  slice compiles: explicitly no collector (assert via a worker test: a `worker fn` with a
  wrong-typed internal call still panics in the isolate — full checks there).
- [ ] **Step 5:** Commit — `feat(cli): elision default per measured decision + --no-elide /
  ASCRIPT_NO_ELIDE kill switch (ELIDE §5)`.

### Task 4.2: paranoid mode

**Files:** modify `src/main.rs`/`src/lib.rs` (mode plumb), `src/vm/run.rs` + `src/interp.rs`
(failure-path escalation), `tests/elide.rs`.

- [ ] **Step 1 (failing test):** with `ASCRIPT_ELIDE_PARANOID=1`, a healthy typed program's
  output == elide-off output byte-for-byte; AND a synthetic wrong-proof (inject a fake key into
  the set via a test-only seam) makes the contract failure escalate to a panic message starting
  `ELIDE proof violated (checker soundness bug):` on BOTH engines.
- [ ] **Step 2:** Implement per spec §6.3: paranoid ⇒ compile/mark as elide-OFF but retain the
  per-module sets (VM: on the `Chunk`/module record; tree-walker: on the `Interp` keyed by
  module) consulted ONLY on the contract-failure paths (`contract_panic` call sites for
  let/call/ret) — zero hot-path cost. Escalated message includes site span + expected/actual.
- [ ] **Step 3:** Add a CI-shaped test that runs the whole example corpus + the typed examples
  under paranoid mode (both engines) asserting zero escalations and elide-off-identical output.
  Commit — `feat(elide): paranoid proof-violation mode (ELIDE §6.3)`.

### Task 4.3: differential elide axis + cross-axis + fuzz axis + coverage

**Files:** modify `tests/vm_differential.rs`, `fuzz/fuzz_targets/differential.rs`,
`tests/elide.rs`.

- [ ] **Step 1:** Extend the corpus differential: every corpus/golden program runs the existing
  modes under elide-ON (the default) AND all under elide-OFF, asserting (1) within-axis
  four-mode identity for both axes, (2) **cross-axis equality elide-on == elide-off** (spec
  §6.1 — THE soundness fuzzer). Use `vm_run_source_elided`/`vm_run_source_no_elide` + a
  tree-walker marked/unmarked pair. Both feature configs.
- [ ] **Step 2:** Fuzz axis: `differential.rs` adds the elide-on configuration to its engine
  set (compute proofs from the generated source; compare outputs with the existing engines).
  Run a 10-minute local fuzz batch; any divergence is a Phase-1 predicate bug — fix with a
  failing-test-first reduction in `tests/elide.rs`.
- [ ] **Step 3 (coverage, spec §6.4):** assertions in `tests/elide.rs`: typed examples + typed
  bench produce `|ElisionSet| > 0` per file and report the elision rate (printed + asserted
  ≥ a floor pinned from actuals); pre-existing unannotated examples produce ZERO elisions AND
  byte-identical bytecode vs `--no-elide` (snapshot a hash).
- [ ] **Step 4:** Commit — `test(elide): differential elide axis + cross-axis, fuzz axis,
  coverage/parity gates (ELIDE §6.1-6.4, Gate 15)`.

### Task 4.4: per-row batteries (the classification table, happy + edge)

**Files:** `tests/elide.rs`.

- [ ] **Step 1:** For each elidable row (§3 rows 1–3): positive test (typed program, assert
  elided via disasm/count AND output identical across elide-on/off/four-mode) + negative twins
  keeping checks: mutated binding (probe 3), `any`-typed source, spread arg, named args, rest
  callee, async/generator/worker callee, interface-typed param, `array<int>` param, `object`
  param (probe 4), uninitialized annotated let, fn with non-always-returning body + non-nil
  ret, fn-expression/arrow/method calls. Each negative asserts BOTH not-elided (count) AND the
  runtime panic still fires where it fires today.
- [ ] **Step 2:** Kept-row spot checks (rows 4–12): CheckParam still fires for a bad default at
  a proven call site; `Class.from` validation unchanged; std-call misuse unchanged.
- [ ] **Step 3:** Both configs green. Commit — `test(elide): per-row positive/negative
  batteries (ELIDE §3, Gates 9-10)`.

### Task 4.5: Phase 4 review

- [ ] Reviewer re-runs: full suite both configs, vm_differential both configs, a fresh fuzz
  batch, paranoid corpus run, the CLI kill-switch matrix (`--no-elide` × `ASCRIPT_NO_ELIDE` ×
  paranoid), `.aso` build+run round-trip with elision, and adversarial programs of their own
  invention against the predicate (aim: produce ANY elide-on/off divergence — success means a
  bug to fix now).

---

## Phase 5 — Performance, examples, docs, bookkeeping

### Task 5.1: bench A/B + RSS (Gates 12/16/17/18)

**Files:** `bench/ELIDE_RESULTS.md`, `bench/run_elide_bench.sh`.

- [ ] **Step 1:** Same-session A/B per spec §7: (1) untyped corpus elide-on vs off (expect ≈0;
  any regression is a bug — find it); (2) `call_heavy_typed.as` elide-on vs `--no-elide` (the
  headline) + typed-vs-untyped elide-on; (3) typed advanced example; all with
  `/usr/bin/time -l` RSS columns (expect unchanged) and one `--profile cpu` artifact showing
  the `check_call_args` share shrink. Record elision rates beside the timings.
- [ ] **Step 2:** Re-run `tests/vm_bench.rs` gates: spec/tw geomean ≥2× holds; the DBG
  zero-cost gate unaffected (no dispatch-loop change in the off path). Startup budget number
  re-confirmed in the final default configuration.
- [ ] **Step 3:** Commit — `bench(elide): same-session A/B + RSS + elision rates (ELIDE §7)`.

### Task 5.2: examples (Gate 9) + docs (Gate 13)

**Files:** create `examples/typed_contracts.as`, `examples/advanced/typed_pipeline.as`; modify
`docs/content/language/type-contracts.md`.

- [ ] **Step 1:** `examples/typed_contracts.as` (intro): annotated fns/lets/returns doing real
  work, comments noting these checks are statically proven + elided; runs clean on
  `target/release/ascript run`, fmt-idempotent, zero `type-*` (it joins the Gate-5 corpus).
  `examples/advanced/typed_pipeline.as` (production-shaped, fully error-handled): a typed data
  pipeline with BOTH proven fast-path calls AND an explicit gradual boundary (an `any`-typed
  ingress that keeps its runtime check — with `recover` demonstrating the contract panic still
  guarding it). Four-mode tested by the conformance corpus automatically.
- [ ] **Step 2:** Docs: `docs/content/language/type-contracts.md` gains an "Annotations and
  performance" section — what gets elided (plain-language version of §2/§3), the gradual
  boundary keeps checks, `--no-elide`, the paranoid mode for engine developers; no NAV change
  (existing page). Verify rendered links serve (`cd docs && python3 -m http.server` spot
  check).
- [ ] **Step 3:** Commit — `docs+examples(elide): typed examples (happy+boundary) + contracts
  page section (Gates 9/13)`.

### Task 5.3: bookkeeping + final holistic review + FULL gates checklist

**Files:** `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md`,
`superpowers/specs/2026-06-12-contract-elision-design.md` (decision §5.1 outcome recorded).

- [ ] **Step 1:** `CLAUDE.md`: an ELIDE paragraph in the campaign/subsystem notes (the
  predicate in one line, the kill switch, the cross-axis gate, ASO bump, "raw Yes is not a
  proof" warning for future checker work). `roadmap.md`: the milestone record. `goal-perf.md`:
  flip ELIDE to ✅ AND correct the stanza's "tree-walker keeps full checks" line per spec §2.4.
  Spec: fill in the §5.1 decision with the measured numbers.
- [ ] **Step 2 (holistic review — fresh subagent, REQUIRED):** reviews the ENTIRE branch diff
  against the spec section-by-section, hunting: predicate holes (new anchoring forms smuggled
  in without table rows), diagnostic-neutrality, any `elide_contracts=true` outside the two
  sanctioned paths, ASO/verify completeness, span-key edge cases, doc accuracy. Runs commands;
  evidence before verdict.
- [ ] **Step 3 (the full gates checklist — every box ticked with command output):**
  - [ ] `cargo clippy --all-targets` clean AND `--no-default-features --all-targets` clean.
  - [ ] `cargo test` green AND `cargo test --no-default-features` green.
  - [ ] `tests/vm_differential.rs` green in both configs — elide-on four-mode, elide-off
    four-mode, cross-axis equality.
  - [ ] Fuzzer axis: a recorded local batch (≥30 min total across targets) with zero
    divergences; paranoid CI mode test green (corpus zero-escalation).
  - [ ] Coverage: typed-corpus elision rate > 0 asserted; untyped corpus zero-elision +
    byte-identical bytecode asserted; count parity (collector == compiler == marker) asserted.
  - [ ] Gate 5: zero `type-*` on `examples/**` both configs (incl. the new typed examples).
  - [ ] Gate 12/17: vm_bench geomean ≥2×, zero-cost-off confirmed; startup budget met (or the
    recorded fallback decision applied consistently).
  - [ ] Gate 18: RSS table in `bench/ELIDE_RESULTS.md`, no regression.
  - [ ] bench A/B committed (`bench/ELIDE_RESULTS.md` complete: baseline, envelope, decision,
    headline, rates).
  - [ ] Examples fmt-idempotent + four-mode byte-identical; docs section live;
    CLAUDE.md/roadmap/goal-perf updated.
  - [ ] No placeholders/TODOs on reachable paths; every in-branch bug fix has a
    failing-test-first regression guard (rule-6 fix included).
- [ ] **Step 4:** Final commit + merge readiness note. Merge `--no-ff` per house cadence after
  owner sign-off.
