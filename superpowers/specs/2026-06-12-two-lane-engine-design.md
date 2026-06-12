# AScript Two-Lane Fiber Engine + Inline Ready-Future Completion — Design (LANE)

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** LANE (the first engine spec of the PERF campaign — see `goal-perf.md`)
- **Depends on:** **DEFER** (`2026-06-12-defer-statement-design.md`) — merged FIRST per owner
  decision; LANE rebases over DEFER's frame-exit/unwind drain hooks and classifies its two
  opcodes (§3, "DEFER coordination"). Otherwise first in the campaign's execution order
  (**includes the campaign's Phase-0 bench-corpus task** as plan Task 0, so every later spec
  has before/after numbers).
- **Depended on by:** CALL (the trampoline drives elements through the sync driver), DECODE (the
  sync driver is the pre-decoded stream's primary consumer), EXEC (its evidence gate is the
  post-LANE re-profile), JIT (the sync subset defines the compilable subset; the lane-escalation
  seam is the natural native↔interpreter boundary).
- **Engines:** **VM only.** The tree-walking interpreter is UNTOUCHED — it remains the permanent
  byte-identity oracle (`goal.md` pillar 1). Both VM lanes must equal it, always.
- **Breaking:** **no.** No syntax change, no semantics change, no opcode change, no `.aso` change
  (`ASO_FORMAT_VERSION` untouched). Runtime-only: a second *driver* over the same `Fiber` state.

---

## 0. Read this first — the one-sentence design

The VM's execution state already lives **entirely outside the Rust call stack**: a
[`Fiber`](../../src/vm/fiber.rs) is `{ frames: Vec<CallFrame>, stack: Vec<Value>, state }`, each
`CallFrame` carries its own `ip` (`src/vm/fiber.rs:20,71`), and the async `run_loop`
(`src/vm/run.rs:1088`) is *just a driver* polling that state. Therefore a second driver — a
**plain, non-async `run_loop_sync`** executing the suspension-free opcode subset in a tight loop —
can interleave freely with the async one: **lane-switching is choosing which driver polls the
fiber.** No on-stack replacement, no deopt metadata, no state reconstruction — the fiber *is* the
state machine, and both drivers execute the same opcodes through the same shared helpers over the
same fiber, so byte-identity holds by construction wherever code is shared and is *proven* by the
differential + fuzzer wherever it is transcribed (§6).

Everything else in this document is making that sentence precise, conservative, and provable.

## 1. Summary & motivation (the measured evidence)

`bench/PROFILING_RESULTS.md` (Phase-0 profiling, 2026-06-06) measured where time actually goes:

| workload | dominant cost | VM dispatch share | VM/TW |
|---|---|---:|---:|
| `async_inline` (400k trivial async calls) | **async runtime 78%** (kevent/reactor park 55%, tokio abort+ref_dec+notify+SharedFuture ~12%) | 9% | 1.09× |
| `async_concurrent` (200k gathers ×4) | **async runtime 71%** (kevent 49%, SharedFuture::get 5%) | 5% | 1.23× |
| `object_churn` (tight loop) | dispatch/VM 49% (run_loop 18%, `Fiber::frame` 9%, push/pop 6%) | 49% | 2.52× |
| `workflow_loop` | **fsync 96%** | <1% | 1.01× |

The structural cause: **`run_loop` is an `async fn`** — every instruction of every program
executes inside the tokio machinery (the loop body is a compiler-generated state machine; every
call-path re-entry is a boxed, `async_recursion` future; `Vm::call_value` wraps each re-entry in
`grow_future(self.run(&mut fiber))`, `src/vm/run.rs:4568`). Yet the overwhelming majority of
opcodes — arithmetic, locals, jumps, property access, comparisons, plain synchronous calls — **can
never suspend**. A program should pay the async tax only at genuine suspension points.

LANE introduces exactly that: a synchronous dispatch driver for the never-suspends subset, with
the existing async `run_loop` demoted to an **orchestrator** that bursts into the sync lane and
takes over only at the ops that can actually suspend. Plus the single cheapest async win the
profiling identified: **`await` on an already-completed future takes the value inline** — no boxed
`get()` future, no `Notify`, no leaving the sync lane (§4).

What LANE deliberately does **not** attempt (recorded so expectations stay honest, §8):
- It does not change *when* async bodies run. Eager `spawn_local` scheduling is untouched, so an
  `await` of a **just-spawned, not-yet-run** task is still a pending await that suspends through
  the scheduler — `async_inline`'s exact shape. The scheduler/reactor residual is **EXEC**'s
  evidence-gated territory; LANE's post-merge re-profile is what opens (or closes) EXEC's gate.
- It does not touch allocation (`workflow_loop` fsync, `json_roundtrip` malloc) — WARM/CALL/SHAPE
  territory.

## 2. The two-driver mechanism

### 2.1 What exists today (grounded)

`Vm::run_loop` (`src/vm/run.rs:1088`) loops: capture `fault_ip` → refresh `last_fault_source`
(SP4 §3 panic provenance, `run.rs:1092–1096`) → decode `Op::from_u8(code[ip])` → advance `ip` past
the opcode + `op.operand_width()` operands (`run.rs:1101–1103`, `src/vm/opcode.rs:788`) → one
giant `match op`. The arms divide cleanly:

- **Suspension-free arms** (the overwhelming majority): pure fiber/stack manipulation delegating
  to helpers *shared with the tree-walker* — `eval_binop_adaptive` → `apply_binop`
  (`run.rs:1181`), `interp::index_get`/`index_set` (`run.rs:2538,2554`), `vm_read_member` /
  `ic_get_field` (`run.rs:2563–2612`), `vm_set_prop` (`run.rs:3600`), `materialize_range*`,
  the match/destructure family, `return_from_frame` (`run.rs:5795`). **None of these contain an
  `.await`.** This includes the **plain synchronous closure call**: the `Op::Call`
  `Value::Closure` arm (non-async/non-worker/non-generator, `run.rs:1757–1827`) pops args, runs
  the shared `check_call_args`, allocates cells, pushes a `CallFrame`, bumps `call_depth` once
  via `enter_frame_depth` (`run.rs:616,1807` — SP3 §B), and publishes profiler frames
  (`run.rs:1823`) — *no await; the loop just continues in the new frame.* `Op::Return` pops the
  frame via `return_from_frame`, which decrements the depth and publishes frames (`run.rs:5822,
  5826`) — also no await.
- **Suspending / async-machinery arms** (the complete inventory, verified by grepping every
  `.await` between `run.rs:1088` and the end of the loop): the `Op::Call`/`Op::CallSpread`
  non-plain-callee paths (worker-stream spawn `run.rs:1652–1655`; async-closure eager
  `spawn_local` + `maybe_yield_for_inflight().await` `run.rs:1743–1755`; native/builtin callee →
  `call_value(..).await` `run.rs:1843`), `Op::CallNamed`/`Op::CallNamedSpread` (variant
  construction, `run.rs:1893–1897`), `Op::CallMethod`/`Op::CallMethodSpread`
  (`dispatch_method(..).await`, `run.rs:2096–2097`, `dispatch_method` at `run.rs:4859`),
  `Op::Import` (`load_file_module(..).await`, `run.rs:2821–2825`), `Op::Await`
  (`f.get().await`, `run.rs:3439`), `Op::IterNext` (`g.resume(..).await` / native stream call,
  `run.rs:3504,3543`), and `Op::Break` (the DBG trap — parks on the command channel and may
  `eval_in_paused_frame(..).await`, `run.rs:3905,3995`).

So the lane split is not speculative — it is reading the existing structure back out of the code.

### 2.2 `run_loop_sync` — the sync driver

A new **plain (non-async) method** on `Vm`:

```rust
/// LANE: the outcome of one synchronous dispatch burst.
pub(crate) enum SyncOutcome {
    /// The fiber finished: `RunOutcome::Done(v)` (root frame returned) or
    /// `RunOutcome::Yielded(v)` (a generator fiber hit `Op::Yield`).
    Finished(RunOutcome),
    /// The burst stopped at an op outside the sync subset (or an `Op::Await`
    /// whose operand is a pending future). Fiber state is EXACT and `ip` still
    /// points AT the escalating opcode byte — the async driver re-decodes it.
    NeedsAsync,
}

fn run_loop_sync(&self, fiber: &mut Fiber) -> Result<SyncOutcome, Control>;
```

The body mirrors `run_loop`'s structure exactly — capture `fault_ip`, refresh
`last_fault_source` per instruction *identically* to `run.rs:1092–1096` (no hoisting in v1; the
per-frame-constant optimization is a recorded follow-up that needs its own
observation-equivalence argument), decode the op — **but checks subset membership BEFORE
advancing `ip`**:

- If `op` is **not** in the sync subset (§3), return `Ok(SyncOutcome::NeedsAsync)` with `ip`
  untouched (still at `fault_ip`). The async driver's next iteration re-reads `code[ip]`, decodes
  the same op fresh, advances, and executes it on its unchanged arm. This is the entire handoff
  protocol — there is no other lane-switch state.
- If `op` is `Op::Await`, attempt inline completion (§4); a pending future is treated as
  "not in the subset" (return `NeedsAsync`, `ip` untouched, the future still on TOS — the
  inline check is read-only, so nothing was consumed).
- Otherwise advance `ip` and execute the op. Substantive arms call the **same shared helpers**
  the async arms call (`eval_binop_adaptive`, `index_get`/`index_set`,
  `ic_get_field`/`vm_read_member`, `vm_set_prop`, `check_call_args`, `return_from_frame`,
  `materialize_range*`, …); trivial arms (push/pop/jump/local) are transcribed. Where today's
  async arm holds non-trivial logic **inline** — the plain-closure call body — that logic is
  first **extracted into one shared helper** (`push_closure_frame`, plan Task 3) used verbatim
  by both drivers, so frame push, `enter_frame_depth`, cell allocation, and the profiler publish
  exist exactly once.
- `Op::Return` / `Op::Propagate` route through the shared `return_from_frame`; a root-frame
  return yields `Finished(RunOutcome::Done(v))`. `Op::Yield` sets `FiberState::Suspended` and
  yields `Finished(RunOutcome::Yielded(v))` — exactly the async arm (`run.rs:3446–3459`).
- Errors are ordinary `Err(Control)` propagation, anchored with the same `panic_at(fiber,
  fault_ip, ..)` / span helpers — the message and span construction is the same code.

