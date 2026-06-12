# Call-Path Allocation Diet + Callback Trampoline (CALL) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. At the end of each
> phase, a **holistic per-phase review subagent** reviews the phase's combined changes before
> the next phase starts. A task/phase is closed only when every box under it is ticked.

**Goal:** Remove the ≥3 heap allocations per VM call (empty-cells fast path, in-place
operand-stack argument binding, fiber pooling) and replace the per-element
fiber+boxed-future+Vec ceremony in higher-order stdlib builtins with ONE reused fiber driven on
LANE's sync lane, with per-element escalation — all behind a single permanent `call_fast` kill
switch, byte-identical across all engine modes, measured per goal-perf Gates 15–18.

**Spec:** `superpowers/specs/2026-06-12-call-path-diet-design.md` (CALL). Read it first; section
references (§) below are into it.

**Architecture:** Unit A reshapes allocation on the existing call path (`src/vm/fiber.rs`,
`src/vm/run.rs`, a borrowing twin of `check_call_args` in `src/interp.rs`). Unit B adds
`src/vm/trampoline.rs` — a `CallbackTrampoline` (one reused fiber + LANE's `run_loop_sync` +
escalation onto the async `run`) and a `CallbackDriver` enum the stdlib iteration sites adopt.
The seam is `Interp::call_value`'s `Value::Closure` arm: trampolines arm only for VM closures,
so the tree-walker is untouched by construction. One flag `Vm.call_fast` gates everything;
`Vm::new_generic` disables it too (the generic floor stays complete).

**Tech stack:** Rust; the async bytecode VM (`src/vm/`); tokio current-thread + `LocalSet`
(`!Send`, never add `Send` bounds); `gcmodule::Cc`; tests via `cargo test` both feature
configs, `tests/vm_differential.rs` four-mode, `fuzz/` differential, `tests/vm_bench.rs`
(Gate 12/17), a new `tests/alloc_count.rs` (Gate 18).

**Hard dependency gate:** LANE merged to the branch base (provides `Vm::run_loop_sync` /
`SyncOutcome::{Finished(RunOutcome), NeedsAsync}` and the functional-idiom bench corpus from
its Task 0).
Phase 0 verifies this; Unit A tasks (Phase 1–2) do not need the sync driver but the branch
still bases on post-LANE `main` to avoid double-churn in `run.rs`.

**Binding execution standards (production-grade mandate):** any bug found while working — ours
or pre-existing, direct or incidental — is fixed in-branch with a failing-test-first regression
guard, never stepped around (goal.md Gate 14). No placeholders, no silent deferrals. Commit per
task with the house trailer:
`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.

---

## File structure

**New files:**
- `src/vm/trampoline.rs` — `CallbackTrampoline` + `CallbackDriver` (Unit B).
- `tests/alloc_count.rs` — counting-allocator slope probes (Gate 18 tripwire).
- `tests/call_fast.rs` — kill-switch parity batteries, reset-invariant tests, coverage
  (anti-false-green) assertions.
- `examples/functional_pipelines.as` — intro functional-idiom corpus example.
- `examples/advanced/callback_escalation.as` — escalation/edge corpus example (async-awaiting
  callbacks, panicking callbacks + `recover`, `?`-propagation out of a builtin, captured cells).
- `bench/run_call_bench.sh` + `bench/CALL_RESULTS.md` — the same-session A/B + RSS/alloc report.

**Modified files:**
- `src/vm/fiber.rs` — `alloc_cells` empty fast path; `Fiber::reset`.
- `src/vm/run.rs` — `Vm.call_fast` + `fiber_pool` + `call_fast_stats`; `Op::Call` in-place
  binding; pooled take/return in `call_value` / `invoke_compiled_method` /
  `invoke_compiled_static`; `cells.get`-based binding loops.
- `src/interp.rs` — `check_call_args` refactor onto shared cores + `check_call_args_in_place`.
- `src/stdlib/array.rs`, `src/stdlib/object.rs`, `src/stdlib/stream.rs` — `CallbackDriver`
  adoption at the §5.5 iteration sites.
- `src/lib.rs` — `vm_run_source_no_call_fast` + `call_fast` threading in `vm_run_source_cfg` +
  the `ASCRIPT_NO_CALL_FAST` env seam (read at `Vm` construction in the `run` path).
- `tests/vm_differential.rs` — fourth mode; `tests/property.rs` + `fuzz/fuzz_targets/
  differential.rs` — fourth axis; `tests/vm_bench.rs` — re-run gates (no structural change).
- `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md` (status flip) — final task.

---

## Phase 0 — Preflight: dependency gate + BEFORE measurements

### Task 0.1: verify LANE and record the baseline

**Files:** create `bench/CALL_RESULTS.md` (baseline section), `bench/run_call_bench.sh`.

- [ ] **Step 1:** Verify LANE is merged at the branch base: `grep -n "fn run_loop_sync"
  src/vm/run.rs`
  and `grep -rn "NeedsAsync" src/vm/` succeed; `ls bench/profiling/ | grep -i
  "callback\|functional\|pipeline"` shows LANE Task 0's functional-idiom workloads. **If any of
  these fail, STOP — this plan is blocked on LANE; escalate to the owner.** Confirm the LANE
  symbol names as shipped (`Vm::run_loop_sync`, `SyncOutcome::{Finished(RunOutcome),
  NeedsAsync}` — the names this plan uses throughout).
- [ ] **Step 2:** Write `bench/run_call_bench.sh` (mirrors `bench/run_shared_heap_bench.sh`):
  builds `--release`, runs the functional-idiom + object_churn workloads under
  `/usr/bin/time -l` capturing wall time + `maximum resident set size`, prints a table. Run it
  on the UNMODIFIED branch base and paste the output into `bench/CALL_RESULTS.md` under
  `## Baseline (pre-CALL, same session)`. Also record `ascript run --profile cpu` output paths
  for the two headline workloads (the shipped profiler is the instrument, Gate 16).
- [ ] **Step 3:** Commit — `git commit -m "bench(call): baseline harness + pre-CALL numbers"`.

### Task 0.2: Phase 0 review

- [ ] **Step 1:** Independent reviewer confirms: LANE symbols recorded accurately, baseline
  numbers are real captured output (not invented), script is re-runnable.

---

## Phase 1 — The kill switch + differential/fuzz scaffolding (inert)

### Task 1.1: `Vm.call_fast` + stats counters + the no-call-fast mode

**Files:** modify `src/vm/run.rs`, `src/lib.rs`, `tests/vm_differential.rs`,
`tests/property.rs`, `fuzz/fuzz_targets/differential.rs`; create `tests/call_fast.rs`.

- [ ] **Step 1: Write the failing test** — in `tests/call_fast.rs`:

```rust
//! CALL — kill-switch + coverage scaffolding. The `no_call_fast` mode must run the
//! corpus byte-identically; the stats accessor must exist (counters asserted >0 in
//! Phase 3 once the fast paths land).
#[test]
fn no_call_fast_mode_runs_byte_identically() {
    ascript::run_on_worker_stack(|| async {
        let src = r#"
            fn add(a, b) { return a + b }
            let xs = [1, 2, 3]
            print(xs.map((x) => add(x, 10)))
        "#;
        let spec = ascript::vm_run_source(src).await.expect("spec vm");
        let ncf = ascript::vm_run_source_no_call_fast(src).await.expect("no-call-fast vm");
        assert_eq!(spec, ncf);
    });
}
```

  (Check how existing integration tests drive the `!Send` runtime — `tests/vm_differential.rs`
  uses a current-thread runtime helper; copy that idiom exactly rather than inventing
  `run_on_worker_stack` usage if it differs.)
