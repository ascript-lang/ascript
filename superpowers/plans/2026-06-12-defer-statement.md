# DEFER — `defer` Statement — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task is executed by a **fresh implementer subagent**, then verified by an **independent reviewer subagent** that runs the commands and probes edges (code quality + spec adherence) before acceptance. At the end of each phase, a **holistic per-phase review subagent** reviews the phase's combined changes before the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Ship the `defer` statement — `defer [await] <call>`, reserved keyword, call-only, args evaluated at the defer statement, per-activation LIFO stack, drained on every frame exit (normal/return/`?`-propagate/panic-unwind; NOT on `exit()`, task cancellation, or `gen.close()`), with the §3.6 panic-merge rules and first-class `defer await` — byte-identical across tree-walker, specialized VM, generic VM, and `.aso`, with the FULL grammar tax paid (both parsers, tree-sitter regen+publish+pins, formatter, LSP, REPL, checker lints, fuzzer axis, `.aso` 27→28).

**Architecture:** New `Stmt::Defer { call, awaited, span }` (legacy AST) + `DeferStmt` (CST/ungram) + `defer_statement` (tree-sitter, keyword reserved in all three front-ends). Tree-walker: a `defers` list installed on every activation env by `run_body`/the top-level drivers; drain + the shared `merge_defer_outcome` helper run before the return-contract check. VM: `CallFrame.defers: Vec<DeferEntry>` (heapless when empty), two opcodes `DeferPush`/`DeferPushMethod` (the Method form preserves the schema/shared/workflow call-position hooks), drains at `Op::Return`/`Op::Propagate` + a single unwind chokepoint in `Vm::run` for `Err(Panic|Propagate)` (never `Exit`). Spec: `superpowers/specs/2026-06-12-defer-statement-design.md` — **read it first; every rule, edge, message, and rejected alternative is there.**

**Tech stack:** Rust, single binary `ascript`; `src/{lexer,parser,ast,interp,env,fmt}.rs`, `src/syntax/{lexer,parser,kind}.rs` + `src/syntax/ast/` + `src/syntax/resolve/` + `src/syntax/format/`, `src/compile/mod.rs`, `src/vm/{opcode,chunk,fiber,run,verify,aso,disasm,bcanalysis}.rs`, `tree-sitter-ascript/` (grammar.js + regen `--abi 14`), `src/check/rules/`, `src/check/infer/pass.rs`, `src/lsp/providers/`, `src/fuzzgen/`, `tests/{vm_differential,frontend_conformance,treesitter_conformance,cli}.rs`, `tests/vm_bench.rs` (Gate 12 + `dbg_zero_cost_gate`), `bench/`.