The sync driver is **a loop, not recursion**: a script call pushes a `CallFrame` on the fiber
(native stack depth unchanged), and the recursion-depth guard is the same `call_depth` `Cell`
incremented **exactly once per logical call** inside the shared `push_closure_frame` (SP3 §B —
the invariant the plan tests explicitly). Native-stack re-entry (`call_value` → `run`) only
happens on the async driver, where `stacker::grow_future` already guards it (`run.rs:4568`).

### 2.3 The orchestrator

`run_loop` keeps its full match — **every arm unchanged** — and gains one block at the top of its
loop:

```rust
async fn run_loop(&self, fiber: &mut Fiber) -> Result<RunOutcome, Control> {
    loop {
        // LANE: burst through the suspension-free subset on the sync driver.
        // `sync_lane == false` (the kill switch) skips straight to the
        // pre-LANE async dispatch below — the permanent diagnostic mode.
        if self.sync_lane {
            match self.run_loop_sync(fiber)? {
                SyncOutcome::Finished(outcome) => return Ok(outcome),
                SyncOutcome::NeedsAsync => {} // fall through: async-execute ONE op
            }
        }
        let fault_ip = fiber.frame().ip;
        /* ... the existing loop body, byte-for-byte ... */
    }
}
```

Properties this shape buys:

- **The orchestrator is the only caller of `run_loop_sync`.** A fiber is only ever driven by
  `run_loop` (program root, `call_value` re-entry, generator `resume_vm`, paused-frame eval) —
  all of which funnel through `Vm::run` → `run_loop`. There is no second entry point to the sync
  driver, so "a fiber resumed by the wrong driver" is structurally impossible: the async driver
  *contains* the sync driver. (CALL will later add a second, carefully-scoped caller for the
  trampoline; that is CALL's correctness burden, recorded here as the seam's contract: *callers
  of `run_loop_sync` must treat `NeedsAsync` by handing the fiber to `run_loop`, never by
  retrying.*)
- **Default mode:** the async match is reached only at escalation ops, executes exactly one of
  them, and loops back into a burst. With `sync_lane == false` every instruction takes the
  pre-LANE path (plus one predictable branch per iteration — the same cost class as the existing
  `self.specialize` checks; proven free by the Gate-12/17 re-runs).
- **Generators get the lane for free:** `GeneratorHandle::resume_vm` drives `Vm::run` on the
  generator's own fiber, so a generator body bursts exactly like any other code; `Op::Yield`
  ends the burst as `Finished(Yielded)`. Generator *resume from the consumer side* stays an
  escalation (`Op::IterNext`, `gen.next()` method dispatch), as required.

## 3. The sync subset — conservative, op-classified, runtime-escalated

v1 classifies **per opcode** (and for `Op::Call`, per *callee kind* via a read-only `peek` at the
callee stack slot before any pop). This is deliberately a coarse, conservative allowlist —
"definitely cannot suspend" — not an effect analysis of function bodies (rejected for v1, §9).

**IN the sync subset (executed by `run_loop_sync`):**

| group | ops |
|---|---|
| consts / stack | `Const` `Nil` `True` `False` `Pop` `Dup` `Swap` `Rot3` `Template` |
| arithmetic / compare (shared `apply_binop`) | `Add Sub Mul Div Mod Pow Lt Le Gt Ge Eq Ne InstanceOf BitAnd BitOr BitXor Shl Shr WrapAdd WrapSub WrapMul Range` + `Neg Not BitNot` + `InstanceOfType` |
| ranges | `RangeInclusive` `RangeStepValue` `RangeResolveStep` `RangeHasNext` `CheckNumbers` |
| locals / globals / cells / upvalues | `GetLocal` `SetLocal` `GetGlobal` `DefineGlobal` `SetGlobal` `ImmutableError` `GetLocalCell` `SetLocalCell` `FreshCell` `GetUpvalue` `SetUpvalue` `CheckParam` `CheckLocal` `JumpIfArgSupplied` |
| jumps | `Jump` `Loop` `JumpIfFalse` `JumpIfTrue` `JumpIfNotNil` |
| literal / arg builders | `NewArray` `NewObject` `NewMap` `MapEntry` `Spread` `SpreadArgs` `AppendArray` `AppendObject` `SpreadObject` `AppendNamedArg` `AppendPosArg` `AppendSpreadArg` `ArrayLen` |
| member / index access | `GetIndex` `SetIndex` `GetProp` `GetPropOpt` `SetProp` `GetSuper` |
| destructure / match | `CheckArrayDestructure` `CheckObjectDestructure` `ArrayElem` `ObjectKey` `ArrayRest` `ObjectRest` `MatchArray` `MatchObject` `MatchHasKey` `MatchVariant` `MatchVariantArity` `MatchVariantHasField` `VariantElem` `VariantField` `MatchRange` `MatchNoArm` |
| closures & sync iteration setup | `Closure` `IterSnapshot` |
| calls (conditional) | `Call` / `CallSpread` **only when the peeked callee is a plain `Value::Closure`** (`!is_async && !is_worker && !is_generator`) → the shared `push_closure_frame`. Any other callee kind: escalate. |
| unwind / terminals | `Return` `Propagate` (both **only while `frame.defers` is empty** — see "DEFER coordination" below) `Unwrap` `Yield` |
| defer registration (DEFER) | `DeferPush` `DeferPushMethod` — each only pops already-evaluated values and pushes a bound thunk onto `frame.defers`; no call, no await |
| await (conditional) | `Await` — non-future operand (identity) or a **resolved** future (§4). Pending future: escalate. |