- [ ] **Step 2: Run it — expect FAIL** (compile error: `vm_run_source_no_call_fast` missing).
- [ ] **Step 3: Implement** —
  - `src/vm/run.rs`: beside `specialize` (`run.rs:117`) add:

```rust
    /// **The CALL kill switch (CALL §8.1).** Gates EVERY call-path fast path this
    /// spec adds: the empty-cells return (A1), in-place arg binding (A2), fiber
    /// pooling (A3), and trampoline arming (B). Permanent, mirroring `specialize`;
    /// `Vm::new_generic` sets BOTH false so the generic mode stays the complete
    /// everything-off floor.
    call_fast: bool,
    /// CALL §8.3 coverage counters (anti-false-green): plain `Cell` bumps whose
    /// cost is bounded by the Gate-17 zero-cost bench. Asserted >0 over the
    /// functional corpus so the differential proves the fast paths actually ran.
    call_fast_stats: Cell<CallFastStats>,
```

```rust
#[derive(Clone, Copy, Default, Debug)]
pub struct CallFastStats {
    pub inplace_binds: u64,
    pub pooled_fiber_reuses: u64,
    pub trampoline_calls: u64,
    pub trampoline_escalations: u64,
}
```

    with `#[doc(hidden)] pub fn call_fast_stats(&self) -> CallFastStats` and an internal
    `#[inline] fn bump_stat(&self, f: impl FnOnce(&mut CallFastStats))`. Constructor plumbing:
    `with_specialize(interp, specialize)` becomes a delegator to
    `with_flags(interp, specialize, /*call_fast*/ specialize)` (generic ⇒ both off); add
    `pub fn with_flags(interp, specialize: bool, call_fast: bool) -> Rc<Self>` and keep
    `new`/`new_generic`/`new_with_instrument` signatures unchanged.
  - `src/lib.rs`: thread a `call_fast: bool` parameter through `vm_run_source_cfg`
    (`lib.rs:2269`) — existing wrappers pass `specialize` for it (`vm_run_source` ⇒ true,
    `vm_run_source_generic` ⇒ false); add the env seam `ASCRIPT_NO_CALL_FAST=1` read once at
    `Vm` construction in the `run` path (the LANE `ASCRIPT_NO_SYNC_LANE` precedent, itself
    mirroring `ASCRIPT_NO_SPECIALIZE` at `lib.rs:2067` — kill switches are `ASCRIPT_NO_*`;
    tests use the explicit constructor, never the env); and add:

```rust
/// CALL §8.1: the SPECIALIZED VM with the call-path fast paths disabled
/// (`specialize = true, call_fast = false`) — isolates a CALL divergence from an
/// IC/adaptive one. `#[doc(hidden)]` test API, the fourth differential mode.
#[doc(hidden)]
pub async fn vm_run_source_no_call_fast(src: &str) -> Result<(String, Option<i32>), AsError> {
    vm_run_source_cfg(src, true, false, false, false).await
}
```

- [ ] **Step 4: Run it — expect PASS.**
- [ ] **Step 5: Wire the fourth mode/axis the same PR (Gate 15):**
  - `tests/vm_differential.rs`: in the corpus-runner helper that already runs tw/spec/generic,
    add the `vm_run_source_no_call_fast` run + equality assertion (find the single summarize/
    compare funnel — grep `vm_run_source_generic` — and extend it there, NOT per-test).
  - `tests/property.rs::three_way_differential_over_generated_programs` → four-way.
  - `fuzz/fuzz_targets/differential.rs`: add the fourth projection + `assert_eq!` (mirror the
    existing `gen` arm).
- [ ] **Step 6:** `cargo test --test vm_differential` (both configs) + `cargo test --test
  call_fast` + clippy both configs — green (the flag is inert; identity is trivially true).
- [ ] **Step 7: Commit** — `git commit -m "feat(vm/call): call_fast kill switch + stats + fourth differential mode/fuzz axis (CALL §8)"`.
- [ ] **Step 8: Independent review** — reviewer confirms: generic mode sets `call_fast=false`;
  the fuzz target builds (`cargo fuzz build differential` if cargo-fuzz available, else
  `cargo check` under the fuzz workspace); no behavior change (spot-diff a corpus run).

---

## Phase 2 — Unit A: the allocation diet (each item individually revertible + measured)

### Task 2.1 (A1): empty-cells fast path

**Files:** modify `src/vm/fiber.rs`, `src/vm/run.rs`; create `tests/alloc_count.rs`.

- [ ] **Step 1: Write the failing tests** —
  (a) unit, in `src/vm/fiber.rs` tests:

```rust
#[test]
fn alloc_cells_is_allocation_free_when_no_cell_slots() {
    let cells = alloc_cells(8, &[]);
    assert!(cells.is_empty(), "no cell slots => empty Vec (capacity 0, no heap alloc)");
    assert_eq!(cells.capacity(), 0);
    // With cell slots the vector is still fully sized (unchanged behavior).
    let cells = alloc_cells(3, &[1]);
    assert_eq!(cells.len(), 3);
    assert!(cells[1].is_some());
}
```

  (b) the Gate-18 tripwire, `tests/alloc_count.rs` (new test binary with a counting
  allocator — the slope method, CALL §8.4):

```rust
//! CALL Gate-18: per-call/per-element allocation-count slope probes. A dedicated
//! binary because it installs a global allocator. Slopes (allocs(2N)-allocs(N))/N
//! are immune to startup/runtime noise; budgets are generous tripwires, not exact
//! counts — a regression that doubles a slope trips them, allocator jitter does not.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn allocs_for(src: &str, call_fast: bool) -> u64 {
    // Run on the same worker-stack + current-thread-runtime idiom the differential
    // uses; measure ALLOCS around the run only.
    let before = ALLOCS.load(Relaxed);
    run_program(src, call_fast); // helper over vm_run_source / vm_run_source_no_call_fast
    ALLOCS.load(Relaxed) - before
}

fn slope(make_src: impl Fn(u64) -> String, call_fast: bool) -> f64 {
    const N: u64 = 20_000;
    let a = allocs_for(&make_src(N), call_fast);
    let b = allocs_for(&make_src(2 * N), call_fast);
    (b.saturating_sub(a)) as f64 / N as f64
}