**Binding execution standards (goal.md Gates 1–14 + goal-perf Gates 15–18, production-grade mandate):** TDD per task (failing test → minimal code → green → commit, trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`); any bug found en route — ours or pre-existing — fixed in-branch with a failing-test-first regression guard; clippy clean + tests green in BOTH feature configs at every phase close; the tree-walker is never relaxed; no placeholder/TODO on a reachable path. Branch: `feat/defer-statement` off `main`.

---

## File structure

**New files:**
- `src/check/rules/defer_in_loop.rs`, `src/check/rules/defer_async_call.rs` — the two lints.
- `examples/defer.as`, `examples/advanced/defer_resources.as` — the corpus examples.
- `bench/DEFER_RESULTS.md` — same-session A/B + RSS report.

**Modified files:**
- `src/lexer.rs`, `src/syntax/lexer.rs` — `Tok::Defer` / `SyntaxKind::DeferKw` (reserved).
- `src/parser.rs`, `src/ast.rs`, `src/fmt.rs` — legacy parse + `Stmt::Defer` + Display/fmt arms.
- `src/syntax/kind.rs`, `src/syntax/parser.rs`, `src/syntax/ast/{ascript.ungram,mod.rs}`, `src/syntax/resolve/mod.rs`, `src/syntax/format/mod.rs` — CST surface.
- `tree-sitter-ascript/grammar.js` + regenerated `src/parser.c` + `queries/highlights.scm`.
- `src/env.rs` — the `defers` scope field + accessors.
- `src/interp.rs` — `DeferEntry`, `merge_defer_outcome` (shared SoT), `Stmt::Defer` eval, `run_body` + top-level drains.
- `src/vm/{fiber,opcode,run,verify,aso,disasm,bcanalysis}.rs`, `src/compile/mod.rs` — VM half.
- `src/check/rules/mod.rs`, `src/check/infer/pass.rs` — lint registration + infer walk.
- `src/lsp/providers/{completion,semantic_tokens}.rs` (+ provider tests).
- `src/fuzzgen/mod.rs` — generator axis.
- `editors/zed/extension.toml`, `editors/nvim/lua/ascript/treesitter.lua` (pin bumps at merge wave), `editors/vscode` keyword list if present.
- `tests/{frontend_conformance,treesitter_conformance,vm_differential,cli}.rs`.
- `docs/content/language/{errors,syntax,modules-async}.md`, `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md`, the LSPEC spec inventory note.

---

## Phase 0 — sequencing decision + collision audit + branch

### Task 0.1: record the owner's sequencing decision and open the branch

DEFER touches the same return/unwind paths LANE/CALL/DECODE rework (goal-perf, restated in spec §0). It must land **before LANE starts or after the engine waves merge — never concurrently**.

- [x] **Step 1:** *sequencing decision = **before LANE** (DEFER lands first; SHAPE parallel-allowed), decided by owner in the goal-perf.md 2026-06-12 lock record ("Owner decisions recorded at lock: **DEFER lands first** (before LANE; SHAPE parallel-allowed)") and reaffirmed in the campaign execution brief ("DEFER first").* Recorded 2026-06-12.
- [x] **Step 2:** `git checkout -b feat/defer-statement main` (or rebase target per the decision).
- [x] **Step 3:** Re-run the collision audit and paste the (empty) results into the commit body of the first Phase-1 commit: `grep -rn '\bdefer\b' examples docs/content tests --include='*.as' --include='*.md' --include='*.rs'` and `grep -rn '"defer"' src/stdlib/` — both must show no identifier/export uses (prose comments in `src/` are fine). A hit = a real collision to resolve with the owner BEFORE reserving the keyword.
- [x] **Step 4:** Read the spec end-to-end. Confirm `ASO_FORMAT_VERSION` is still 27 (`src/vm/aso.rs:167`) — if another spec merged a bump, this plan's "28" means "current + 1" everywhere (the cross-spec rule).

---

## Phase 1 — front-end surface: lexers, parsers, AST, tree-sitter (no semantics yet)

> At phase close: all three front-ends accept/reject the same defer forms; `Stmt::Defer` exists with compile-enforced match arms stubbed as loud `unimplemented-feature` Tier-2 errors NOWHERE — instead the interp/compile arms land in this phase as *temporary parse-through* is FORBIDDEN; to keep every commit green, Phase 1 wires the interp/compile arms to a clear `CompileError`/panic `"defer is not yet executable (DEFER Phase 2/3)"` **only on the feature branch**, removed by Phases 2–3 (the branch merges as a whole; no such string may survive to merge — Task 6.3 greps for it).

### Task 1.1: reserve the keyword + legacy parser + AST + Display/fmt arms

**Files:** `src/lexer.rs`, `src/parser.rs`, `src/ast.rs`, `src/fmt.rs`, `src/interp.rs` (arm stub), `tests/frontend_conformance.rs` (legacy half)

- [x] **Step 1: Failing tests first** — in `src/parser.rs`'s test module (the `worker_is_contextual_not_reserved` idiom):

```rust
#[test]
fn defer_is_reserved_and_call_only() {
    // accepted forms
    for src in [
        "fn f() { defer g() }",
        "fn f() { defer obj.close() }",
        "fn f() { defer a?.flush() }",
        "fn f() { defer (cond ? a : b)() }",
        "fn f() { defer (() => { print(1) })() }",
        "fn f() { defer g(...xs) }",
        "fn f() { defer await g() }",
        "defer g()",                      // top level is legal
    ] { assert!(parse_ok(src), "should accept: {src}"); }
    // rejected: non-call
    for src in ["fn f() { defer x }", "fn f() { defer a + b }", "fn f() { defer g }",
                "fn f() { defer g()? }", "fn f() { defer g()! }"] {
        assert_parse_err(src, "defer requires a call");
    }
    // rejected: named args (spec §2.1 v1 Tier-1)
    assert_parse_err("fn f() { defer g(x: 1) }", "defer does not support named-argument calls");
    // reserved keyword
    assert_parse_err("let defer = 5", "");
    assert_parse_err("fn defer() {}", "");
}
```

- [x] **Step 2:** Run — expect FAIL (no `Tok::Defer`).
- [x] **Step 3: Implement.** `Tok::Defer` in the keyword table beside `"interface"` (`src/lexer.rs:601`). `statement()` (`src/parser.rs:96`) gains:

```rust
Tok::Defer => {
    let kw_span = self.span();
    self.advance();
    let awaited = if *self.peek() == Tok::Await { self.advance(); true } else { false };
    let call = self.expr()?;
    let span = kw_span.to(call.span);
    match &call.kind {
        ExprKind::Call { args, .. } => {
            if args.iter().any(|a| matches!(a, crate::ast::CallArg::Named { .. })) {
                return Err(AsError::at(
                    "defer does not support named-argument calls — bind the value first or use an arrow",
                    span));
            }
            Ok(Stmt::Defer { call, awaited, span })
        }
        _ => Err(AsError::at(
            "defer requires a call — only a call expression can be deferred (write `defer (() => …)()` for inline cleanup)",
            span)),
    }
}
```

  `Stmt::Defer { call: Expr, awaited: bool, span: Span }` in `src/ast.rs:285`; `Display` arm (`(defer …)` / `(defer-await …)` s-expr); `src/fmt.rs` statement arm (`defer `/`defer await ` + the expression writer). The interp `exec` arm: temporary branch-local error per the phase note. NOTE: `await` here is the STATEMENT form — the parsed `call` is the bare call (the parser consumed `await` itself; do NOT wrap in `ExprKind::Await`).
- [x] **Step 4:** Run — expect PASS. Full `cargo test --lib` green (legacy-parser suites).
- [x] **Step 5: Commit** — `git commit -m "feat(parser): reserve 'defer'; legacy parse of defer [await] <call> + Stmt::Defer" -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"` (paste the Task 0.1 grep audit in the body).

### Task 1.2: CST front-end — DeferKw/DeferStmt, typed AST, resolver, infer walk

**Files:** `src/syntax/{lexer,kind,parser}.rs`, `src/syntax/ast/{ascript.ungram,mod.rs}`, `src/syntax/resolve/mod.rs`, `src/check/infer/pass.rs`, `src/compile/mod.rs` (arm stub), `tests/frontend_conformance.rs`

- [x] **Step 1: Failing tests** — extend the `both_accept`/both-reject catalog (`tests/frontend_conformance.rs:43` idiom) with EVERY Task-1.1 form: each accepted form `both_accept`, each rejected form rejected **by both front-ends** (assert legacy AND CST reject — the `;`-in-class lesson: the catalog must be both-sided). Include `let defer = 5` both-reject.
- [x] **Step 2:** Run — expect FAIL (CST side).
- [x] **Step 3: Implement.** `SyntaxKind::DeferKw` (keyword table `src/syntax/lexer.rs:416`) + `SyntaxKind::DeferStmt` (`kind.rs`, registered beside `WhileStmt`); `stmt()` arm (`src/syntax/parser.rs:268`): `DeferKw => defer_stmt(p)` — bump kw, optional `AwaitKw` bump, parse expression, complete `DeferStmt`; the call-shape + named-arg validation lives in the CST→checks layer exactly where other structural validations live (match the legacy messages verbatim; the ±1-column caret tolerance applies). `ascript.ungram`: `DeferStmt = 'defer' 'await'? Expr` + the typed-AST node in `src/syntax/ast/mod.rs` (`call()` accessor, `awaited()` flag via token probe). Resolver (`src/syntax/resolve/mod.rs:648` region): walk `DeferStmt` like `ExprStmt` (resolve the inner expression; no bindings). Infer pass (`src/check/infer/pass.rs`): statement walk arm = `synth` the call expression. Compiler `compile_stmt` (`src/compile/mod.rs:1783`): temporary branch-local `CompileError` per the phase note. `compile/mod.rs` helpers that enumerate `Stmt::` (`top_level_bound_names:841`, the stmt-syntax table `:1199`) gain arms (Defer binds nothing).
- [x] **Step 4:** Run conformance — PASS. `cargo test --test frontend_conformance` green.
- [x] **Step 5: Independent review checkpoint** — reviewer probes: `defer await await f()` (reject — inner expr is `Await`, not a call), `defer (f)()` (accept), `defer f ()` with newline between (per existing newline rules — document observed behavior in the test), REPL `is_incomplete` unaffected (`defer` alone on a line = parse error, not continuation).
- [x] **Step 6: Commit** — `feat(cst): DeferKw/DeferStmt — parser, ungram AST, resolver, infer walk`.

### Task 1.3: tree-sitter grammar + regen + conformance + queries

**Files:** `tree-sitter-ascript/grammar.js`, regenerated `tree-sitter-ascript/src/parser.c`, `tree-sitter-ascript/queries/highlights.scm`, `tests/treesitter_conformance.rs`

- [x] **Step 1: Failing test** — add the defer catalog to `treesitter_conformance` (accepted forms parse with zero ERROR nodes and a `defer_statement` node with a `call` field; rejected forms produce ERROR or fail the structural assertion the harness uses; `let defer = 5` errors — keyword extraction reserves it).
- [x] **Step 2:** Implement in `grammar.js`: `defer_statement` per spec §2.4 (`seq('defer', optional('await'), field('call', $.call_expression), optional(';'))`), added to `_statement` (`grammar.js:173`). If `tree-sitter generate --abi 14` reports a conflict with `await_expression`, declare the GLR conflict (the `?`/ternary precedent) — never weaken the hand parsers to match. **Regenerate:** `cd tree-sitter-ascript && tree-sitter generate --abi 14`; commit the regenerated `src/parser.c`.
- [x] **Step 3:** `queries/highlights.scm:21` keyword list gains `"defer"` (and verify the statement's `"await"` is captured — the existing `await` capture may already match the literal). Spot-check `indents.scm`/`folds.scm` need nothing (statement-level).
- [x] **Step 4:** `cargo test --test treesitter_conformance` green (build.rs picks up the new parser.c); `cargo test --test frontend_conformance` still green.
- [x] **Step 5: Commit** — `feat(grammar): tree-sitter defer_statement + regen --abi 14 + highlights`. (Publish + editor pins happen at the merge wave — Task 5.3.)

### Task 1.4: Phase 1 holistic review

- [x] **Step 1:** Holistic subagent: all three front-ends agree on the FULL accept/reject catalog; the reserved keyword errors are clear; no front-end accepts named-arg or non-call defers; both feature configs build + clippy clean; the temporary not-yet-executable markers are present in exactly two places (interp exec arm, compile_stmt arm) and tracked.

---

## Phase 2 — tree-walker semantics (the oracle first)

> Tests in this phase run on the tree-walker via the existing single-engine entry (`run_source` with `ASCRIPT_ENGINE=tree-walker` / the interp-direct test helpers). Phase 3 upgrades every battery to four-mode.

### Task 2.1: DeferEntry + the activation defer list + push semantics

**Files:** `src/interp.rs`, `src/env.rs`
**Tests:** inline `#[tokio::test]`s in `src/interp.rs` (the existing eval-test idiom)

- [x] **Step 1: Failing tests** — evaluation timing + LIFO + scoping:

```rust
// (sketch — use the crate's run_source_capture helper; assert captured output)
defer_args_evaluated_at_statement(): "fn f() { let x = 1; defer print(x); x = 2; print(\"body\") } f()" → "body\n1\n"
defer_lifo(): three defers print 3\n2\n1 after body
defer_is_function_scoped_not_block(): defer inside `if {}` runs at fn exit
defer_in_loop_accumulates(): loop of 3 → three entries, LIFO at fn exit
defer_closure_sees_mutation(): "let x = 1; defer (() => print(x))(); x = 2" in fn → prints 2 (capture-by-cell)
defer_optchain_nil_receiver_no_entry_no_arg_eval(): `defer a?.m(sideEffect())` with a=nil → side effect NOT printed
defer_member_receiver_evaluated_at_statement(): `let o = A(); defer o.show(); o = B()` → A's show runs
defer_spread_materialized(): `defer f(...xs); xs.push(9)` → f sees the snapshot
defer_top_level_runs_at_program_end(): top-level defer prints after last top-level stmt
```

- [x] **Step 2:** Run — FAIL (the Phase-1 stub error).
- [x] **Step 3: Implement.**
  - `src/interp.rs` (beside `Control`): the shared types — these exact shapes are consumed by the VM in Phase 3:

```rust
/// One registered deferred call (spec §3.1). Engine-shared semantics; each engine
/// stores these in its own activation (tree-walker: env defer list; VM: CallFrame.defers).
pub(crate) enum DeferKind {
    /// `defer f(…)` — callee evaluated at the defer statement.
    Call { callee: Value },
    /// `defer recv.name(…)` / `recv?.name(…)` — receiver evaluated at the defer
    /// statement; execution re-enters the MEMBER-CALL evaluator so the
    /// schema/shared/workflow call-position hooks fire (spec §3.1).
    Method { recv: Value, name: Rc<str> },
}
pub(crate) struct DeferEntry {
    pub kind: DeferKind,
    pub args: Vec<Value>,   // flat — spread already materialized
    pub awaited: bool,      // `defer await …`
    pub span: Span,         // the defer statement's span (panic anchoring)
}
```

  - `src/env.rs`: `Scope.defers: Option<Rc<RefCell<Vec<DeferEntry>>>>` (`None` in `global()`/`child()`); `Environment::install_defer_scope() -> Rc<RefCell<Vec<DeferEntry>>>` and `Environment::defer_scope(&self) -> Option<Rc<…>>` (walks parents to the nearest `Some`).
  - `run_body` (`interp.rs:5146`): `let defers = call_env.install_defer_scope();` before binding args.
  - `Stmt::Defer` exec arm: match `call.kind` — `Call{callee: box Member{object,name}}` → eval `object`, Method entry; `OptMember` → eval object, if `nil` do nothing (skip arg eval), else Method; other callee → eval callee, Call entry. Then eval args (the existing `CallArg` eval path used by call sites — positional + spread, flat vec). Push. Returns `Flow::Normal`.
  - Top-level drivers: every `exec(program, env)` driver (enumerate: `grep -n '\.exec(' src/interp.rs src/lib.rs src/repl*.rs` — run_file/run_source/run_tests path, REPL submission, module import exec, worker isolate entry if it bypasses run_body) installs a defer scope on the program env and (Task 2.2) drains it.
- [x] **Step 4:** Tests for push-side behavior that don't need drain ordering (the nil-receiver/arg-eval ones) — note most Step-1 tests also need Task 2.2's drain; implement 2.1+2.2 as one TDD arc if cleaner, but commit separately only when both green.
- [x] **Step 5: Commit** — `feat(interp): DeferEntry + activation defer scopes + defer statement evaluation`.

### Task 2.2: draining — every tree-walker exit path + the merge rules

**Files:** `src/interp.rs`
**Tests:** inline + `tests/cli.rs` (program-level exit-code/panic-output cases)

- [x] **Step 1: Failing tests** — the frame-exit matrix (spec §3.3) + merge rules (§3.6) + ordering (§3.7/§3.8):

```text
drain_on_normal_completion / drain_on_return / drain_on_propagate (the `?` runs defers; pair preserved)
drain_on_panic_unwind_innermost_first: nested fns, panic in inner — inner defers before outer
recover_sees_panic_after_defers: recover(f) → defers in f ran; [nil, err] message intact
defer_panic_replaces_return (§3.6 r1): fn returns 1 but defer panics → caller sees panic
defer_panic_supersedes_propagate (§3.6 r2)
defer_panic_during_panic_appends_suppressed_note (§3.6 r3): EXACT message
  "boom (suppressed panic in deferred call: cleanup failed)"
remaining_defers_run_after_defer_panic (§3.6 r4): 3 defers, middle one panics → all 3 side effects observed
defer_runs_before_return_contract_check (§3.7): deferred print precedes the contract panic
return_value_unmodifiable: defer mutates a local the return already read → caller sees original
exit_skips_defers (§3.3): exit(0) after a defer → defer side effect ABSENT (cli.rs, observe stdout)
break_continue_dont_drain
recursion_cap_defer (§3.8): fn exiting at MAX_CALL_DEPTH → deferred call panics "maximum recursion depth exceeded", merged per rules
module_import_defers_run_at_import; repl_submission_defers (REPL session test)
defer_result_discarded: deferred call returning [nil, "e"] → no panic, no output change
schema_method_defer_hook_preserved: defer s.parse(x) on a schema → call_schema path observed (compare against direct call)
frozen_instance_method_defer: distinct "not available on a frozen instance" diagnostic preserved
```

- [x] **Step 2:** Run — FAIL.
- [x] **Step 3: Implement.**
  - The shared merge helper (single SoT, spec §3.6 — VM reuses it verbatim in Phase 3):

```rust
/// Fold a panic raised by a deferred call into the in-flight frame outcome.
/// Rules (DEFER spec §3.6): Normal/Return → the panic becomes the outcome;
/// Propagate → SUPERSEDED by the panic; Panic → the ORIGINAL wins and the new
/// message is appended as a suppressed note. Exit is handled by the CALLER
/// (drain aborts; exit propagates) and never reaches here.
pub(crate) fn merge_defer_panic(pending: &mut Result<Value, Control>, new: AsError) {
    match pending {
        Ok(_) | Err(Control::Propagate(_)) => *pending = Err(Control::Panic(new)),
        Err(Control::Panic(orig)) => orig.message = format!(
            "{} (suppressed panic in deferred call: {})", orig.message, new.message),
        Err(Control::Exit(_)) => unreachable!("drain never runs under Exit"),
    }
}
```

  - The drain driver (one async fn, used by run_body AND the top-level drivers):

```rust
/// Drain `list` newest-first into `pending` (spec §3.2/§3.5/§3.6). Takes the
/// entries OUT first (idempotent; a panic mid-drain leaves an empty list).
/// An Exit raised by an entry aborts the drain and becomes the outcome.
async fn run_defers(&self, list: &Rc<RefCell<Vec<DeferEntry>>>,
                    pending: &mut Result<Value, Control>, env: &Environment) { … }
```

    Per entry: Method → the member-call evaluator path (the SAME function the `Call`-with-`Member`-callee evaluator uses — factor it if needed so the schema/shared/workflow hooks are structurally shared, not copied); Call → `call_value`. Result: `Value::Future` + `awaited` → `f.get().await` (its Err merges); `Value::Future` + bare → `merge_defer_panic` with the §3.4 message `deferred call returned a future that would be cancelled on drop — use 'defer await f()' or do async cleanup before exit` anchored at `entry.span`; other results discarded. `Err(Control::Exit)` from an entry → `*pending = Err(exit)` and `return` (remaining entries skipped, spec §3.6 r5).
  - `run_body`: restructure the outcome match into `let mut pending: Result<Value, Control> = …` (Exit short-circuits BEFORE drain), `self.run_defers(&defers, &mut pending, call_env).await`, then the return-contract check applies only to `Ok` — keeping the existing Propagate-pair contract behavior (a Propagate that SURVIVED the drain converts to the pair result exactly as today). The `_depth` guard is still alive across the drain (§3.8) — verify by reading the drop order.
  - Top-level drivers: drain after the body with the same helper; `Propagate => Ok` conversion stays AFTER the drain.
- [x] **Step 4:** Run — PASS. Full `cargo test` (default config) green — the entire existing corpus must be untouched (no defers = empty lists = no behavior change).
- [x] **Step 5: Independent review checkpoint** — reviewer probes: defer inside `init` with a failing field contract; defer in a method calling `super`; double-`recover` nesting; a deferred call that itself defers (inner activation drains first); `exit()` raised INSIDE a deferred call (terminates, skips remaining — cli.rs observation); borrow-across-await audit of the new drain (`cargo clippy` + manual read).
- [x] **Step 6: Commit** — `feat(interp): defer draining on every frame exit + the §3.6 merge rules (oracle complete)`.

### Task 2.3: tree-walker `defer await` + async/generator/cancellation semantics

**Files:** `src/interp.rs`, `src/coro.rs` (tests only — no behavior change)
**Tests:** inline async tests

- [x] **Step 1: Failing tests:**

```text
defer_await_happy_path: async cleanup fn; defer await teardown(); body returns → teardown completed before caller's await resolves
defer_await_lifo_mixed: [sync defer, await defer, sync defer] → strict LIFO, the await completes before the older sync defer runs
defer_await_during_propagate_unwind: `?` fires; defer await runs; pair delivered after
defer_await_during_panic_to_recover: recover sees the panic only after the awaited defer completed
bare_future_defer_panics_with_exact_message (§3.4)
defer_await_on_sync_call_is_identity: defer await syncFn() — no error
async_fn_defers_across_awaits: defer before first await; exit after third → runs
generator_completion_runs_defers: fn* body with defer; drive to done → defer ran before done reported
generator_panic_runs_defers
generator_close_does_NOT_run_defers (spec §4.3): close() mid-suspend → side effect absent; same for last-drop
task_cancellation_does_NOT_run_defers (spec §4.2): spawn an async fn parked on a channel with a defer; drop the handle (race loser); deterministic post-check shows the defer never ran
defer_await_cancelled_mid_drain: cancellation while a deferred await is suspended → older defers don't run (documented rule) — use a controllable future
```

- [x] **Step 2:** Implement: this is mostly already done by Task 2.2's `awaited` handling — this task PROVES the async matrix and fixes whatever it flushes out (e.g. the drain helper must not hold the `RefCell` list borrow across the per-entry await — entries were taken out, verify).
- [x] **Step 3:** Run — PASS. **Commit** — `test(interp): defer await + async/generator/cancellation matrix (tree-walker)`.

### Task 2.4: Phase 2 holistic review

- [x] **Step 1:** Holistic subagent over the tree-walker semantics: every spec §3/§4 rule has a named test; the merge helper is the only place outcome-folding happens; the member-call hook path is structurally shared (grep for a second schema-hook check — must not exist); no `RefCell` borrow across await in any new code; full suite + clippy BOTH configs green; the existing corpus byte-identical (run the tree-walker side of `vm_differential` — VM still errors on defer, expected until Phase 3, so restrict to non-defer corpus = the whole existing corpus).

---

## Phase 3 — VM: opcodes, frame state, unwind chokepoint, `.aso`, four-mode proof

### Task 3.1: opcodes + compiler + frame defers + the Return/Propagate drains

**Files:** `src/vm/{opcode,fiber,run}.rs`, `src/compile/mod.rs`
**Tests:** hand-built-chunk unit tests (the `run.rs` idiom) + `tests/vm_differential.rs` (first four-mode battery)

- [x] **Step 1: Failing tests** — (a) opcode round-trip/width units in `opcode.rs` (`DeferPush` width 2, `DeferPushMethod` width 4, dense discriminants test extended); (b) a four-mode `assert_three_way_matches` battery porting Task 2.1/2.2's CORE cases (timing, LIFO, scoping, return/propagate drains, §3.6 r1/r2, §3.7 ordering, top-level).
- [x] **Step 2:** Implement:
  - `Op::DeferPush` (flags u8: bit0 awaited, bit1 spread; argc u8) and `Op::DeferPushMethod` (name u16 const, flags u8, argc u8) appended after `Op::Break`; `operand_width` arms; `ALL` list.
  - `CallFrame.defers: Vec<DeferEntry>` (`src/vm/fiber.rs:20`; both construction sites init `Vec::new()`).
  - `compile_stmt` `DeferStmt` arm (spec §5.2): member callee → receiver, args, `DeferPushMethod` (OptMember: dup/nil-test jump that pops and skips — mirror the existing optional-chain lowering and pin a bytecode-shape unit test); other → callee value, args, `DeferPush`; spread → the existing array-builder + bit1; named → unreachable (parser rejects; keep a defensive `CompileError`).
  - `run_loop` arms: `DeferPush`/`DeferPushMethod` pop into a `DeferEntry` (span = op span) and append to `fiber.frame_mut().defers`.
  - `Op::Return` (`run.rs:3345`) + `Op::Propagate` err path (`run.rs:3359`): `if !fiber.frame().defers.is_empty()` → `let list = mem::take(&mut fiber.frame_mut().defers);` → `self.run_frame_defers(list, &mut pending).await` (the VM drain: Method → the generic member-call routine — the hook chokepoint; Call → `call_value`; `awaited`/bare-future per §3.4; merge via the SHARED `interp::merge_defer_panic`) → on surviving value, `return_from_frame`; on panic, `return Err(...)` (the chokepoint will see this frame's list empty).
- [x] **Step 3:** Four-mode battery green in BOTH feature configs; the hand-chunk tests green; `--no-specialize` runs identical (defer has no specialized path — assert by running the battery generic).
- [x] **Step 4: Commit** — `feat(vm): DeferPush/DeferPushMethod + frame defer stack + Return/Propagate drains`.

### Task 3.2: the unwind chokepoint + async/generator parity

**Files:** `src/vm/run.rs`
**Tests:** `tests/vm_differential.rs`

- [x] **Step 1: Failing tests** — four-mode port of the panic-path matrix: panic unwind innermost-first across nested frames; recover-after-defers; §3.6 r3 exact suppressed-note message; r4 remaining-defers; deep-recursion cap; `exit()` skip; generator completion/panic/close()/drop; cancellation; defer await full matrix (happy/propagate/panic-to-recover/mixed-LIFO/bare-future message).
- [x] **Step 2:** Implement in `Vm::run` (`run.rs:1057`, beside the SP4 span-source binder): on `Err(Control::Panic(_) | Control::Propagate(_))` from `run_loop` — never `Exit` — if any live frame has defers, run the chokepoint drain: walk `fiber.frames` top-down, `mem::take` each frame's list, drain LIFO into the pending control (shared merge), then return the final control. Frames are NOT popped (unchanged abandonment semantics); document why double-drain is impossible (taken lists). Mind: the drain awaits (re-entrant `self.run` on fresh fibers; `run` is already async) — no fiber borrow held across it (`fiber` is `&mut`, fine).
- [x] **Step 3:** All Phase-2 tree-walker tests get four-mode twins; run the FULL `cargo test --test vm_differential` both configs — the entire existing corpus + the defer batteries byte-identical. Any divergence: fix the ENGINE (usually drain placement vs the contract check, or depth accounting), never the assertion.
- [x] **Step 4: Independent review checkpoint** — reviewer hand-probes: a panic raised INSIDE `return_from_frame`'s contract check after a successful drain (caller defers must still run via the chokepoint; this frame's list empty); a defer registered in a generator that is resumed across `task.spawn` boundaries; `Op::Break` (DBG breakpoint) inside a deferred body; profiler attribution sanity (`--profile cpu` on a defer-heavy script doesn't crash and attributes to the deferred fn).
- [x] **Step 5: Commit** — `feat(vm): unwind chokepoint drain — defers on panic/propagate unwind, byte-identical four-mode`.

### Task 3.3: `.aso` 27→28, verifier, disasm, bcanalysis, negative space

**Files:** `src/vm/{aso,verify,disasm,bcanalysis}.rs`, `tests/` (aso/verify suites)

- [x] **Step 1: Failing tests:** verifier accepts a compiled defer program; rejects hand-built bad forms — `DeferPushMethod` name idx out of range / non-string const (structured error, the `BadInterface` precedent), nonzero undefined flag bits (both ops), `DeferPush` with insufficient stack depth; `ASO_FORMAT_VERSION == 28` asserted; an `.aso` built pre-bump rejected with the clean version error; round-trip: `build` then `run file.aso` joins the four-mode battery (it already does via the standing pipeline — verify defer examples flow through).
- [x] **Step 2:** Implement: bump `ASO_FORMAT_VERSION` to 28 (read-the-constant rule); `stack_effect` (`verify.rs:217`): `DeferPush` pops `argc+1` (bit1 → 2), pushes 0; `DeferPushMethod` pops `argc+1` (bit1 → 2), pushes 0; operand validation incl. flags-bit whitelist; `disasm.rs` renders both (decoded flags + name); `bcanalysis.rs` decode table arms. Regenerate the fuzz `.aso` corpus if the version gate invalidates it (`fuzz/regenerate_aso_corpus.sh`).
- [x] **Step 3:** Green; **Commit** — `feat(aso): ASO 28 — defer opcodes verified, disassembled, analyzed`.

### Task 3.4: coverage counters + Phase 3 holistic review

- [x] **Step 1:** `#[cfg(feature = "fuzzgen")]`-gated counters on `Vm` + the interp (entries pushed / drained / chokepoint drains taken) + the corpus assertion test: after the differential corpus runs, all nonzero (anti-false-green, Gate 15).
- [x] **Step 2:** Holistic subagent: the two temporary Phase-1 stub strings are GONE (grep `not yet executable` → zero); the merge helper has exactly ONE definition with both engines calling it; every §3.3 matrix row has a four-mode test; full suite + clippy both configs; `vm_differential` full corpus green both configs.
- [x] **Step 3: Commit** — `test(vm): defer coverage counters + corpus assertion`.

---

## Phase 4 — checker, formatter, LSP, REPL, fuzzer

### Task 4.1: lints — `defer-in-loop` + `defer-async-call`

**Files:** `src/check/rules/{defer_in_loop,defer_async_call}.rs`, `src/check/rules/mod.rs`, `tests/check.rs`

- [ ] **Step 1: Failing tests:** `defer-in-loop` fires inside `while`/`for` (incl. for-range/for-of/for-await), does NOT fire for a defer inside a nested `fn`/arrow within the loop, does not fire outside loops; `defer-async-call` fires on bare `defer asyncDecl()` (same-file `async fn` decl), does NOT fire on `defer await asyncDecl()`, member callees, imported names, or dynamic callees; both Warning; both suppressible via `ascript.toml [lint]`; **Gate 5:** zero hits on `examples/**` in both configs after Phase 5's examples land (the intro example structures its loop demo inside a wrapper fn or carries the documented suppression — decide in Task 5.1, never weaken the gate).
- [ ] **Step 2:** Implement (the `range_step.rs` walking idiom; messages verbatim from spec §6.1/§6.2); register both in `ALL` (`rules/mod.rs:31`); add the lint docs lines wherever lints are enumerated (check `docs/content` lint listing and `src/check/config.rs` name registry — follow the `range-step` registration trail end-to-end).
- [ ] **Step 3:** Green both configs; **Commit** — `feat(check): defer-in-loop + defer-async-call lints (Warning)`.

### Task 4.2: formatter (CST + legacy) + idempotence

**Files:** `src/syntax/format/mod.rs`, `src/fmt.rs` (arm landed in 1.1 — verify), `tests` (fmt suites)

- [ ] **Step 1: Failing tests:** `fmt` canonicalizes `defer   f( 1,2 )` → `defer f(1, 2)`; `defer  await  f()` → `defer await f()`; idempotent on both examples (Task 5.1) and on a comment-attached defer (`// note` above a defer survives — the IFACE comment-attachment lesson: test leading comments explicitly).
- [ ] **Step 2:** Implement the `DeferStmt` arm in `src/syntax/format/mod.rs` (beside `ReturnStmt:166`): `defer` + space + optional `await` + space + the expression renderer. Run the formatter over the whole `examples/**` corpus — zero diffs on non-defer files.
- [ ] **Step 3:** Green; **Commit** — `feat(fmt): canonical defer rendering (CST formatter) + idempotence`.

### Task 4.3: LSP + REPL

**Files:** `src/lsp/providers/{completion,semantic_tokens}.rs`, LSP/REPL tests

- [ ] **Step 1: Failing tests:** completion offers `defer` keyword + the snippet in statement position; semantic tokens classify the `defer` keyword token as keyword (provider test on a defer-containing file — it is a real reserved token, so this pins the default classification rather than adding a remap, spec §7); REPL session test: top-level defer runs at submission end; a REPL-defined fn with a defer behaves on a later line.
- [ ] **Step 2:** Implement: `"defer"` in the keyword list (`completion.rs:28`) + snippet (`completion.rs:174` table). Check `editors/vscode` for a TextMate keyword enumeration — update if present (record either way in the commit body).
- [ ] **Step 3:** Green; **Commit** — `feat(lsp,repl): defer keyword completion/snippet + semantic token + REPL pins`.

### Task 4.4: fuzzer axis (Gate 15, same PR)

**Files:** `src/fuzzgen/mod.rs`, `tests/property.rs` (if the seed-battery lives there)

- [ ] **Step 1:** Teach `stmt()` to emit, weighted: bare `defer declared_fn(args…)` (printing fns so order bites), `defer (() => …)()` touching a mutable local, defer inside generated loops and nested fns, `defer await generated_async_fn()`, and defers in bodies that `?`-propagate via `rerr`. Keep generated programs deterministic (no clock/rng — the standing fuzzgen rule).
- [ ] **Step 2:** Extend the multi-seed differential stress test; assert via the Task-3.4 counters that a 200-seed batch pushes AND drains defers (anti-false-green). Smoke campaign if cargo-fuzz available: `cargo +nightly fuzz run differential -- -runs=50000` → zero divergences.
- [ ] **Step 3:** Green; **Commit** — `test(fuzz): defer axis in the grammar-aware generator + coverage assertion`.

### Task 4.5: Phase 4 holistic review

- [ ] **Step 1:** Holistic subagent: Gate 11 evidence per tool (conformance tests, fmt idempotence run, LSP provider tests, REPL session test — run them, paste outputs); lints registered exactly once; fuzzgen inspection of 20 generated programs shows defers in varied positions; both configs green + clippy clean.

---

## Phase 5 — examples, docs, grammar publish

### Task 5.1: examples (intro + advanced)

**Files:** `examples/defer.as`, `examples/advanced/defer_resources.as`

- [ ] **Step 1:** `examples/defer.as` — intro, printing deterministically: basic close pattern, LIFO, args-at-statement timing, `?`-interplay (a propagating fn whose defers run), `defer await` of an async cleanup, the function-scoped rule, and the loop-accumulation demo structured to keep Gate 5 at zero (wrapper fn per spec discussion — decide here, record in the file's comments).
- [ ] **Step 2:** `examples/advanced/defer_resources.as` — production-shaped, fully error-handled: multi-resource acquire-with-defer (real `std/fs` temp files so it runs everywhere), panic-unwind observed through `recover` including the §3.6 suppressed-note message, the generator-owner pattern (§4.3), `exit`-free, deterministic output → joins the corpus (NOT in `EXAMPLE_SKIPS`).
- [ ] **Step 3:** Verify four ways: `target/release/ascript run`, `run --tree-walker`, generic (via differential), `build` + `run .aso`; `ascript fmt` idempotent; `ascript check` clean (Gate 5).
- [ ] **Step 4: Commit** — `examples(defer): intro + production-shaped resource cleanup corpus`.

### Task 5.2: docs + CLAUDE.md + LSPEC note

**Files:** `docs/content/language/{errors,syntax,modules-async}.md`, `CLAUDE.md`, `superpowers/roadmap.md`, `superpowers/specs/2026-06-12-language-spec-stability-design.md` (inventory note)

- [ ] **Step 1:** Write per spec §10: errors.md primary section ("Cleanup with `defer`": form, call-only, timing, LIFO, the frame-exit matrix table, merge rules in user terms, `defer await` + the bare-future error, the cancellation/`gen.close()`/`exit()` non-runs stated LOUDLY with the native-`Drop` guidance); syntax.md statement-list line + link; modules-async.md async/generator/cancellation rules. No new page → no `NAV` change. Serve the site and sanity-check rendering + in-content links.
- [ ] **Step 2:** `CLAUDE.md` gotcha bullet (spec §10 contents); roadmap entry; goal-perf status at merge; one-line LSPEC coordination note (the semantics chapter must absorb §3's matrix).
- [ ] **Step 3: Commit** — `docs(defer): errors-page section, async rules, CLAUDE.md, LSPEC note`.

### Task 5.3: grammar publish + editor pins (merge-wave step)

- [ ] **Step 1:** `./scripts/sync-grammar.sh` (subtree-split + mirror push; prints the new SHA). If sandbox-gated, note that CI `mirror-grammar.yml` publishes deterministically on origin push (the ADT precedent) — but the PIN BUMP is manual either way.
- [ ] **Step 2:** Bump `editors/zed/extension.toml` (`commit`) and `editors/nvim/lua/ascript/treesitter.lua` (`revision`) to the split SHA. One publish per merge wave — coordinate if another grammar-touching branch is in flight (none expected; DEFER is the campaign's only grammar change).
- [ ] **Step 3: Commit** — `chore(grammar): publish tree-sitter mirror + bump zed/nvim pins`.

---

## Phase 6 — performance gates + full matrix + holistic (Definition of Done)

### Task 6.1: Gate 12/16/17/18 — zero regression for defer-free code

**Files:** `bench/DEFER_RESULTS.md`, `tests/vm_bench.rs` (standing gates)

- [ ] **Step 1: Same-session A/B (Gate 16):** one session, one machine — `main` vs `feat/defer-statement`, the standing bench corpus (+ call-heavy workloads if LANE Task 0 merged), 5× medians, shipped profiler attribution. **Expectation stated, result measured:** defer-free delta ≈ 0 (the empty-`Vec` check + 24B frame growth must be noise); any measurable regression is a bug — fix the check placement, don't accept.
- [ ] **Step 2:** Gate 17: `cargo test --test vm_bench` — spec/tw geomean ≥2× holds; the dispatch loop was touched → re-run `dbg_zero_cost_gate` (instrument==None ≈ armed-idle); record both numbers.
- [ ] **Step 3:** Gate 18: peak RSS per workload (`/usr/bin/time -l`) before/after; a defer-HEAVY microbench (10k-iteration defer-in-loop) reported honestly (linear growth by design — the lint is the guard).
- [ ] **Step 4:** Write `bench/DEFER_RESULTS.md` (numbers, machine, commits, the no-kill-switch rationale referenced from spec §5.5). **Commit.**

### Task 6.2: full matrix

- [ ] **Step 1:** `cargo test` — all binaries green, 0 failures.
- [ ] **Step 2:** `cargo test --no-default-features` — green.
- [ ] **Step 3:** `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` — clean.
- [ ] **Step 4:** `cargo test --test vm_differential` both configs — full corpus + goldens + every defer battery, four-mode byte-identical; the Task-3.4 coverage assertion nonzero.
- [ ] **Step 5:** Conformance (frontend + treesitter), fmt idempotence, LSP suites, REPL pins, fuzz smoke — green; `cargo test --test cli` (exit-skip + panic-output cases) green.

### Task 6.3: whole-effort holistic review + merge

- [ ] **Step 1:** Holistic-review subagent over the ENTIRE branch diff against the spec: a §-by-§ coverage table; zero TODO/placeholder (grep `not yet executable`, `TODO`, `unimplemented` in the diff → zero); every brief-mandated test present (defer-await ×4, bare-future message, LIFO mixed, capture interplay, rest/spread args, methods/init, recover combinations, loops, nested fns); the tree-walker remains the oracle (no relaxed assertion anywhere); invariants intact (`Value: !Send`, no borrow across await, native handles untraced, GC untouched).
- [ ] **Step 2:** Every checkbox in this plan ticked; the Phase-0 sequencing line filled in.
- [ ] **Step 3:** Merge `feat/defer-statement` → `main` with `--no-ff`; update `goal-perf.md` status (DEFER → ✅) in the merge commit.

---

## Self-review (author pass)

- **Spec coverage:** §2 surface → Phase 1 (all three front-ends + catalogs); §3.1–3.3 → 2.1/2.2 + 3.1/3.2 four-mode; §3.4 defer await (owner amendment) → 2.3 + 3.2; §3.5 stash → structural (locals across await) with tests in 2.3/3.2; §3.6 merge → the ONE shared helper + exact-message tests; §3.7/§3.8 ordering/depth → named tests; §4 async/cancel/gen/workers → 2.3 + 3.2 (workers implicitly — isolate engines are these engines; the pooled-isolate drain-at-exit fact is asserted by the worker corpus staying green); §5.4 aso/verify/disasm → 3.3; §5.5 no-kill-switch → bench report note; §6 lints → 4.1; §7 tooling → 4.2/4.3 + 1.3; §8 gates → 3.4/4.4/6.2; §9 perf → 6.1; §10 docs → 5.1/5.2; §11 rejections respected (no block-scope, no auto-await, no close-resume — nothing in the plan builds them).
- **Oracle-first ordering:** tree-walker semantics complete and reviewed (Phase 2) before any VM code — the four-mode differential then proves the VM against it, never the reverse.
- **No placeholders:** the shared `DeferEntry`/`DeferKind`/`merge_defer_panic` shapes are written once in Task 2.1/2.2 and consumed by name in 3.1/3.2; the two branch-local Phase-1 stubs are tracked and grep-killed in 3.4/6.3.
- **The risky parts get the reviewers:** the unwind chokepoint (3.2 review checkpoint probes contract-panic-after-drain and DBG interplay), the hook-preserving Method entry (2.2 schema/frozen probes), and the reservation (1.2 reviewer probes the ambiguity corners the spec used to justify reserving).