**ESCALATION ops (handled only by the async driver; the sync driver returns `NeedsAsync` at an
un-advanced `ip`):**

- `Call`/`CallSpread` with a non-plain callee — async closure (eager `spawn_local` + the
  `maybe_yield_for_inflight` backpressure point), `worker fn` / `worker fn*` dispatch, generator
  construction, and every native/builtin/class/bound-method callee (which routes through the
  async `call_value`, `run.rs:4462` — see "stdlib classification" below).
- `CallNamed`, `CallNamedSpread` (variant construction awaits, `run.rs:1896`).
- `CallMethod`, `CallMethodSpread` — **all method calls escalate in v1**, including the IC
  in-frame fast path inside `dispatch_method`. This is deliberate scope control: for a sync
  method the `dispatch_method(..).await` completes within the same poll (no scheduler
  round-trip), so escalation costs exactly what the call costs today — no regression, no win.
  Lifting the IC frame-push fast path into the lane is **CALL**'s first deliverable (it depends
  on LANE and owns the call-path diet).
- `Import` (module load I/O).
- `GetIter` / `IterNext` / `IterClose` — the whole `for await` family stays on the async driver
  (its per-step `resume(..).await` dominates; keeping the family together keeps the
  transcription surface small; `IterSnapshot` for *sync* `for..of` stays in-lane).
- `Class`, `DefineInterface`, `DefineExport` — suspension-free but **cold** (once per
  program/module); escalating them shrinks the transcribed surface at zero steady-state cost.
- `Break` — the DBG trap. A debugger/coverage **byte-patch makes that offset an escalation point
  by construction**: the sync driver decodes the patched byte as `Op::Break`, returns
  `NeedsAsync`, and the async driver's existing trap arm does the park/un-patch/re-dispatch
  (`run.rs:3905`). No instrumentation logic enters the sync driver (§5).
- An invalid opcode byte panics with the identical "invalid opcode byte" VM-bug panic as
  `run.rs:1098–1099` (not escalated — same unreachable-by-construction contract).

**DEFER coordination (sequencing decided: the DEFER spec,
`2026-06-12-defer-statement-design.md`, lands BEFORE LANE — LANE rebases over its
frame-exit/unwind drain hooks).** DEFER adds two opcodes and makes frame exit potentially
suspending; the lane classification is:

- `DeferPush` / `DeferPushMethod` are **suspension-free** and stay IN the subset (table above):
  each pops the already-evaluated callee/receiver + args and appends a bound thunk to the
  current frame's `defers` — no call is performed, nothing can await.