#[test]
fn capture_free_call_alloc_slope_drops_with_call_fast() {
    let mk = |n: u64| format!(
        "fn f(a, b) {{ return a + b }}\nlet s = 0\nfor i in 0..{n} {{ s = f(s, 1) }}\nprint(s)"
    );
    let on = slope(&mk, true);
    let off = slope(&mk, false);
    // A1+A2: the qualifying call shape allocates ~0 per call; the off path pays
    // cells-vec + 2 arg Vecs (>= 3). Generous tripwire bounds:
    assert!(on < 1.0, "call_fast per-call alloc slope {on} (want ~0)");
    assert!(on < off / 2.0, "on={on} not < half of off={off}");
}
```

  (For Task 2.1 alone, assert a weaker interim bound — `on < off` and `on <= off - 0.9`
  (one allocation removed per call); tighten to the final bound in Task 2.2.)
- [ ] **Step 2: Run — expect FAIL** (`capacity == 0` fails; slope unchanged).
- [ ] **Step 3: Implement** —
  - `src/vm/fiber.rs::alloc_cells` (`fiber.rs:56`):

```rust
pub(crate) fn alloc_cells(
    slot_count: usize,
    cell_slots: &[u32],
) -> Vec<Option<Cc<RefCell<Value>>>> {
    // CALL §2: the overwhelmingly common case — this frame captures nothing by
    // reference — allocates NOTHING (an empty Vec touches no heap). Every binding
    // consumer indexes via `.get(slot)` so the empty vector is safe by construction.
    if cell_slots.is_empty() {
        return Vec::new();
    }
    let mut cells = vec![None; slot_count];
    for &slot in cell_slots {
        let idx = slot as usize;
        cells[idx] = Some(Cc::new(RefCell::new(Value::Nil)));
    }
    cells
}
```

  - Switch EVERY direct `cells[slot]` / `cells[0]` binding-loop index in `src/vm/run.rs` to
    `cells.get(slot).and_then(|c| c.as_ref())`. Sites (verify by `grep -n "cells\[" src/vm/run.rs`):
    the generator arms (`:1687`, `:4517`), the `Op::Call` plain arm (`:1797`), `call_value`
    (`:4547`), the CallMethod IC push (`:4964` — `cells[0]` for self — and `:4973`), and the
    `invoke_compiled_method`/`invoke_compiled_static` binding loops. Pattern:

```rust
if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
    *cell.borrow_mut() = v;
} else {
    fiber.stack[slot_base + slot] = v;
}
```

    Note: this site rewrite is SAFE unconditionally (an empty vec only exists when no slot is
    a cell), so it is NOT gated on `call_fast` — but the `Vec::new()` early-return IS:
    thread a `call_fast: bool` argument? **No** — decision recorded: the early return is
    behavior-invisible (an all-`None` vector and an empty vector are indistinguishable to
    `.get`-based consumers), so A1 is gated only by the differential, not the flag; the flag
    gates paths with new CONTROL FLOW (A2/A3/B). Document this in the `call_fast` doc-comment.
- [ ] **Step 4: Run — expect PASS** (unit + interim slope). `cargo test --test vm_differential`
  both configs — byte-identical.
- [ ] **Step 5: Measure item A1 individually:** run `bench/run_call_bench.sh`, append an
  `### A1` row (time + RSS + slope numbers) to `bench/CALL_RESULTS.md`.
- [ ] **Step 6: Commit** — `git commit -m "perf(vm/call): A1 — empty-cells fast path, .get-based binding loops (CALL §2)"`.
- [ ] **Step 7: Independent review** — reviewer greps for any remaining direct `cells[` index
  on a binding path, probes a closure-capturing program (`let fs = []; for i in 0..3 {
  fs.push(() => i) }`; per-iteration freshness) across all four modes, and re-runs the
  differential.

### Task 2.2 (A2): in-place argument binding over the stack window

**Files:** modify `src/interp.rs`, `src/vm/run.rs`; extend `tests/call_fast.rs`,
`tests/alloc_count.rs`.

- [ ] **Step 1: Write the failing tests** — in `tests/call_fast.rs`, a panic-wording parity
  battery (these must pass BEFORE and AFTER — write them first against today's behavior, then
  they guard the refactor):

```rust
/// Arity/contract panic wording is byte-identical across all four modes for every
/// arg shape the in-place path touches (CALL §3.4). Each case asserts the same
/// (stdout, panic message) on tw / spec / generic / no-call-fast.
#[test]
fn arg_panic_wording_parity() {
    let cases = [
        // exact arity, too few / too many
        r#"fn f(a, b) {} f(1)"#,
        r#"fn f(a, b) {} f(1, 2, 3)"#,
        // defaults: at-least wording + prologue evaluation order
        r#"fn f(a, b = a + 1) { print(b) } f(5)"#,
        r#"fn f(a, b = 1) {} f()"#,
        // contract violation on the 2nd arg (left-to-right order)
        r#"fn f(a: number, b: string) {} f(1, 2)"#,
        // rest param (must FALL BACK, identical behavior)
        r#"fn f(a, ...rest: array<number>) { print(rest) } f(1, 2, "x")"#,
        r#"fn f(...rest) { print(rest) } f(1, 2, 3)"#,
    ];
    for src in cases { assert_four_mode_identical(src); }
}
```

  and the tightened alloc slope from Task 2.1 Step 1 (`on < 1.0`).
- [ ] **Step 2: Run — expect** the parity battery PASSES already (it pins today's wording);
  the tightened slope FAILS.
- [ ] **Step 3: Implement** —
  - `src/interp.rs`: extract the shared cores from `check_call_args` (`interp.rs:7898`) with
    NO wording/order change:

```rust
/// Shared arity gate (CALL §3.3): the EXACT wording/branching previously inline in
/// `check_call_args` — "expected N" for exact arity, "at least"/"at most" otherwise.
fn check_call_arity(
    params: &[crate::ast::Param],
    n_args: usize,
    span: Span,
    what: &str,
) -> Result<(), Control> { /* moved verbatim from check_call_args */ }

/// Shared per-arg contract gate (CALL §3.3): env-aware when the engine supplies
/// (interp, env); the env-free `check_type` otherwise. Synchronous; never awaits.
fn check_param_contract(
    p: &crate::ast::Param,
    a: &Value,
    span: Span,
    interp: Option<&Interp>,
    env: Option<&Environment>,
) -> Result<(), Control> { /* the (interp,env) match + contract_panic, moved verbatim */ }

/// CALL §3: the borrowing twin of [`check_call_args`] — IDENTICAL checks over args
/// already positioned on the operand stack, no Vec consumed or produced. Caller
/// guarantees `!has_rest` (rest collection genuinely allocates; those calls keep
/// the Vec path). Returns the supplied count (== args.len(), capped by arity).
pub(crate) fn check_call_args_in_place(
    params: &[crate::ast::Param],
    args: &[Value],
    span: Span,
    what: &str,
    interp: Option<&Interp>,
    env: Option<&Environment>,
) -> Result<usize, Control> {
    debug_assert!(!params.last().is_some_and(|p| p.rest));
    check_call_arity(params, args.len(), span, what)?;
    for (p, a) in params.iter().zip(args.iter()) {
        check_param_contract(p, a, span, interp, env)?;
    }
    Ok(args.len())
}
```

    `check_call_args` itself becomes a consumer of the two cores (same `BoundArgs` result,
    same signature — every other caller untouched).
  - `src/vm/run.rs` `Op::Call` (`:1757` arm): insert the qualifying fast path BEFORE the
    existing pop-into-Vec body, guarded and falling through:

```rust
Value::Closure(callee) => {
    let call_span = fiber.frame().closure.proto.chunk.span_at(fault_ip);
    let what = callee.proto.chunk.name.as_deref().unwrap_or("function");
    // CALL §3 (A2): in-place binding — the args already sit contiguously at
    // `stack[callee_idx+1..]`, exactly one slot above the new frame's window.
    // Qualify: no rest param (defaults are fine — the prologue fills Nil slots
    // driven by `frame.argc`, and `resize` writes the same Nils the BoundArgs
    // placeholders carried). Checks are the SAME shared cores `check_call_args`
    // uses — wording/order byte-identical by construction.
    if self.call_fast && !callee.proto.has_rest {
        let supplied = crate::interp::check_call_args_in_place(
            &callee.proto.params,
            &fiber.stack[callee_idx + 1..],
            call_span,
            what,
            Some(&self.interp),
            Some(&self.class_env()),
        )?;
        // Drop the callee value; the argc args shift down one slot to start AT
        // slot_base (an argc-element memmove, zero allocation).
        fiber.stack.remove(callee_idx);
        let slot_base = callee_idx;
        let slot_count = callee.proto.chunk.slot_count as usize;
        let cells =
            super::fiber::alloc_cells(slot_count, &callee.proto.chunk.cell_slots);
        fiber.stack.resize(slot_base + slot_count, Value::Nil);
        // Rare: a param the resolver promoted to a cell slot moves from the
        // window into its cell (a callback mutating a captured param).
        if !cells.is_empty() {
            for slot in 0..supplied {
                if let Some(cell) = cells.get(slot).and_then(|c| c.as_ref()) {
                    *cell.borrow_mut() = std::mem::replace(
                        &mut fiber.stack[slot_base + slot],
                        Value::Nil,
                    );
                }
            }
        }
        self.bump_stat(|s| s.inplace_binds += 1);
        self.enter_frame_depth(call_span)?;
        fiber.frames.push(super::fiber::CallFrame {
            closure: callee,
            ip: 0,
            slot_base,
            cells,
            ret_span: call_span,
            def_class: None,
            argc: supplied,
        });
        self.publish_profile_frames(fiber);
    } else {
        /* the EXISTING pop-into-Vec + check_call_args body, verbatim */
    }
}
```

- [ ] **Step 4: Run — expect PASS**: parity battery (all four modes), tightened slope, full
  `cargo test --test vm_differential` both configs, `cargo test` default config.
- [ ] **Step 5: Measure item A2 individually** → `### A2` row in `bench/CALL_RESULTS.md`.
- [ ] **Step 6: Commit** — `git commit -m "perf(vm/call): A2 — in-place arg binding over the operand-stack window (CALL §3)"`.
- [ ] **Step 7: Independent review** — reviewer probes: defaults referencing earlier params
  (`fn f(a, b = a * 2)`), a default that itself calls a function, contract panic on arg 3 of 3
  (order), `CallSpread` through the same arm (`f(...[1,2])`), a cell-slot param
  (`fn f(x) { let g = () => { x = x + 1; return x }; g(); return x }` called via the VM),
  recursion-limit depth parity, and the panic-path stack-shape note (§3.4 — confirm no path
  resumes a fiber after `Err`).

### Task 2.3 (A3): fiber pooling for re-entrant calls

**Files:** modify `src/vm/fiber.rs` (`Fiber::reset`), `src/vm/run.rs`; extend
`tests/call_fast.rs`, `tests/alloc_count.rs`.

- [ ] **Step 1: Write the failing tests** —
  (a) `tests/alloc_count.rs`: re-entrant call slope (each element of a `map` over a NON-closure
  path is Phase 3; here use `recover`/method dispatch — simplest deterministic re-entry is a
  bound-method call through `call_value`):

```rust
#[test]
fn reentrant_call_value_fiber_is_pooled() {
    // arr.map drives Vm::call_value per element (pre-trampoline): the per-element
    // Fiber's two Vecs disappear under pooling.
    let mk = |n: u64| format!(
        "let xs = 0..{n}\nlet s = xs.map((x) => x + 1)\nprint(len(s))"
    );
    let on = slope(&mk, true);
    let off = slope(&mk, false);
    assert!(on < off, "pooling should reduce per-element allocs: on={on} off={off}");
}
```

  (b) `src/vm/fiber.rs` unit tests for `reset`:

```rust
#[test]
fn reset_reestablishes_the_one_frame_invariant() {
    let closure = closure_with_cell_slots(3, vec![2]);
    let mut f = Fiber::new(closure.clone());
    // Simulate mid-flight wreckage: extra operands, a dirty state.
    f.push(Value::Int(1));
    f.push(Value::Int(2));
    f.state = FiberState::Done;
    let old_cell = f.frame().cells[2].clone();
    f.reset(closure);
    assert_eq!(f.frames.len(), 1);
    assert_eq!(f.frame().ip, 0);
    assert_eq!(f.frame().slot_base, 0);
    assert_eq!(f.stack.len(), 3);
    assert!(f.stack.iter().all(|v| matches!(v, Value::Nil)));
    assert_eq!(f.state, FiberState::Running);
    // Cells are FRESH per reset (capture identity — CALL §4.2/§5.4).
    assert!(!gcmodule::Cc::ptr_eq(
        old_cell.as_ref().unwrap(),
        f.frame().cells[2].as_ref().unwrap()
    ));
}
```

  (verify `Cc::ptr_eq` exists — if not, compare via `cc_addr`/raw pointer like `gc::cc_addr`).
- [ ] **Step 2: Run — expect FAIL** (no `reset`, no pool).
- [ ] **Step 3: Implement** —
  - `src/vm/fiber.rs`:

```rust
/// CALL §4: re-establish the fresh one-frame state (the Fiber::new postcondition)
/// over this fiber's EXISTING Vec capacities. Clears frames/stack UNCONDITIONALLY
/// (a panic-unwound fiber carries arbitrary mid-flight state); allocates FRESH
/// cells (cell identity is observable — never reused across calls).
pub fn reset(&mut self, top: Cc<Closure>) {
    let slot_count = top.proto.chunk.slot_count as usize;
    self.frames.clear();
    self.stack.clear();
    self.stack.resize(slot_count, Value::Nil);
    let cells = alloc_cells(slot_count, &top.proto.chunk.cell_slots);
    self.frames.push(CallFrame {
        closure: top,
        ip: 0,
        slot_base: 0,
        cells,
        ret_span: Span::new(0, 0),
        def_class: None,
        argc: 0,
    });
    self.state = FiberState::Running;
}
```

  - `src/vm/run.rs`: `fiber_pool: RefCell<Vec<Fiber>>` on `Vm` (+ `const FIBER_POOL_MAX:
    usize = 8;`), with:

```rust
/// CALL §4.2: take = exclusive ownership (pop removes it — recursion/re-entrancy
/// safe by construction). Returned ONLY on clean completion; an Err-unwound fiber
/// is dropped (today's behavior — never pool mid-flight state). Borrows are short
/// synchronous pop/push — never held across an await.
fn take_pooled_fiber(&self, top: Cc<Closure>) -> Fiber {
    if self.call_fast {
        if let Some(mut f) = self.fiber_pool.borrow_mut().pop() {
            f.reset(top);
            self.bump_stat(|s| s.pooled_fiber_reuses += 1);
            return f;
        }
    }
    Fiber::new(top)
}

fn return_pooled_fiber(&self, mut fiber: Fiber) {
    if !self.call_fast {
        return;
    }
    let mut pool = self.fiber_pool.borrow_mut();
    if pool.len() < FIBER_POOL_MAX {
        fiber.frames.clear();
        fiber.stack.clear();
        pool.push(fiber);
    }
}
```

  - Adopt at the run-to-completion re-entry sites ONLY: `Vm::call_value`'s Closure arm
    (`run.rs:4540` — replace `Fiber::new(closure)` with `take_pooled_fiber(closure)`, then on
    the `Done(v)` arm `self.return_pooled_fiber(fiber); Ok(v)`), `invoke_compiled_method`
    (`:5604`), `invoke_compiled_static` (`:5693`), and `vm_construct`'s init drive if it owns
    a fiber (read it; adopt only if it is a run-to-completion fiber). **Do NOT touch** the
    generator builds (`run.rs:1682/4512` — the handle owns those fibers), the module fiber
    (`:987`), or the program root.
- [ ] **Step 4: Run — expect PASS** + `vm_differential` both configs + the existing
  `call_value_invokes_a_closure_repeatedly_each_on_its_own_fiber` unit (`run.rs:7701` — read
  it; if it asserts fiber NON-reuse it documents the OLD contract and must be updated to
  assert the new pooled contract with the kill switch off variant preserved — record this as a
  deliberate contract change in the commit message).
- [ ] **Step 5: Probe tests (add to `tests/call_fast.rs`):** deep nested re-entrancy
  (`map` inside `map` inside `map` — distinct fibers live simultaneously), a panicking callee
  (fiber dropped, pool not poisoned — next call still correct), a generator + `for await`
  interleaved with pooled calls, all four-mode identical.
- [ ] **Step 6: Measure item A3 individually** → `### A3` row in `bench/CALL_RESULTS.md`
  (include retained-pool memory note: ≤8 fibers/isolate).
- [ ] **Step 7: Commit** — `git commit -m "perf(vm/call): A3 — fiber pooling at the re-entrant call funnels (CALL §4)"`.
- [ ] **Step 8: Independent review** — reviewer hunts double-live hazards (a pooled fiber
  reachable twice), audits every `return_pooled_fiber` call dominates a `Done` (never an
  `Err`/`Yielded`), confirms no borrow of `fiber_pool` crosses an await (clippy
  `await_holding_refcell_ref` is the backstop but eyeball it), and stress-runs the workers
  examples (per-isolate Vm ⇒ per-isolate pool; no cross-thread sharing).

### Task 2.4: Phase 2 holistic review

- [ ] **Step 1:** Holistic-review subagent over the combined Unit A diff: A1/A2/A3 compose
  (in-place binding + empty cells + pooling on one call), per-item bench rows are real, the
  differential + fuzz axis green both configs, clippy clean both configs, `cargo test` +
  `cargo test --no-default-features` green.
- [ ] **Step 2:** Re-run `tests/vm_bench.rs` — spec/tw geomean still ≥2× and no
  spec-vs-generic regression; record interim numbers in `bench/CALL_RESULTS.md`.
- [ ] **Step 3:** Any finding becomes a tracked fix in this phase before Phase 3 starts.

---

## Phase 3 — Unit B: the callback trampoline

### Task 3.1: `CallbackTrampoline` + the reset invariant (no stdlib wiring yet)

**Files:** create `src/vm/trampoline.rs`; modify `src/vm/mod.rs` (declare), `src/vm/run.rs`
(make `run_loop_sync` reachable if LANE left it private); extend `tests/call_fast.rs`.

- [ ] **Step 1: Write the failing tests** (Rust harness in `src/vm/trampoline.rs` tests +
  `tests/call_fast.rs`): build a Vm, compile a source defining a callback, fetch the closure
  value, arm a trampoline, and assert —
  (a) three sequential `call1` invocations return correct results (the same fiber reused —
  assert via `call_fast_stats().trampoline_calls == 3`);
  (b) a mid-element contract panic (`fn cb(x: string) {...}` called with an int) yields the
  byte-identical `Control::Panic` message AND the NEXT `call1` on the same trampoline still
  returns the correct result (the reset invariant — no poisoned state);
  (c) an async-awaiting callback (`(x) => { return await asyncDouble(x) }` shape — a sync
  closure that awaits a future it obtains) escalates: result correct,
  `trampoline_escalations >= 1`;
  (d) `call_depth` exactly-once: a self-recursive callback trips
  `maximum recursion depth exceeded` at the identical depth under trampoline-on and
  trampoline-off (compare the two entry points' outputs).
- [ ] **Step 2: Run — expect FAIL** (module missing).
- [ ] **Step 3: Implement `src/vm/trampoline.rs`:**

```rust
//! CALL §5 — the higher-order callback trampoline. ONE reused fiber drives a plain
//! (non-async/generator/worker) VM-closure callback across all elements of a
//! higher-order builtin loop on LANE's sync lane; a suspension escalates THAT
//! element's live fiber onto the async driver (never re-executed). The tree-walker
//! is untouched by construction: arming requires a `Value::Closure`, which only the
//! VM produces (`Interp.vm` weak, interp.rs).

use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use crate::vm::fiber::Fiber;
use crate::vm::value_ext::{Closure, RunOutcome};
use crate::vm::Vm;
use gcmodule::Cc;
use std::rc::Rc;

pub(crate) struct CallbackTrampoline {
    vm: Rc<Vm>,
    closure: Cc<Closure>,
    /// The ONE reused fiber. `None` only before the first element (lazily built so
    /// arming a trampoline for an empty input allocates nothing).
    fiber: Option<Fiber>,
    span: Span,
}

/// A builtin loop's callback driver: the trampoline fast path, or the exact
/// today-path (per-element `Interp::call_value`) for everything else. One enum so
/// every stdlib site keeps a single loop.
pub(crate) enum CallbackDriver<'i> {
    Tramp(CallbackTrampoline),
    Generic { interp: &'i Interp, f: Value, span: Span },
}

impl CallbackTrampoline {
    /// CALL §5.2: arm iff `f` is a plain VM closure (the engine seam — tree-walker
    /// callbacks are `Value::Function` and never arrive here), the Vm upgrades, and
    /// the kill switch is on. `None` => generic path, behavior-identical.
    pub(crate) fn arm(interp: &Interp, f: &Value, span: Span) -> Option<CallbackTrampoline> {
        let Value::Closure(c) = f else { return None };
        if c.proto.is_async || c.proto.is_generator || c.proto.is_worker {
            return None;
        }
        let vm = interp.vm()?;
        if !vm.call_fast() {
            return None;
        }
        Some(CallbackTrampoline { vm, closure: c.clone(), fiber: None, span })
    }

    /// Run ONE callback invocation. `args` live in the caller's stack array — they
    /// are moved (mem::take) into the slot window; NO per-element Vec, NO boxed
    /// future on the non-suspending path.
    pub(crate) async fn call(&mut self, args: &mut [Value]) -> Result<Value, Control> {
        let what = self.closure.proto.chunk.name.as_deref().unwrap_or("function");
        // check_call_args semantics per element (ELIDE is a different spec): the
        // SAME shared cores — byte-identical arity/contract panics, same order.
        let supplied = crate::interp::check_call_args_in_place(
            &self.closure.proto.params,
            args,
            self.span,
            what,
            Some(self.vm.interp()),
            Some(&self.vm.class_env()),
        )?;
        let mut fiber = match self.fiber.take() {
            Some(mut f) => {
                // THE RESET INVARIANT (§5.4): one fresh frame @ip0, slot_count Nil
                // stack, fresh cells, Running — regardless of the last element's fate.
                f.reset(self.closure.clone());
                f
            }
            None => Fiber::new(self.closure.clone()),
        };
        fiber.frame_mut().ret_span = self.span;
        fiber.frame_mut().argc = supplied;
        for (slot, a) in args.iter_mut().enumerate() {
            let v = std::mem::replace(a, Value::Nil);
            if let Some(cell) = fiber.frame().cells.get(slot).and_then(|c| c.as_ref()) {
                *cell.borrow_mut() = v;
            } else {
                fiber.stack[slot] = v;
            }
        }
        // SP3: exactly ONE logical-call increment per element (RAII, unwinds on Err)
        // — identical to Vm::call_value's discipline.
        let _depth = self.vm.interp().enter_call_depth_scoped(self.span)?;
        self.vm.bump_trampoline_call();
        // The sync lane (LANE): no .await, no boxed future. grow() = the same SP9
        // native-stack discipline the async funnel uses.
        let outcome = match crate::vm::stack::grow(|| self.vm.run_loop_sync(&mut fiber))? {
            crate::vm::run::SyncOutcome::Finished(outcome) => outcome,
            crate::vm::run::SyncOutcome::NeedsAsync => {
                // ESCALATION (§5.3): continue the SAME live fiber on the async
                // driver — NEVER re-dispatch through call_value (side effects up to
                // the suspension already happened). Costs this element exactly
                // today's boxed-future price; the NEXT element re-arms the sync lane.
                self.vm.bump_trampoline_escalation();
                crate::vm::stack::grow_future(self.vm.run(&mut fiber)).await?
            }
        };
        match outcome {
            RunOutcome::Done(v) => {
                self.fiber = Some(fiber); // retained for the next element
                Ok(v)
            }
            RunOutcome::Yielded(_) => {
                unreachable!("a non-generator callback cannot yield (compiler bug)")
            }
        }
    }
}

impl CallbackDriver<'_> {
    pub(crate) async fn call1(&mut self, a: Value) -> Result<Value, Control> {
        match self {
            Self::Tramp(t) => {
                let mut args = [a];
                t.call(&mut args).await
            }
            Self::Generic { interp, f, span } => {
                interp.call_value(f.clone(), vec![a], *span).await
            }
        }
    }
    pub(crate) async fn call2(&mut self, a: Value, b: Value) -> Result<Value, Control> {
        match self {
            Self::Tramp(t) => {
                let mut args = [a, b];
                t.call(&mut args).await
            }
            Self::Generic { interp, f, span } => {
                interp.call_value(f.clone(), vec![a, b], *span).await
            }
        }
    }
}

impl Interp {
    /// The one constructor every stdlib iteration site uses (CALL §5.5).
    pub(crate) fn callback_driver<'i>(&'i self, f: Value, span: Span) -> CallbackDriver<'i> {
        match CallbackTrampoline::arm(self, &f, span) {
            Some(t) => CallbackDriver::Tramp(t),
            None => CallbackDriver::Generic { interp: self, f, span },
        }
    }
}
```

  Confirm LANE's shipped names per Task 0.1 (`Vm::run_loop_sync(&mut Fiber) ->
  Result<SyncOutcome, Control>`, `SyncOutcome::{Finished(RunOutcome), NeedsAsync}`; adjust only
  the module path if `SyncOutcome` lives elsewhere). Add the small `Vm`
  accessors this needs (`call_fast()`, `interp()`, `bump_trampoline_call/escalation` over
  `bump_stat`). The `Err` arm intentionally drops the fiber? **No** — note: on `Err` the
  fiber was already taken out of `self.fiber`; it drops with the early `?` return. The next
  `call` lazily rebuilds via `Fiber::new` — correct (reset would also be correct, but
  drop-on-err matches the pool discipline and keeps the invariant trivially provable). Add a
  unit test pinning exactly that sequence (b above).
- [ ] **Step 4: Run — expect PASS** (a)–(d); clippy both configs (watch
  `await_holding_refcell_ref`: `class_env()` returns an owned/cloned env — verify, don't hold
  a borrow across `t.call(...).await` in the tests).
- [ ] **Step 5: Commit** — `git commit -m "feat(vm/call): CallbackTrampoline + CallbackDriver — one reused fiber on the sync lane, per-element escalation (CALL §5)"`.
- [ ] **Step 6: Independent review** — reviewer probes the reset invariant adversarially
  (panic at different depths inside the callback, propagate (`?`) inside the callback body,
  a callback that spawns an un-awaited async fn — inflight accounting unchanged), and
  verifies the escalation path against a callback that awaits a TIMER (a genuinely pending
  future, not a ready one).

### Task 3.2: wire `array.rs` + `object.mapValues`

**Files:** modify `src/stdlib/array.rs`, `src/stdlib/object.rs`; extend `tests/call_fast.rs`.

- [ ] **Step 1: Write the failing test** — the coverage (anti-false-green) assertion:

```rust
#[test]
fn trampoline_actually_runs_on_array_builtins() {
    // Run a map/filter/reduce/sort/groupBy pipeline on the specialized VM and
    // assert the trampoline counters moved (CALL §8.3) AND output matches the
    // other three modes.
    let src = r#"
        let xs = (0..200).map((x) => x * 2)
            .filter((x) => x % 3 != 0)
            .sort((a, b) => b - a)
        let total = xs.reduce((acc, x) => acc + x, 0)
        print(total, xs.groupBy((x) => x % 10 == 0 ? "round" : "other").keys().len())
    "#;
    let (out, stats) = run_with_stats(src); // helper: vm_run_source variant exposing Vm stats
    assert!(stats.trampoline_calls > 0, "trampoline never armed — false green");
    assert_four_mode_identical(src);
}
```

  (The `run_with_stats` helper needs a `#[doc(hidden)]` lib entry that returns the Vm's
  `call_fast_stats` alongside output — add it in this task, mirroring `vm_run_source_cfg`.)
- [ ] **Step 2: Run — expect FAIL** (`trampoline_calls == 0` — sites not wired).
- [ ] **Step 3: Implement** — adopt `callback_driver` at each §5.5 array/object site keeping
  ONE loop per site. `map` becomes (pattern for all 11 + mapValues):

```rust
"map" => {
    let arr = want_array(&arg(args, 0), span, &ctx("map"))?;
    let f = arg(args, 1);
    let items = arr.borrow().clone();
    let mut out = Vec::with_capacity(items.len());
    // CALL §5: one driver for the whole loop — trampoline for a plain VM closure,
    // the exact per-element call_value path otherwise (tree-walker callbacks,
    // async/generator/worker callees, kill switch off).
    let mut cb = self.callback_driver(f, span);
    for item in items.into_iter() {
        out.push(cb.call1(item).await?);
    }
    Ok(Value::Array(crate::value::ArrayCell::new(out)))
}
```

  Site-by-site notes (read each before editing): `filter`/`find`/`partition` pass
  `item.clone()` (the item survives the call) — keep the clone exactly where it is today;
  `reduce` is `cb.call2(acc, item)`; the `sort` comparator is `cb.call2(item.clone(),
  sorted[lo].clone())` inside the insertion loop (the driver is armed ONCE outside both
  loops); `groupBy` clones the item for the key call; `mapValues` is
  `cb.call2(v.clone(), Value::Str(k.as_str().into()))`. **Behavioral rule:** argument values,
  clone placement, truthiness checks, error messages (`array.sort comparator must return a
  number, got ...`) are untouched — only the call ceremony changes.
- [ ] **Step 4: Run — expect PASS**; full `vm_differential` both configs (the corpus is rich
  in map/filter/reduce — this IS the main guard); `cargo test --no-default-features` (array/
  object are core — must build & pass).
- [ ] **Step 5: Commit** — `git commit -m "perf(stdlib/call): trampoline array.* + object.mapValues iteration sites (CALL §5.5)"`.
- [ ] **Step 6: Independent review** — reviewer diffs each site against `main` asserting
  ONLY the ceremony changed (clones/truthiness/messages identical), runs the tree-walker over
  the same corpus (`Value::Function` callbacks — `cargo test --test vm_differential` covers,
  plus an explicit `ASCRIPT_ENGINE=tree-walker` example run), and probes: empty arrays
  (trampoline armed, zero calls — lazily no fiber), a callback that mutates the source array
  mid-map (pre-cloned items semantics unchanged), nested `map`-in-`map` with two live
  trampolines.

### Task 3.3: wire `stream.rs` + record the non-wired decision table

**Files:** modify `src/stdlib/stream.rs`; extend `tests/call_fast.rs`.

- [ ] **Step 1: Write the failing test** — a stream pipeline
  (`stream.range(0, 1000).map(f).filter(g)` consumed via `each`/`reduce`/`find`) asserting
  stats moved + four-mode identity (same shape as Task 3.2's).
- [ ] **Step 2: Run — expect FAIL** (stats zero on stream ops).
- [ ] **Step 3: Implement** — read `stream.rs:560-710` first: the pipeline stage calls
  (`:577/581/600`) live inside the per-pull driver and the terminal loops (`each` `:669`,
  `reduce` `:680`, `find` `:702`). Arm ONE driver per stage function at pipeline-drive setup
  (the stage fns are fixed for the pipeline's life), falling back per the same rule. If the
  stage storage makes holding a `CallbackDriver` across pulls awkward (lifetimes through the
  stream state machine), arm per terminal-loop instead and record the narrowing in the code
  comment + spec deviation note — do NOT contort the stream engine.
- [ ] **Step 4: Run — expect PASS** + differential both configs.
- [ ] **Step 5:** Add the §5.5 non-wired decision table as a module comment in
  `src/vm/trampoline.rs` (events/sync/task/timers/bench/assert/schema/net_http/workflow/
  http_server: single-shot sites, generic path, still benefit from A1/A3 — documented
  decision, not a deferral).
- [ ] **Step 6: Commit** — `git commit -m "perf(stdlib/call): trampoline stream pipeline sites; record non-wired decision table (CALL §5.5)"`.
- [ ] **Step 7: Independent review** — reviewer probes async stage fns in a stream (must take
  the generic path), `for await` over a wired stream, backpressure unchanged.

### Task 3.4: escalation & control-flow edge battery + examples

**Files:** create `examples/functional_pipelines.as`, `examples/advanced/callback_escalation.as`;
extend `tests/call_fast.rs`, `tests/vm_differential.rs` (corpus registration if not automatic).

- [ ] **Step 1:** Write `examples/functional_pipelines.as` (intro; Gate 9 happy path):
  map/filter/reduce/sort/groupBy/partition pipelines over real data, `stream.range` pipeline,
  deterministic printed output. Verify with `target/release/ascript run` and `ascript fmt`
  idempotence.
- [ ] **Step 2:** Write `examples/advanced/callback_escalation.as` (Gate 9 edge cases,
  production-shaped, fully error-handled): a map whose callback awaits an `async fn`
  (escalation per element); a `recover`-wrapped reduce whose callback panics mid-fold; a
  filter using `?`-propagation out through the enclosing function returning `[value, err]`;
  a callback closing over a mutated local (cell capture freshness across elements); a sort
  comparator that throws on a poisoned element, recovered. Deterministic output; runs to
  completion (NOT skip-listed).
- [ ] **Step 3:** Add the focused four-mode tests in `tests/call_fast.rs` mirroring each edge
  (panic message parity, recover catches the same `Control::Panic`, `?` carries the same pair,
  recursion-limit parity through a trampolined callback) and assert
  `trampoline_escalations > 0` on the escalation example (coverage of the fallback itself —
  anti-false-green for the escalation arm).
- [ ] **Step 4:** Run the examples in all four modes + `.aso` (`build` then `run file.aso`) —
  byte-identical; corpus tests green both configs.
- [ ] **Step 5: Commit** — `git commit -m "examples(call): functional pipelines + escalation edge corpus (CALL Gates 9/15)"`.
- [ ] **Step 6: Independent review** — reviewer runs both examples under
  `--tree-walker`, the VM, generic, no-call-fast, AND compiled `.aso`; runs
  `ascript check` (0 diagnostics); probes one extra edge of their choosing (e.g. a worker-fn
  value passed to `map` — must dispatch to the pool, not trampoline).

### Task 3.5: Phase 3 holistic review

- [ ] **Step 1:** Holistic-review subagent over the combined Unit B diff: the seam claim
  (tree-walker untouched — `git diff` shows no tree-walker-path behavior change; stdlib diffs
  are ceremony-only), reset invariant tests adversarially re-probed, escalation never
  re-executes an element (read the code path; confirm no `call_value` re-dispatch exists in
  the escalation arm), SP3 exactly-once audited at every increment site, clippy + full suite
  both configs.
- [ ] **Step 2:** Findings fixed in-phase before Phase 4.

---

## Phase 4 — Gates, measurement, docs, Definition of Done

### Task 4.1: same-session A/B + allocation report + zero-cost gates

**Files:** modify `bench/CALL_RESULTS.md`; no production code (unless a gate fails — then fix
in-branch, failing-test-first).

- [ ] **Step 1:** Same-session A/B (Gate 16): in ONE session, run `bench/run_call_bench.sh`
  on `main` (worktree or stash) and on the branch head; record both tables in
  `bench/CALL_RESULTS.md` — functional-idiom workloads (the headline), object_churn,
  json_roundtrip, async workloads (expect ~no change), with wall time AND peak RSS per
  workload. Profile the headline workload with the shipped profiler
  (`ascript run --profile cpu`) before/after; paste the attribution deltas.
- [ ] **Step 2:** Allocation counts (Gate 18): record the `tests/alloc_count.rs` slopes
  (per-call, per-element, re-entrant; on vs off) in the report. Memory regression anywhere =
  a bug to fix, never a tradeoff.
- [ ] **Step 3:** Re-run `tests/vm_bench.rs` fully: spec/tw geomean ≥2× (Gate 17 floor) AND
  `dbg_zero_cost_gate` (the call path is touched — instrument==None ≈ armed-idle must hold);
  record both numbers.
- [ ] **Step 4:** Kill-switch-off parity timing: `no_call_fast` mode within noise of
  pre-CALL `main` (the off path must cost nothing new).
- [ ] **Step 5: Commit** — `git commit -m "bench(call): same-session A/B + allocation slopes + RSS + zero-cost gates (CALL §6/§8.4)"`.

### Task 4.2: docs + status updates

**Files:** modify `CLAUDE.md`, `superpowers/roadmap.md`, `goal-perf.md`,
`bench/PROFILING_RESULTS.md`.

- [ ] **Step 1:** `CLAUDE.md`: add a condensed CALL entry under the campaign subsystems
  (the `call_fast` kill switch + fourth differential mode, the trampoline seam
  (`Value::Closure`-only ⇒ VM-only), the fiber-pool lifecycle rule (take=ownership,
  return-only-on-Done, generators never pooled), the reset invariant, and the
  alloc-count slope harness). No user-facing docs change (no API/syntax change — Gate 13
  satisfied by bench + repo docs; state this explicitly in the commit).
- [ ] **Step 2:** `goal-perf.md`: flip CALL's status to ✅ with a one-line result summary
  (measured numbers, not adjectives); add the post-CALL re-profile snapshot reference. Append
  the post-CALL section to `bench/PROFILING_RESULTS.md` (the mandatory re-profile checkpoint
  that re-ranks the remaining specs).
- [ ] **Step 3:** `superpowers/roadmap.md`: the milestone record entry.
- [ ] **Step 4: Commit** — `git commit -m "docs(call): CLAUDE.md/roadmap/goal-perf status + post-CALL profile snapshot"`.

### Task 4.3: full matrix + Definition of Done (the gates checklist — goal.md 1–14 + goal-perf 15–18)

**Files:** none (verification; fixes spawn tracked tasks).

- [ ] **Gate 1 (four-mode byte-identity):** `cargo test --test vm_differential` green in BOTH
  feature configs; examples identical on tree-walker / specialized / generic / no-call-fast /
  `.aso`-compiled.
- [ ] **Gate 2 (clippy):** `cargo clippy --all-targets` AND
  `cargo clippy --no-default-features --all-targets` clean.
- [ ] **Gate 3 (tests):** `cargo test` AND `cargo test --no-default-features` green
  (including `tests/alloc_count.rs`, `tests/call_fast.rs`).
- [ ] **Gate 4 (no borrow across await):** audited — `fiber_pool`/trampoline borrows are
  synchronous; clippy `await_holding_refcell_ref` clean.
- [ ] **Gate 5 (zero `type-*` corpus FPs):** `ascript check` over `examples/**` emits 0
  type/exhaustiveness diagnostics in both configs (the new examples included).
- [ ] **Gate 6 (no placeholders/silent deferrals):** the non-wired callback sites and the
  CallMethod in-place follow-up are DOCUMENTED decisions (§7/§5.5), not silent drops; grep for
  TODO/unimplemented in the diff.
- [ ] **Gate 7 (corpus migrated, never deleted):** no example/golden removed.
- [ ] **Gate 8 (continuous infra):** the differential fuzzer (fourth axis) + `tests/property.rs`
  four-way + CI fuzz smoke green.
- [ ] **Gate 9 (examples happy+edge):** `functional_pipelines.as` +
  `advanced/callback_escalation.as` runnable, fmt-idempotent, four-mode tested.
- [ ] **Gate 10 (unit tests happy+edge):** reset invariant, panic/propagate/recover,
  recursion-limit parity, empty-input, rest/defaults/contract shapes, pool re-entrancy — all
  present and green both configs.
- [ ] **Gate 11 (tooling parity):** no syntax/surface change ⇒ no grammar/LSP/fmt/REPL change;
  CONFIRM by running `tests/treesitter_conformance.rs` + `tests/frontend_conformance.rs` +
  `tests/lsp.rs` (green, untouched) and a REPL smoke (`map` over a closure in the REPL session
  Vm — the persistent-Vm path uses the same call paths).
- [ ] **Gate 12/17 (zero perf regression + the floor):** spec/tw geomean ≥2× holds;
  `dbg_zero_cost_gate` re-run green; no-call-fast mode at parity with pre-CALL baseline
  (recorded Task 4.1).
- [ ] **Gate 13 (docs):** Task 4.2 landed; bench report complete; no stale doc claims.
- [ ] **Gate 14 (production-grade, zero lingering bugs):** every bug found en route fixed
  in-branch with a failing-test-first regression guard (list them in the PR description);
  independent reviewers' open findings all closed.
- [ ] **Gate 15 (new config = differential mode + fuzz axis, with coverage):** the
  no-call-fast mode + fuzz axis landed in the SAME PR as the first fast path (Phase 1 before
  Phase 2 — verify commit order); coverage assertions (`trampoline_calls > 0`,
  `inplace_binds > 0`, `trampoline_escalations > 0` on the escalation example) green —
  anti-false-green proven.
- [ ] **Gate 16 (same-session A/B):** `bench/CALL_RESULTS.md` baseline and candidate measured
  in one session on one machine, shipped profiler used.
- [ ] **Gate 18 (memory measured):** allocation slopes + peak RSS recorded before/after; no
  regression unexplained.
- [ ] **Final holistic review (whole-effort):** a fresh subagent over the ENTIRE branch diff —
  spec §1–§9 coverage table, every plan checkbox ticked, zero open deferrals beyond the two
  recorded follow-ups (CallMethod in-place → DECODE; smallvec alternative → recorded decision),
  cross-phase composition probe (a functional pipeline inside a worker isolate inside a
  recovered panic, four-mode identical).
- [ ] **Merge:** `--no-ff` to `main` once everything above is green.

---

## Self-review (author pass)

- **Spec coverage:** §2 (A1) → Task 2.1; §3 (A2) → Task 2.2; §4 (A3) → Task 2.3; §5 (trampoline,
  seam, reset, escalation, site table) → Tasks 3.1–3.3; §6 (performance honesty) → Tasks 0.1,
  4.1; §7 (scope/rejected) → recorded in Tasks 2.3/3.3/4.3; §8 (kill switch, modes, fuzz axis,
  coverage, memory method) → Tasks 1.1, 3.2, 3.4, 4.1, 4.3.
- **No placeholders:** every code step shows real code grounded in the read sources
  (`alloc_cells` fiber.rs:56, the `Op::Call` arm run.rs:1757, `check_call_args` interp.rs:7898,
  `array.map` array.rs:52, `call_value` run.rs:4462); LANE-supplied names
  (`run_loop_sync`/`SyncOutcome::{Finished, NeedsAsync}`) are confirmed in Task 0.1 before use.
- **Order of operations:** the kill switch + differential mode + fuzz axis (Phase 1) land
  BEFORE any fast path (Gate 15); each diet item is a separate commit, individually measured
  and revertible; the trampoline lands only after its reset-invariant harness passes.
- **Known reconciliation point:** whether `Op::Break`/instrument force `NeedsAsync` is LANE's
  contract (the names — `run_loop_sync`/`SyncOutcome::{Finished, NeedsAsync}` — are LANE's
  exported ones, adopted here) — Task 0.1 confirms; if
  LANE shipped a different escalation surface, Task 3.1 adapts the trampoline to it and the
  spec gets a dated deviation note (never a silent fork).