- **Frame exit with a non-empty defer stack is an ESCALATION.** Once `defer await` exists,
  `Return`/`Propagate`/unwind become potentially suspending (the drain may drive a future to
  completion). The sync driver's `Return`/`Propagate` arms therefore check
  `frame.defers.is_empty()` first: non-empty → `NeedsAsync` at the un-advanced `ip`, and the
  async driver's drain-then-`return_from_frame` arm (DEFER §5.2) executes unchanged. The unwind
  chokepoint (`Vm::run`, DEFER drain site 3) is async-driver territory by construction — the
  sync driver only propagates `Err(Control)` outward. An EMPTY defer stack — every pre-DEFER
  program, and the vast majority of frames — exits in-lane exactly as specced (`Vec::is_empty`
  on an already-hot frame field; DEFER's own zero-cost-when-unused argument).
- This keeps the load-bearing claim exact: **the sync subset contains no awaits** — with the one
  defer-exit carve-out that a frame whose `defers` is non-empty leaves the lane *before* any
  drain begins, so the potentially-awaiting drain always executes on the async driver.

**Stdlib / native classification (conservative by construction).** v1 does not classify
individual stdlib functions as sync vs async. Every native/builtin callee escalates, because the
only route to one is `Op::Call`'s non-`Closure` arm / `Op::CallMethod` — both escalation ops —
and *execution paths that can await* (HTTP, fs, process, channel ops, `task.*`) live exclusively
behind `Interp::call_value` / `call_stdlib`, which are `async fn`s. A precise sync-allowlist for
native fns (so e.g. `math.abs` doesn't break a burst) is a recorded follow-up that belongs to
CALL's trampoline work, not v1.

**`maybe_yield_for_inflight` — the fairness analysis (verified, stated precisely).** The
cooperative yield (`src/interp.rs:1283–1287`) yields **only when `inflight ≥
INFLIGHT_YIELD_CAP` (= 256, `src/interp.rs:651`)** — not merely when nonzero — and it is called
**only from async-task spawn sites**: the tree-walker's three spawn paths
(`src/interp.rs:5339,5923,5995`) and the VM's async-closure call arm (`run.rs:1754`),
`dispatch_method`'s async-method branch, and `invoke_compiled_static`'s async branch
(`run.rs:5715`). **Every one of those VM sites lives inside an escalation op's handler.** The
sync subset therefore contains *no spawn sites and no yield points at all* — so the sync driver
needs no inflight check: the backpressure yield always executes on the async driver, exactly
where and when it executes today. And a sync burst starves concurrent tasks *exactly as much as
today's `run_loop` does on the same opcode sequence*: an `async fn` only yields at `.await`
points, and the suspension-free arms contain none — the burst's non-yielding stretch is the same
stretch the async loop already runs without yielding. (The brief's "escalate when the in-flight
counter is nonzero" is *stronger than needed and not what the code does*; the precise invariant
is the one above, and the plan adds a guard test: a program with >256 in-flight un-awaited tasks
behaves byte-identically lane-on vs lane-off.)

**Determinism seams (SP9) — verified unaffected.** Every det-routed operation (RNG:
`math.random`/`uuid`/`crypto.randomBytes`; clock: `time.*`/`date.*`; FFI record/replay) is a
stdlib call, reached only through the escalating call ops → the async driver →
`Interp::call_stdlib`, the same chokepoint as today. Nothing in the sync subset consults
`Interp.determinism`. Record/replay runs are part of the differential corpus already
(workflow examples); they stay byte-identical by construction.

## 4. Inline ready-future completion

### 4.1 Mechanism

`SharedFuture` (`src/task.rs:106`) gains a non-consuming, non-blocking probe:

```rust
impl ResultCell {
    /// LANE: non-blocking probe. `Some(result)` iff already resolved. Read-only:
    /// never notifies, never consumes, never touches the abort handle.
    fn try_get(&self) -> Option<Result<Value, Control>> {
        self.0.slot.borrow().as_ref().cloned()
    }
}
impl SharedFuture {
    pub fn try_get(&self) -> Option<Result<Value, Control>> { self.0.cell.try_get() }
}
```

The sync driver's `Op::Await` handling (peek-first, so escalation leaves the stack exact):

1. Peek TOS. **Not a `Value::Future`** → pop and push back (identity, `await 5 == 5`) — stays
   in-lane, matching the async arm's `other => fiber.push(other)` (`run.rs:3442`).
2. A `Value::Future` whose `try_get()` is `Some(Ok(v))` → pop the future, push `v`. `Some(Err(c))`
   → pop the future, return `Err(c)` — the panic/propagation stored in the future surfaces with
   the *identical* `Control` value the async arm's `f.get().await?` would re-raise.
3. `None` (pending) → return `NeedsAsync`, `ip` un-advanced, future still on TOS. The async
   driver re-executes the whole `Op::Await` arm: `f.get().await` parks, the scheduler runs ready
   tasks, the waiter wakes on resolve — today's path, byte-for-byte.

### 4.2 Why this cannot change observable interleaving (the load-bearing argument)

**Awaiting an already-resolved future is *already synchronous* in today's engine.**
`ResultCell::get` (`src/task.rs:63–81`) checks the slot *before* constructing or awaiting the
`Notify` future: if the slot is filled, `get()` returns on its **first poll without ever
suspending** — no reactor registration, no yield to the scheduler, no task switch. So the inline
take replaces a completes-in-one-poll async call with a plain function call returning the same
cloned `Result` — it removes constant-factor machinery (the boxed `get()` future construction,
the poll plumbing, the async-fn state-machine traffic) and keeps the burst unbroken, while the
*scheduling trace is identical by construction*: in neither world does an await-of-resolved
yield control.

Enumerated edge cases, each verified identical:

- **Error-carrying future** (`resolve(Err(Control::Panic/Propagate))`): `try_get` clones the same
  `Result<Value, Control>`; the `?`-shaped re-raise out of the driver is the same `Control` value
  taking the same propagation path through `run` (`run.rs:1057`).
- **First-writer-wins / multi-waiter** (`race`): `try_get` is read-only — it neither resolves nor
  notifies; other waiters observe the identical stored result.
- **Cancel-on-drop:** the await pops its `Value::Future` handle clone in both worlds (one `Rc`
  decrement at the same program point); `try_get` never touches the `AbortHandle`.
- **Detached / taskless futures** (`task.spawn`, `SharedFuture::resolved`): no abort handle to
  interact with; same read.
- **What is deliberately NOT inlined — the exclusion list:** a **pending** future (the eager
  spawn means a just-spawned body has not run; inlining anything there would require *running*
  the task out of scheduler order — forbidden, escalate); `task.gather`/`race`/`timeout`
  internals (stdlib calls → escalation ops, untouched); generator `resume` (its own
  driver); anything reached via `Interp`'s tree-walker `Await` (the oracle is untouched).

**Honest reach (consumed by §8):** the awaits this hits are those on futures *resolved by the
time the await executes* — gather tails after the first await parks, futures stored and awaited
later, re-awaited futures, awaits after intervening suspension. `async_inline`'s
spawn-then-immediately-await is **pending by construction** at its first await and keeps the
scheduler round-trip; how much of the measured 71–78% moves is exactly what the Task-9 A/B and
post-LANE re-profile answer, and the residual is EXEC's documented gate.

## 5. Instrumentation parity (DBG/DX)

The DBG seam is `Vm.instrument: RefCell<Option<Box<Instrumentation>>>` (`run.rs:197`;
`src/vm/instrument.rs:36`), consulted at exactly three places. Parity per place:

1. **Profiler frame publish** (`publish_profile_frames`, `run.rs:663` — a single `None`-check
   when off) fires at frame push/pop. Both sites live in code the lanes **share**:
   `push_closure_frame` (the extracted plain-call helper, plan Task 3) and `return_from_frame`
   (`run.rs:5826`). The sync driver therefore performs *the identical publishes at the identical
   program points* — there is no second implementation to drift.
2. **Breakpoints** (runtime-patched `Op::Break` bytes through the `UnsafeCell`-backed `Code`):
   the sync driver reads `chunk.code[ip]` through the same `Code` accessor, so a patched byte is
   observed at the next decode of that offset exactly as today (single-threaded; no new
   visibility question). A patched byte decodes as `Op::Break` → escalation → the async trap arm
   parks/un-patches/re-dispatches unchanged (`run.rs:3905`). After the un-patch, the orchestrator
   bursts again and the sync driver executes the restored original op. Debugger interaction never
   enters the sync driver.
3. **Coverage traps** (DX): same byte-patch mechanism, same escalation, same cold-arm handling
   (`run.rs:3938–3954`). Each line still traps at most once, then runs free — in the sync lane.

**Gate 17 obligation:** `tests/vm_bench.rs dbg_zero_cost_gate` (`vm_bench.rs:499`) is re-run by
this spec (it touches the dispatch loop and the call path); the armed-idle/none geomean bound
(≤1.05×) and the spec/tw ≥2× floor must both hold with the lane ON.

## 6. Correctness — kill switch, differential modes, fuzz axis, coverage assertion

### 6.1 The permanent kill switch

A new `Vm.sync_lane: bool` field, mirroring `specialize` (`run.rs:117`) in every respect:

- **Default `true`** (the production engine). `false` forces the pure async driver — the
  pre-LANE dispatch path, instruction for instruction.
- **CLI seam:** `ASCRIPT_NO_SYNC_LANE=1` (mirroring `ASCRIPT_NO_SPECIALIZE`, `src/lib.rs:2067`).
  Read at `Vm` construction so **worker isolates inherit it automatically** (each isolate builds
  its own `Vm` in its own process-wide env). Tests use the explicit constructor/setter, never the
  env (parallel-test hygiene).
- **Test entry point:** `vm_run_source_no_sync_lane` in `src/lib.rs`, alongside
  `vm_run_source_generic` / `vm_run_source_armed_idle` (`lib.rs:2240–2266`), threading a flag
  through `vm_run_source_cfg`.
- **Permanent**, not bring-up scaffolding (campaign Gate 15). `sync_lane` and `specialize` are
  orthogonal: the sync driver honors `self.specialize` inside the shared helpers exactly as the
  async driver does (the IC/adaptive guards are *inside* `eval_binop_adaptive`/`ic_get_field`),
  so generic-VM × sync-lane composes with no special casing.

### 6.2 Differential modes (Gate 1 + Gate 15)

The standing identity grows a mode. `tests/vm_differential.rs` asserts, over the expression
battery, the program battery, the goldens, and the whole-`examples/**` corpus, in **both feature
configs**:

> tree-walker == specialized-VM(lane ON, the default) == specialized-VM(lane OFF)
> == generic-VM(lane ON)

"Lane forced" needs no extra knob: the lane has no warm-up threshold — `sync_lane == true`
*always* bursts first, so the default mode IS the forced mode (every sync-subset op on the corpus
executes in the lane; the coverage assertion below proves it). The tree-walker is never relaxed;
a lane-on/lane-off divergence is a transcription or handoff bug in the sync driver — fix the
driver, never the assertion.

### 6.3 The fuzz axis (same PR)

`fuzz/fuzz_targets/differential.rs` (and the in-suite
`tests/property.rs::three_way_differential_over_generated_programs` battery) gains the fourth
projection: `vm_run_source_no_sync_lane`. Any disagreement between lane-on and lane-off on a
generated program is a libFuzzer crash with the program attached — the continuous guard on the
transcribed arms. Landed in the same PR as the driver (campaign Gate 15).

### 6.4 The coverage assertion (the anti-false-green rule)

A lane that silently escalates everything would pass every differential while executing zero
instructions in the lane — the JIT spec's false-green trap (JIT spec §5.1), applied here. So the
lane is **counted and asserted**:

- `Vm` gains `lane_sync_ops: Cell<u64>` and `lane_bursts: Cell<u64>`. The sync driver counts
  retired instructions in a **burst-local `u64`** (a register increment) and flushes to the
  `Cell`s once per burst exit — no per-instruction `Cell` traffic, no flag check. Cost is
  asserted away by the Gate-12/17 re-runs; if it ever measures, the fix is `cfg`-gating the
  counters, never relaxing the bench.
- A `#[doc(hidden)]` test entry (`vm_run_source_lane_stats`) returns `(output, exit,
  sync_ops, bursts)`. `vm_differential.rs` asserts: (a) on the corpus run with the lane on,
  the **aggregate sync-lane instruction count is > 0 and is reported** (count + share printed,
  so a collapse is visible in CI logs); (b) a focused tight-loop program retires ≥ 1,000,000
  ops in the lane (per-program floor — the burst genuinely ran the loop); (c) with
  `sync_lane == false`, `lane_sync_ops == 0` (the kill switch genuinely kills).

### 6.5 Invariants carried over (each gets a regression test in the plan)

- **`call_depth` exactly once per logical call** (SP3 §B): the increment lives only in the shared
  `push_closure_frame` / `enter_frame_depth` and the existing re-entry guards; deep recursion on
  the lane panics `maximum recursion depth exceeded` byte-identically, lane-on vs lane-off vs
  tree-walker.
- **No Rust recursion in the sync driver** (it is a loop; frames go on the fiber).
- **No `RefCell` borrow held across an await**: trivially preserved — the sync driver contains no
  awaits (the §3 defer-exit carve-out escalates BEFORE any drain, so this remains literally
  true); its `instrument`/`last_fault_source` borrows are scoped exactly as today's.
- **Redeclaration/const-immutability runtime timing**, capacity errors, and every Tier-2 panic
  message: unchanged because the raising code is the same shared helpers (the corpus + panic
  batteries enforce).

## 7. Workers, REPL, `.aso`

- **Workers:** each isolate's `Vm` gets both lanes independently (the field is per-`Vm`; nothing
  crosses the airlock — `SyncOutcome` is `pub(crate)` and never serialized). The env kill switch
  is process-wide, so a denied lane is denied everywhere, uniformly.
- **REPL:** the persistent session `Vm` bursts per submitted chunk; no change (cross-line
  persistence is `user_globals`, orthogonal).
- **`.aso`:** untouched. The lane is a runtime driver over the same `Chunk`; `ASO_FORMAT_VERSION`
  does not move; `src/vm/verify.rs` unchanged; an `.aso`-loaded program bursts identically
  (`vm_differential`'s `.aso` mode covers it).

## 8. Performance — expectations stated, results measured (Gates 12, 16, 17, 18)

**Task 0 (campaign Phase 0, executed in THIS plan before any engine change):** `bench/profiling/`
gains the three missing workloads — `func_pipeline.as` (map/filter/reduce over realistic records:
the higher-order-callback constant factor), `call_heavy.as` (small-function-call dominated), and
`server_request.as` (request-shaped glue: parse → route → build → stringify, deterministic and
socket-free so it joins the run-to-completion corpus) — plus **`bench/ab.sh`**, a same-session
A/B harness (two binaries, interleaved runs, per-workload medians + geomean + peak RSS via
`/usr/bin/time -l`). A re-baseline is appended to `bench/PROFILING_RESULTS.md` as a dated
section. Every subsequent spec's headline number runs through this harness (campaign Gate 16).

**Expectations (honest, not promises):**

- **Should move sharply:** the *dispatch* baseline on suspension-free code — `object_churn`,
  the vm_bench compute corpus, `call_heavy` (plain-closure calls never leave the lane), and
  `func_pipeline` partially (per-element callback re-entry is CALL's job; the callback *bodies*
  burst). Mechanism: a plain loop instead of an async state machine — better register
  allocation, no poll plumbing, no `async_recursion` boxing on the burst path.
- **Should move meaningfully:** `async_concurrent` — gather-tail awaits land on resolved futures
  → inline completion; bursts between suspension points shed the per-op async overhead.
- **Bounded, measured, not promised:** `async_inline` — its single await is pending by
  construction under unchanged eager scheduling (§4.2); the spawn + scheduler round-trip
  remains. The post-LANE re-profile of the async corpus is a **required deliverable**: it either
  opens EXEC's gate (residual async tax ≥15%) or closes EXEC with evidence.
- **Will NOT move (do not claim it):** `workflow_loop` (96% fsync — WARM's group-commit),
  allocation-bound `json_roundtrip` slices (SHAPE/NANB/CALL), method-dispatch-bound code (v1
  escalates `CallMethod` — parity, no win, by design; CALL lifts it).

**Gate obligations:** (12/17) spec/tw bench geomean ≥2× holds at merge; `dbg_zero_cost_gate`
re-run green; lane-off vs pre-LANE-baseline shows no regression (the orchestrator branch is
noise). (16) every headline number is a same-session A/B recorded in `bench/LANE_RESULTS.md`,
measured with the shipped profiler where possible. (18) peak RSS on the corpus reported alongside
time; a memory regression is a bug.

## 9. Scope & rejected alternatives

**In scope:** `run_loop_sync` + `SyncOutcome` + the orchestrator burst; the §3 subset with
per-op/per-callee runtime escalation; `SharedFuture::try_get` + inline ready-future completion;
the `push_closure_frame` extraction; `sync_lane` kill switch (env + test entries); lane
counters + coverage assertion; differential mode + fuzz axis; Gate-12/16/17/18 measurement
artifacts; Task-0 bench corpus + A/B harness.

**Out of scope / non-goals:** EXEC (executor replacement — separate, evidence-gated spec; LANE's
re-profile is its gate input); method-call and higher-order-callback lane entry (CALL); native-fn
sync classification (CALL follow-up); pre-decoded dispatch (DECODE); any opcode/`.aso`/semantics
change; async-fn/generator scheduling changes; the tree-walker (untouched, permanently).

**Rejected:**

- **Full effect-classification of `FnProto`s** (compute per-function "can this body suspend?"
  and run whole call trees natively-sync): rejected for v1 in favor of per-op runtime
  escalation. The analysis is a whole-program fixpoint over a late-bound, dynamically-dispatched
  call graph (globals re-bindable at runtime; method receivers unknowable statically) — it would
  need its own invalidation machinery for a win the per-op escalation already captures (a burst
  simply continues *through* a plain call and stops at the first genuinely-async op). DECODE/JIT
  may revisit with profile data.
- **Rewriting `run_loop` as an explicit state machine** (enum-of-resume-points, hand-rolled
  poll): rejected — **the fiber already is the state machine** (`frames`/`ip`/`stack` externalize
  everything; that is why two drivers work at all). A hand-rolled rewrite re-derives what exists
  and re-proves the entire differential for zero additional capability.
- **Running ready async tasks inline at a pending await** ("help the scheduler" at `Op::Await`):
  rejected — it reorders task execution relative to the eager-spawn model and is observably
  distinguishable (task side-effect ordering). Scheduling changes are EXEC's gated territory.
- **A sync-lane warm-up threshold** (burst only when hot): rejected — the lane is
  cheaper-or-equal per op from the first instruction; a threshold would only shrink coverage and
  complicate the forced-mode differential.
- **Zero-duplication via routing the async loop's sync arms through one shared `step()` fn**:
  considered, rejected for the kill switch's sake — `--no-sync-lane` must preserve the *shipped
  pre-LANE code path* (the same reason `--no-specialize` keeps the generic path physically
  separate). Substantive logic is shared via helpers (the meat); only thin glue is transcribed,
  and the §6 differential modes + fuzz axis exist precisely to police that glue.

## 10. Grounding (verified file:line, 2026-06-12 — re-grep before relying on these)

- `src/vm/run.rs` — `run:1057`, `run_loop:1088`, per-instruction `last_fault_source`
  refresh `:1092–1096`, decode + ip-advance `:1097–1103`, shared-binop arm + `eval_binop_adaptive`
  `:1142–1183`, `Op::Call/CallSpread:1570` (worker-stream await `:1652–1655`; async-closure
  spawn_local + `maybe_yield` `:1743–1755`; **plain-closure frame push `:1757–1827`** with
  `enter_frame_depth:1807`, `frames.push:1808`, `publish_profile_frames:1823`; native-callee
  `call_value(..).await:1843`), `Op::CallNamed:1849` (await `:1896`), `Op::CallMethod:2056`
  (`dispatch_method(..).await:2096–2097`), `Op::GetIndex:2529`/`SetIndex:2542` (shared
  `index_get`/`index_set`), `Op::GetProp:2563` (IC + `vm_read_member`), `Op::Import:2775`
  (awaits `:2821–2825`), `Op::Return:3345`, `Op::Propagate:3359`, `Op::Unwrap:3394`,
  `Op::Await:3426` (`f.get().await:3439`), `Op::Yield:3446–3459`, `Op::GetIter:3462`,
  `Op::IterNext:3487` (awaits `:3504,3543`), `Op::SetProp:3600` (shared `vm_set_prop`),
  `Op::Class:3629`, `Op::Break:3905` (coverage trap `:3938–3954`, debug park + awaited eval
  `:3981–3995`), `call_value:4462` (`grow_future(self.run(..)):4568`), `dispatch_method:4859`,
  `invoke_compiled_static` async branch + `maybe_yield:5715`, `enter_frame_depth:616`,
  `leave_frame_depth:635`, `publish_profile_frames:663`, `return_from_frame:5795`
  (depth dec `:5822`, publish `:5826`), `specialize:117`, `with_specialize:231`,
  `instrument:197`.
- `src/vm/fiber.rs` — `CallFrame` (per-frame `ip`) `:20`, `alloc_cells:56`, `Fiber {frames,
  stack, state}:71` (ALL execution state externalized — the design's load-bearing fact).
- `src/task.rs` — `CellInner:33`, `ResultCell::get` **resolved-slot fast path returns before any
  await** `:63–81` (the §4.2 identity argument), `SharedFuture:106`, `resolved:119`,
  `get:132`, `set_abort:150`, `detach:157`.
- `src/interp.rs` — `INFLIGHT_YIELD_CAP = 256` `:651`, `inflight_guard:1270`,
  `maybe_yield_for_inflight:1283–1287` (yields only at ≥ cap), tree-walker spawn-site callers
  `:5339,5923,5995`.
- `src/vm/opcode.rs` — `Op:29`, `operand_width:788`. `src/vm/value_ext.rs` —
  `RunOutcome:49`, `FiberState:56`.
- `src/lib.rs` — `ASCRIPT_NO_SPECIALIZE` CLI seam `:2066–2067`, `vm_run_source_with:2240`,
  `vm_run_source_armed_idle:2252`, `vm_run_source_cfg:2269`.
- `src/vm/instrument.rs` — `Instrumentation {breakpoints, profiler, coverage}:36–42`.
- `tests/vm_bench.rs` — harness + `dbg_zero_cost_gate:499` (Gate-17 artifact).
  `tests/vm_differential.rs` — the oracle harness + whole-corpus gate + skip-list discipline.
  `fuzz/fuzz_targets/differential.rs` — the three-way fuzz projection LANE extends.
- `bench/PROFILING_RESULTS.md` — the Phase-0 evidence; `bench/profiling/run.sh` + `*.as` — the
  harness Task 0 extends. `goal-perf.md` — campaign gates 15–18; `goal.md` — gates 1–14.
- Precedents: `--no-specialize` three-way differential (`run.rs:104–117`); the DBG instrument
  seam + Gate-12 discipline (`superpowers/specs/2026-06-08-debugger-profiler-design.md`); the
  JIT spec's anti-false-green coverage rule
  (`superpowers/specs/2026-06-08-baseline-jit-design.md` §5.1).
