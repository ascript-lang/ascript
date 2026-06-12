# Bespoke VM Executor (EXEC) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task
> is executed by a **fresh implementer subagent**, then verified by an **independent reviewer
> subagent** that runs the commands and probes edges before acceptance. A final **holistic
> review** covers the whole branch before merge. A task is closed only when every box under it
> is ticked.

> **⛔ EVIDENCE GATE — Phase 0 IS the first task and BOTH of its outcomes are terminal states.**
> This plan executes ONLY if the post-LANE re-profile shows the async-runtime share ≥15% on the
> async corpus (`goal-perf.md`). Task 0 measures and records the verdict. On NO-GO the plan
> STOPS at Task 0 with the evidence committed — that is a successful, complete outcome.

**Goal:** Replace tokio `current_thread`+`LocalSet` as the per-isolate **task driver** (spawn,
wake, cooperative yield) with a purpose-built `!Send` executor — slab tasks, FIFO ready queue,
same-thread wakes that never touch the reactor — while tokio remains the I/O + timer driver
(leaf futures unchanged). Cancel-on-drop / structured concurrency / eager scheduling survive
**byte-identically**, proven by the differential + an async-weighted fuzz axis + the M17 leak
battery. Architecture B from the spec (executor-as-one-tokio-task) is v1.

**Architecture:** `src/exec/` — `Core { slab, ready: VecDeque<TaskId>, injected (std::mpsc),
shared: Arc<Shared>, running, live, spawned_total }`; waker = safe `std::task::Wake` over
`Arc<ExecWaker { id, queued: AtomicBool, shared }>` with a same-thread fast path (thread-local
`CURRENT_EXEC`) and lock-free cross-thread injection; the executor future runs inside today's
unchanged `LocalSet` under `tokio::task::unconstrained` with an `EXEC_TICK_BUDGET` self-yield
for driver liveness. ONE seam `exec::spawn_local` (dual `JoinHandle`/`AbortHandle`:
Bespoke|Tokio) covers EVERY `tokio::task::spawn_local` in `src/`; `ASCRIPT_EXECUTOR=tokio` is
the permanent kill switch. Spec: `superpowers/specs/2026-06-12-vm-executor-design.md` —
**read it fully before any task; its §4 invariant table (S1–S14) is the test contract.**

**Tech stack:** Rust (single binary `ascript`); tokio 1.x current-thread (`Cargo.toml:32`);
`std::task::Wake`, `std::sync::mpsc` (lock-free, `Sender: Sync` on the repo's Rust ≥1.72);
the M17 runtime (`src/task.rs`), both engines (`src/interp.rs`, `src/vm/run.rs`), workers
(`src/worker/`). Tests: `cargo test` BOTH feature configs, `tests/vm_differential.rs`,
`tests/vm_bench.rs`, `fuzz/fuzz_targets/differential.rs` + `tests/property.rs`, Miri on
`src/exec`, `bench/ab.sh` + `bench/profiling/`.

**Binding execution standards (non-negotiable):**
- TDD per task: failing test → minimal code → green → commit. Frequent commits, house trailer
  on every commit: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Production-grade mandate (goal.md Gates 1–14):** any bug found while working — ours or
  pre-existing, direct or incidental — is fixed **in this branch** with a failing-test-first
  regression guard. No placeholders, no silent deferrals, no `unwrap()` reachable by input.
- **Scheduling identity is never relaxed:** a bespoke/tokio output divergence is a bug in the
  executor's discipline. Fix the executor, never the assertion, never the corpus.
- Clippy clean AND tests green under `--all-targets` and `--no-default-features
  --all-targets` before any "done" claim. Evidence (command output) before assertions.
- Branch: `feat/vm-executor` off `main`. Merge `--no-ff` after holistic review.

---

## File Structure

**New files:**
- `src/exec/mod.rs` — executor core, waker, seam, handles (split into `core.rs`/`waker.rs`/
  `handle.rs` submodules if mod.rs exceeds ~800 lines).
- `bench/profiling/spawn_wake.as` — spawn/wake microbenchmark workload.
- `bench/EXEC_GATE.md` — the Task-0 gate measurement + verdict (written on GO **and** NO-GO).
- `bench/EXEC_RESULTS.md` — the Task-10 A/B report (GO path only).

**Modified files:**
- `src/lib.rs` — `pub mod exec`; executor installation at every entry future;
  `ASCRIPT_EXECUTOR` seam; test entries `vm_run_source_tokio_exec`, `vm_run_source_exec_stats`.
- `src/task.rs` — `HandleInner.abort: RefCell<Option<exec::AbortHandle>>`; `set_abort`
  signature.
- `src/interp.rs` (5 sites + 2 bridges), `src/vm/run.rs` (2 sites + test helper),
  `src/worker/mod.rs` (4 bridges), `src/worker/isolate.rs` + `src/worker/dispatch.rs`
  (per-isolate install), `src/repl.rs` (session install) — spawn-seam migration.
- `src/stdlib/{task_mod,time_timers,sync,http_server,postgres}.rs`, `src/stdlib/ai/mod.rs` —
  spawn-seam migration + handle-type swaps.
- `tests/vm_differential.rs` — executor differential mode + scheduling battery + coverage
  assertion. `tests/vm_bench.rs` — executor on/off section + gate re-runs.
- `fuzz/fuzz_targets/differential.rs` + `tests/property.rs` — executor projection +
  async-heavy grammar weights.
- `bench/profiling/run.sh` — `spawn_wake` workload. `bench/PROFILING_RESULTS.md` — post-EXEC
  dated section.
- `docs/content/language/modules-async.md` — a short "engine internals" note (executor +
  `ASCRIPT_EXECUTOR`). `CLAUDE.md`, `goal-perf.md`, `superpowers/roadmap.md` — Task 11.

---

## Task 0 — THE GATE: post-LANE re-profile and the go/no-go record

**Precondition:** LANE merged to `main` (verify: `git log main --oneline | grep -i lane`).
Do NOT run this task on a branch that doesn't contain LANE.

**Files:** Create `bench/EXEC_GATE.md`. Modify `goal-perf.md` (status table). NO engine code.

- [ ] **Step 1: Build the profiling binary from current `main`.**
  ```bash
  cd /Users/mahmoud/ascript && git checkout main && git pull
  cargo build --profile profiling
  ```
- [ ] **Step 2: Re-profile the async corpus, same session** (the exact Phase-0 methodology —
  `bench/PROFILING_RESULTS.md` header documents the tooling):
  ```bash
  bench/profiling/run.sh          # runs all workloads incl. async_inline/async_concurrent
                                  # + LANE's func_pipeline/call_heavy/server_request,
                                  # with macOS `sample` attribution via parse_sample.py
  ```
  If LANE's plan already appended its post-LANE dated section to `PROFILING_RESULTS.md` in
  this same session's tree, the numbers may be reused ONLY if produced on this machine; a
  cross-day baseline is forbidden (the SRV MINOR-2 lesson, Gate 16) — when in doubt, re-run.
- [ ] **Step 3: Compute the async-runtime share** per async workload (`async_inline`,
  `async_concurrent`; report `func_pipeline`/`call_heavy`/`server_request` shares for
  context): the sum of kevent/park + timer + tokio task/notify/abort/ref_dec +
  SharedFuture attributions from the sample breakdown, as a fraction of worker-thread
  self-time — the same buckets as `PROFILING_RESULTS.md`'s "CPU attribution" table.
- [ ] **Step 4: Write `bench/EXEC_GATE.md`** — date, machine, commit SHA, the per-workload
  table (share before LANE / after LANE), the threshold (≥15%), and the verdict line:
  `VERDICT: GO (share = NN% ≥ 15%)` or `VERDICT: NO-GO (share = NN% < 15%)`.
- [ ] **Step 5 (BOTH outcomes): update `goal-perf.md`** — EXEC's status row: on GO →
  `🟡 in progress (gate met: NN%, bench/EXEC_GATE.md)`; on NO-GO → `✅ CLOSED by evidence
  (residual NN% < 15%, bench/EXEC_GATE.md)` and move EXEC to the "Removed / parked" section
  with one sentence of justification.
- [ ] **Step 6: Commit** (on a branch + PR if on main per repo rules):
  `docs(exec): record the EXEC evidence-gate verdict (post-LANE re-profile)` + house trailer.
- [ ] **Step 7: STOP HERE on NO-GO.** The plan is complete. On GO: create
  `feat/vm-executor` off `main` and proceed to Task 1.

---

## Task 1: `src/exec/` core — slab, FIFO queue, run/drain (no tokio integration yet)

**Files:** Create `src/exec/mod.rs`. Modify `src/lib.rs` (`pub mod exec;`).

- [ ] **Step 1: Failing tests first** (in `src/exec/mod.rs` `#[cfg(test)]`; these drive the
  core with a hand-rolled noop/recording waker, no tokio):
  - `spawn_is_enqueue_not_run` (S1): spawning a future that sets a flag does NOT set the
    flag until the executor ticks.
  - `fifo_order`: three spawned tasks printing to a shared `Rc<RefCell<Vec<_>>>` run in
    spawn order.
  - `completion_frees_slot_and_bumps_gen`: after a task completes, its slot is on the free
    list and `gen` incremented; a retained stale `TaskId` is rejected by the gen check.
  - `wake_requeues_at_back`: a task that returns `Pending` after self-waking runs again
    AFTER later-queued tasks (FIFO).
  - `tick_budget_yields`: with `EXEC_TICK_BUDGET` tasks ready, one `poll_tick` retires at
    most the budget and reports more-work-pending.
- [ ] **Step 2: Implement the core** — real code, the spec §3.1 shape:

```rust
//! EXEC: the bespoke per-isolate task executor (spec
//! `superpowers/specs/2026-06-12-vm-executor-design.md`). Replaces tokio's task
//! HARNESS (spawn/wake/yield) for AScript tasks; tokio remains the I/O + timer
//! driver. `!Send` like the whole runtime; exactly one per isolate thread.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

/// Polls per executor poll before self-yielding back to tokio so the I/O/timer
/// driver stays live under a self-sustaining wake cycle (spec §2.3). Matches
/// tokio's event interval. Internal tunable — NOT user-visible.
const EXEC_TICK_BUDGET: u32 = 61;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TaskId { index: u32, gen: u32 }

enum Inject { Wake(TaskId), Abort(TaskId) }

struct Shared {
    thread: std::thread::ThreadId,
    injected_tx: std::sync::mpsc::Sender<Inject>,
    /// The executor's parked tokio waker (taken by the first wake; cleared at
    /// tick start). Uncontended std Mutex — deliberately not a hand-rolled
    /// AtomicWaker (spec §3.2: zero unsafe, Miri-trivial).
    parked: Mutex<Option<Waker>>,
}

struct TaskCell {
    future: Option<Pin<Box<dyn Future<Output = ()>>>>,
    waker: Waker,                 // built once per task (Arc<ExecWaker>)
    queued: Arc<AtomicBool>,      // shared with the ExecWaker (dedupe)
    aborted: Cell<bool>,
}

struct Slot { gen: u32, cell: Option<TaskCell> }

pub(crate) struct Core {
    tasks: RefCell<Vec<Slot>>,
    free: RefCell<Vec<u32>>,
    ready: RefCell<VecDeque<TaskId>>,
    injected_rx: std::sync::mpsc::Receiver<Inject>,
    shared: Arc<Shared>,
    running: Cell<Option<TaskId>>,
    live: Cell<usize>,
    spawned_total: Cell<u64>,
}
```
  Core methods this task delivers (each small, each unit-tested): `new()`,
  `insert(future) -> TaskId` (slab insert + push-back + `live`/`spawned_total` bumps),
  `enqueue_local(id)`, `drain_injected()` (non-blocking `try_recv` loop folding
  `Wake`→enqueue-if-gen-valid, `Abort`→`mark_abort`), `poll_one() -> bool` (pop → gen check
  → clear `queued` BEFORE polling → `running` set/clear → on `Ready(())` or pre-marked abort:
  drop future, free slot, `live -= 1`), `poll_tick(budget) -> TickOutcome
  {Idle, MoreWork, AllDone}`. **No `unwrap()` on slab access** — gen mismatches and `None`
  cells are silent skips (wake-after-complete is a no-op by design, spec §3.2).
- [ ] **Step 3:** `cargo test exec::` green; `cargo clippy --all-targets` clean.
- [ ] **Step 4: Commit** `feat(exec): task slab + FIFO ready queue + tick core (EXEC Task 1)`.
- [ ] **Independent review:** reviewer re-runs tests, probes: slab reuse after 10k
  spawn/complete cycles keeps `tasks.len()` bounded; a `TaskId` forged with a huge index
  doesn't panic (bounds-checked skip); FIFO holds under interleaved spawn-from-task.

## Task 2: the waker — same-thread fast path, cross-thread injection, Miri

**Files:** Modify `src/exec/mod.rs`.

- [ ] **Step 1: Failing tests:**
  - `same_thread_wake_pushes_local`: a task pending on a custom one-shot cell; waking it
    from another task (same thread) re-queues it without touching `injected_tx` (assert via
    a test-only injected counter).
  - `cross_thread_wake_lands` : spawn a task pending on a flag; hand its `Waker` to a
    `std::thread`; the thread sets the flag and wakes; the executor (driven by a test loop
    that drains injected) completes the task. Joins the thread (no leak).
  - `wake_after_complete_is_noop` (spec §6.4): retain a completed task's waker; `wake()`
    must not panic, not requeue, not disturb a NEW task occupying the reused slot.
  - `wake_dedupes`: two wakes before the poll yield ONE queue entry.
  - `late_wake_after_executor_drop`: a foreign-thread waker fired (and dropped) after the
    `Core` is gone — send error swallowed, no panic (S-soundness for stray wakers).
- [ ] **Step 2: Implement** `ExecWaker` exactly as spec §3.2 (`std::task::Wake` impl — safe
  code only), plus the thread-local registry:
```rust
thread_local! {
    /// The (at most one) live executor on this isolate thread. Installed by the
    /// entry points via `ExecGuard` (RAII uninstall — a panicking run never
    /// leaves a stale entry).
    static CURRENT_EXEC: RefCell<Option<Rc<Core>>> = const { RefCell::new(None) };
}
```
  Same-thread fast path: `CURRENT_EXEC.with(|c| ...)` push to `ready` + take `parked`;
  fall through to injection when the registry is empty or holds a different executor
  (impossible by the one-per-thread invariant, but checked: compare `Arc::ptr_eq` on
  `shared`). Cross-thread path: `injected_tx.send` + take `parked` + `w.wake()`.
- [ ] **Step 3: Miri** (the DBG Miri-clean precedent):
  ```bash
  cargo +nightly miri test -p ascript --lib exec::   # document the exact invocation that
                                                     # works in-repo; isolation flags as needed
  ```
  Must be clean. If Miri cannot run a test (e.g. real threads + time), split the exec tests
  into a Miri-able set and a native-only stress set — both named in the module doc.
- [ ] **Step 4: Stress test** `cross_thread_wake_storm`: 4 threads × 10k wakes/aborts against
  a churning executor (spawn/complete loop), assert no panic, no lost completion, bounded
  queue. Native-only (not Miri), `#[test]` with a fixed iteration count (deterministic
  enough; no timing asserts).
- [ ] **Step 5:** clippy + tests green both configs. **Commit**
  `feat(exec): Send-safe waker — same-thread fast path + lock-free injection (EXEC Task 2)`.
- [ ] **Independent review:** reviewer probes: waker `Clone` + drop on foreign thread;
  `queued` flag reset ordering (wake DURING poll requeues — write a targeted test if the
  implementer didn't); confirms zero `unsafe` in the module.

## Task 3: `JoinHandle<T>` / `AbortHandle` — tokio-parity surface + abort semantics

**Files:** Modify `src/exec/mod.rs`.

- [ ] **Step 1: Failing tests:**
  - `join_handle_awaits_output`: spawn returns `JoinHandle<i32>`; awaiting it yields
    `Ok(42)`; `is_finished()` flips.
  - `abort_idle_task_drops_at_dequeue` (S3): abort a queued-but-not-running task; its
    future's `Drop` (tracked via a guard struct) runs at the NEXT tick, not inside
    `abort()`; join resolves `Err(Aborted)`.
  - `abort_during_own_poll_defers_drop` (S3, the UB guard): a task whose body calls
    `abort()` on its own handle mid-poll; the mark is set, the poll completes its return,
    the future is dropped after — never during — the poll. (Drive via a handle smuggled
    into the body through an `Rc<RefCell<Option<AbortHandle>>>`.)
  - `cross_thread_abort`: `AbortHandle` is `Send + Sync` (compile-time
    `static_assertions::assert_impl_all!`); aborting from a `std::thread` lands via
    injection and cancels.
  - `double_abort_and_abort_after_complete` are no-ops.
- [ ] **Step 2: Implement.** `spawn_local_on(core, fut) -> JoinHandle<T>` wraps the user
  future:
```rust
struct JoinState<T> { slot: RefCell<Option<Result<T, Aborted>>>, waiter: RefCell<Option<Waker>>,
                      finished: Cell<bool> }

pub struct JoinHandle<T> { inner: JoinInner<T> }
enum JoinInner<T> { Bespoke { state: Rc<JoinState<T>>, id: TaskId, shared: Arc<Shared> },
                    Tokio(tokio::task::JoinHandle<T>) }

#[derive(Clone)]
pub struct AbortHandle { inner: AbortInner }
#[derive(Clone)]
enum AbortInner { Bespoke { id: TaskId, shared: Arc<Shared> },
                  Tokio(tokio::task::AbortHandle) }
```
  Bespoke abort: same-thread + not-running → mark + enqueue (drop at dequeue); running ==
  self → mark only; cross-thread → `Inject::Abort`. The abort path also resolves
  `JoinState` to `Err(Aborted)` **at the drop point** (when the future actually drops), so
  `is_finished()`/join observe tokio-equivalent timing. `JoinHandle<T>` is `!Send`
  (documented; every call site is same-thread — spec §5.2 inventory).
- [ ] **Step 3:** clippy + tests green; Miri re-run on the exec module. **Commit**
  `feat(exec): JoinHandle/AbortHandle parity + deferred-drop abort (EXEC Task 3)`.
- [ ] **Independent review:** probes `abort → wake` and `wake → abort` interleavings; a
  joiner dropped before completion (waiter waker dropped cleanly); join-then-abort.

## Task 4: tokio integration — `run_until`/`drain` inside the unchanged `LocalSet`

**Files:** Modify `src/exec/mod.rs`.

- [ ] **Step 1: Failing tests** (now `#[tokio::test]` + `LocalSet`, the runtime shape of the
  entry points):
  - `run_until_drives_root_and_tasks`: root spawns 3 tasks then awaits a `SharedFuture`-like
    rendezvous they resolve; output order matches the tokio-executor run of the same logic.
  - `drain_runs_survivors`: a detached task spawned by root completes during `drain()`.
  - `sleep_inside_bespoke_task_fires` (driver interop): a bespoke task `tokio::time::sleep
    (5ms)` completes — the leaf future registered OUR waker with tokio's timer wheel and the
    park delivered it.
  - `driver_liveness_under_ping_pong` (spec §2.3): two bespoke tasks waking each other via
    `tokio::sync::Notify` in a loop, PLUS a 10ms sleep task that must still fire — the tick
    budget guarantees tokio gets driver turns. Bound the ping-pong by the sleep's side
    effect flag (deterministic exit).
  - `spawn_blocking_wakes_bespoke_task` (cross-thread end-to-end): a bespoke task awaiting
    `tokio::task::spawn_blocking(|| 7)` completes.
- [ ] **Step 2: Implement** the executor future + public driver API:
```rust
pub struct Executor { core: Rc<Core> }

impl Executor {
    pub fn new() -> Self { /* core + shared with current ThreadId */ }
    /// RAII-install into CURRENT_EXEC; uninstalls (and asserts emptiness of the
    /// registry slot it owns) on drop — panic-safe.
    pub fn install(&self) -> ExecGuard { ... }
    /// Drive `root` to completion while ticking the task queue — the bespoke
    /// analogue of `LocalSet::run_until`. Runs as ONE tokio task, wrapped in
    /// `tokio::task::unconstrained` (spec §2.3 coop note) by the caller-facing
    /// helper below.
    pub async fn run_until<F: Future>(&self, root: F) -> F::Output { ... }
    /// Drive until `live == 0` — the bespoke analogue of `local.await`.
    pub async fn drain(&self) { ... }
}
```
  `run_until` internals: `poll_fn` loop — pin root; each poll: clear `parked`, drain
  injected, poll root once (root is polled FIRST each executor wake, then the FIFO — the
  `LocalSet::run_until` shape), then `poll_tick(EXEC_TICK_BUDGET)`; outcomes: root Ready →
  return; budget exhausted with more work → `wake_by_ref` + `Pending`; queue idle → store
  `cx.waker()` in `parked`, **then re-drain injected once** (the lost-wakeup re-check — the
  same pattern as `ResultCell::get`'s, `task.rs:74–79`), and `Pending` only if still idle.
  Callers get `exec_run_until(exec, root)` which applies `tokio::task::unconstrained`.
- [ ] **Step 3:** clippy + tests both configs. **Commit**
  `feat(exec): run_until/drain inside the LocalSet — tick budget + lost-wakeup re-check
  (EXEC Task 4)`.
- [ ] **Independent review:** reviewer hunts the lost-wakeup window specifically (wake lands
  between idle-detect and `parked` store — the re-check must close it; write the targeted
  interleaving test with a foreign-thread wake if missing); confirms `unconstrained` is
  actually applied (a coop-budget test: 1000 `Notify` ops in one tick don't spurious-yield).

## Task 5: the seam + kill switch + installation at every entry point

**Files:** Modify `src/exec/mod.rs`, `src/lib.rs`, `src/main.rs` (run path env read if any),
`src/repl.rs`, `src/worker/isolate.rs`, `src/worker/dispatch.rs`, `src/task.rs`.

- [ ] **Step 1: Failing tests:**
  - `seam_falls_back_to_tokio_without_install`: in a bare `#[tokio::test]` + `LocalSet`
    (no install), `exec::spawn_local` produces a working `JoinHandle` (Tokio variant) —
    the whole existing unit-test fleet keeps working by this property.
  - `seam_routes_to_bespoke_when_installed` + `spawned_total` observes it.
  - `kill_switch_env`: with `ASCRIPT_EXECUTOR=tokio` threaded through the entry-point
    config (tests use the explicit flag, never the env — parallel-test hygiene),
    `spawned_total == 0` after an async program.
  - `task_rs_abort_swap`: re-run the two `task.rs` structured-concurrency tests
    (`dropping_last_handle_aborts_the_task:225`, `detached_future_is_not_aborted:256`)
    against the seam's BOTH variants.
- [ ] **Step 2: The seam fn** (real code):
```rust
/// THE spawn chokepoint (spec §5). Every `tokio::task::spawn_local` in src/
/// routes here. Bespoke when an executor is installed on this thread; tokio
/// otherwise (bare-test harnesses, ASCRIPT_EXECUTOR=tokio runs).
pub fn spawn_local<F>(fut: F) -> JoinHandle<F::Output>
where F: Future + 'static, F::Output: 'static {
    if let Some(core) = CURRENT_EXEC.with(|c| c.borrow().clone()) {
        return spawn_local_on(&core, fut);
    }
    JoinHandle { inner: JoinInner::Tokio(tokio::task::spawn_local(fut)) }
}
```
- [ ] **Step 3: `src/task.rs` handle swap** — the ONLY task.rs change (spec §0.3, S2):
  `abort: RefCell<Option<crate::exec::AbortHandle>>` (`task.rs:90`), `set_abort(&self,
  abort: crate::exec::AbortHandle)` (`task.rs:150`); `Drop` (`:93–101`) and `detach`
  (`:157`) byte-identical. Update task.rs's own tests to `exec::spawn_local`.
- [ ] **Step 4: Installation.** Entry points gain (default-on, env-off):
```rust
let exec = (std::env::var("ASCRIPT_EXECUTOR").ok().as_deref() != Some("tokio"))
    .then(crate::exec::Executor::new);
let _guard = exec.as_ref().map(|e| e.install());
// run_until / drain route through the executor when present:
let result = match &exec {
    Some(e) => local.run_until(crate::exec::exec_run_until(e, root_fut)).await,
    None => local.run_until(root_fut).await,
};
if let Some(e) = &exec { local.run_until(e.drain()).await; }
local.await; // residual no-op guard — kept (spec S11)
```
  applied at: every `LocalSet` entry future in `src/lib.rs` (`run_file*` `:146`,
  `run_tests*`, `run_file_on_vm*` `:2089`, `vm_run_source_cfg` `:2269` and every sibling
  entry that runs programs — enumerate by grepping `LocalSet::new` in lib.rs and classifying
  each), `run_on_worker_stack` (`lib.rs:85–105`), the REPL session (`repl.rs:247,418` —
  install ONCE per session thread, not per line), worker isolates
  (`run_isolate_thread`, `isolate.rs:287–305`; the dedicated path `dispatch.rs:1035`).
  Factor the boilerplate into ONE helper (`exec::run_scoped(local, rt?, root)`) so the
  pattern exists once — DRY across ~12 entry points.
- [ ] **Step 5: Test entries** in `src/lib.rs` beside `vm_run_source_generic:804/2240`:
  `vm_run_source_tokio_exec` (executor off) and `#[doc(hidden)] vm_run_source_exec_stats`
  → `(output, exit, bespoke_spawned_total)` threaded via `vm_run_source_cfg`.
- [ ] **Step 6:** full `cargo test` BOTH configs (this is the first task where the default
  runtime path changes — expect and fix fallout NOW, not in Task 8). clippy both. **Commit**
  `feat(exec): spawn seam + ASCRIPT_EXECUTOR kill switch + per-entry installation
  (EXEC Task 5)`.
- [ ] **Independent review:** reviewer greps for any remaining direct
  `tokio::task::spawn_local` outside `src/exec/` (allowed ONLY in task.rs/exec tests that
  pin the Tokio variant), runs `examples/concurrency.as` + `examples/workers_*.as` under
  both env settings, and verifies REPL cross-line async works interactively.

## Task 6: migrate the engine spawn sites + worker bridges

**Files:** Modify `src/interp.rs`, `src/vm/run.rs`, `src/worker/mod.rs`.

- [ ] **Step 1: Failing test:** `engine_spawns_are_bespoke` — `vm_run_source_exec_stats`
  over a program with N async calls reports `bespoke_spawned_total ≥ N`; same for a
  tree-walker entry (add a tree-walker stats twin if needed).
- [ ] **Step 2: Migrate** (mechanical: `tokio::task::spawn_local` → `crate::exec::
  spawn_local`; `handle.abort_handle()` unchanged textually — the seam's `JoinHandle`
  mirrors it): `interp.rs:5321, 5908, 5982, 2369, 2468`; `run.rs:1747, 5709`, test helper
  `:7762`; `worker/mod.rs:166, 236, 285, 454`. The `set_abort` sites (`interp.rs:5336,5922,
  5994,2382,2509`; `run.rs:1753,5714,7766`; `worker/mod.rs:197,240,322,484`) compile
  against the new type from Task 5 unchanged.
- [ ] **Step 3: Scheduling smoke battery** (the S1 contract, before the full differential):
  a dedicated test module with ~8 order-observing programs (spawn-order prints; un-awaited
  side effects; await-after-spawn; spawn-in-spawn; async method + static async method —
  covering all five engine sites) asserted `bespoke == tokio` byte-identical on BOTH
  engines.
- [ ] **Step 4:** `cargo test` both configs; `tests/vm_differential.rs` green as-is (it runs
  the default = bespoke now). **Commit**
  `feat(exec): engine async-fn spawns + worker bridges on the bespoke executor (EXEC Task 6)`.
- [ ] **Independent review:** reviewer diffs the five sites against the spec's "only the
  spawn call and abort type change" claim; runs the workers example corpus + the 4 known
  flaky workers_stateful tests in isolation; probes an async fn that panics pre-first-await.

## Task 7: migrate the stdlib spawn sites

**Files:** Modify `src/stdlib/{task_mod,time_timers,sync,http_server,postgres}.rs`,
`src/stdlib/ai/mod.rs`, `src/lib.rs:556`.

- [ ] **Step 1: Failing tests:** the existing suites for race/timeout/debounce/throttle/
  rate-limit/server already pin behavior — run them under the bespoke default and fix
  type fallout. Add: `race_losers_cancelled_bespoke` (port of the race-cancellation test
  asserting loser side effects don't run), `debounce_abort_replaces_pending` (timers).
- [ ] **Step 2: Migrate:** `task_mod.rs:148` (+ `AbortOnDrop(tokio::task::AbortHandle)` →
  `AbortOnDrop(crate::exec::AbortHandle)` `:22`), `time_timers.rs:248,354` (+ the stored
  `Option<AbortHandle>` fields `:54–66,235,261,338–366`), `sync.rs:749`,
  `http_server.rs:1389` (+ `Vec<tokio::task::JoinHandle<()>>` → `Vec<crate::exec::
  JoinHandle<()>>` `:1303` and its drain/abort uses), `postgres.rs:73–78,101`,
  `ai/mod.rs` site, `lib.rs:556`.
- [ ] **Step 3:** FULL suite both configs (`net`/`postgres` features exercise the heavy
  sites); clippy both. **Commit**
  `feat(exec): stdlib task sites on the seam — race/timers/server/postgres (EXEC Task 7)`.
- [ ] **Independent review:** reviewer runs the http_server in-crate integration tests plus
  a manual `server.serve` smoke under both env settings (incl. `workers: 2` multi-isolate —
  each isolate installs its own executor); confirms no direct tokio spawn remains in
  stdlib (`grep -rn "tokio::task::spawn_local" src/stdlib/`).

## Task 8: differential mode + fuzz axis + coverage assertion (Gate 15)

**Files:** Modify `tests/vm_differential.rs`, `fuzz/fuzz_targets/differential.rs`,
`tests/property.rs`.

- [ ] **Step 1: Differential mode.** Extend the harness with the executor axis (spec §6.2):
  corpus + goldens + batteries asserted across tree-walker/spec-VM/generic-VM under bespoke
  AND spec-VM under `vm_run_source_tokio_exec`, BOTH feature configs. Add the
  scheduling-sensitive battery (spec §6.2's list) as first-class corpus members.
- [ ] **Step 2: Coverage assertion** (spec §6.5): aggregate `bespoke_spawned_total` over the
  async corpus `> 0` and printed; `== 0` under executor=tokio; a focused program spawns
  ≥ 10,000 bespoke tasks.
- [ ] **Step 3: Fuzz axis.** `differential.rs` + `property.rs` gain the executor=tokio
  projection; the program generator's weights gain an async-heavy profile (async fn decls,
  await, task.spawn/gather/race, un-awaited calls). Run the fuzzer locally ≥ 30 min /
  ≥ 1M execs (record the number) before calling this done; triage any finding as a bug
  (fix executor) per the binding standards.
- [ ] **Step 4:** full suite + clippy both configs. **Commit**
  `test(exec): executor differential mode, async-weighted fuzz axis, coverage assertion
  (EXEC Task 8)`.
- [ ] **Independent review:** reviewer mutates the executor (e.g. flips FIFO to LIFO
  locally) and confirms the differential/fuzzer CATCHES it (the assertion actually bites),
  then reverts; verifies both feature configs.

## Task 9: the no-leak / cancel battery (the M17 memory bar)

**Files:** Modify `tests/` (a new `tests/exec_battery.rs` or extend `vm_limits.rs`/inline),
`src/vm/run.rs` test helpers if needed.

- [ ] **Step 1: The ADR scenario re-run** (spec §6.4): the un-awaited-async-loop program
  (the `run_program_max_inflight` harness, `run.rs:7774`) under bespoke: `max_inflight`
  bounded (≤ the INFLIGHT cap class), AND a CLI-level run measuring peak RSS via
  `/usr/bin/time -l` stays in the ~8 MB class at 200k iterations (assert a generous ceiling,
  e.g. < 40 MB, to be machine-tolerant — the regression it guards is 130 MB-class).
- [ ] **Step 2: S-invariant tests not yet covered:** `inflight_cap_reaps_under_bespoke`
  (S8: > 256 in-flight un-awaited tasks, byte-identical bespoke vs tokio),
  `telemetry_span_isolation_under_bespoke` (S12, `telemetry` feature),
  `generators_untouched` (S9: generator corpus under both executors + assert
  `src/coro.rs` has no diff on this branch via review, not code), `vm_limits.rs` recursion
  battery re-run under bespoke (S14), drain-parity with a detached task completing after
  root (S11).
- [ ] **Step 3:** full suite both configs. **Commit**
  `test(exec): M17 leak bar + cancel/fairness/task-local battery (EXEC Task 9)`.
- [ ] **Independent review:** reviewer runs the RSS scenario 3× and at 2 sizes; probes
  cancel-mid-await-of-sleep (timer leaf dropped cleanly); race-with-all-pending then drop.

## Task 10: bench A/B + microbench + the SHIP decision

**Files:** Create `bench/profiling/spawn_wake.as`, `bench/EXEC_RESULTS.md`. Modify
`bench/profiling/run.sh`, `bench/PROFILING_RESULTS.md`, `tests/vm_bench.rs`.

- [ ] **Step 1: `spawn_wake.as`** — deterministic, both-engine-runnable, prints
  `name=… elapsed_ms=…` like its siblings: (a) 200k spawn+await round trips; (b) 50k
  gather-of-4; (c) 200k awaits of already-resolved futures. Register in `run.sh`.
- [ ] **Step 2: Same-session A/B** (Gate 16): one binary, `bench/ab.sh` driving
  `ASCRIPT_EXECUTOR=bespoke` vs `=tokio` interleaved, medians + geomean + peak RSS over the
  async corpus (`async_inline`, `async_concurrent`, `func_pipeline`, `call_heavy`,
  `server_request`, `spawn_wake`) AND the non-async corpus (`object_churn`,
  `json_roundtrip`, vm_bench compute set). Attribution re-run (`run.sh` + parse_sample.py)
  for the before/after breakdown of the kevent/notify/SharedFuture slices.
- [ ] **Step 3: Gate re-runs (Gates 12/17):** `tests/vm_bench.rs dbg_zero_cost_gate` green;
  spec/tw geomean ≥ 2× holds; executor=tokio vs pre-EXEC `main` shows no regression (the
  seam's fallback branch is noise).
- [ ] **Step 4: Write `bench/EXEC_RESULTS.md`** (date, SHA, machine, tables, attribution
  deltas, RSS) and append the post-EXEC dated section to `bench/PROFILING_RESULTS.md`.
- [ ] **Step 5: THE SHIP DECISION** (spec §7): async-corpus geomean win ≥ 10% AND zero
  non-async/RSS regression AND Tasks 8–9 green → proceed to Task 11. **If the win is
  < 10%:** record the numbers in `EXEC_RESULTS.md` + `goal-perf.md` (EXEC closed:
  gate met but win below ship threshold), do NOT merge the branch — park it with the
  evidence. Both outcomes are explicit, neither is silent.
- [ ] **Step 6: Commit** `bench(exec): same-session A/B + spawn/wake microbench + ship
  verdict (EXEC Task 10)`.
- [ ] **Independent review:** reviewer re-runs the A/B (fresh interleaving) and confirms the
  geomean reproduces within noise; checks no cross-day numbers entered the report.

## Task 11: docs, bookkeeping, holistic review, merge gates

**Files:** Modify `docs/content/language/modules-async.md`, `CLAUDE.md`, `goal-perf.md`,
`superpowers/roadmap.md`.

- [ ] **Step 1: Docs.** `modules-async.md` gains a short "Engine internals: the task
  executor" note — user-visible surface is ONLY the `ASCRIPT_EXECUTOR=tokio` diagnostic
  switch; semantics unchanged. (No NAV change — existing page.) Served-site sanity check.
- [ ] **Step 2: Bookkeeping.** `CLAUDE.md`: a concise EXEC paragraph (the seam, the kill
  switch, the one-executor-per-isolate invariant, "generators never touch the executor",
  the S1 eager-enqueue rule). `goal-perf.md`: EXEC → ✅ merged (or the Task-10 verdict).
  `superpowers/roadmap.md`: the milestone record.
- [ ] **Step 3: Full gates checklist (every box is a command actually run, output captured):**
  - [ ] `cargo clippy --all-targets` clean AND `cargo clippy --no-default-features
    --all-targets` clean.
  - [ ] `cargo test` green AND `cargo test --no-default-features` green.
  - [ ] `tests/vm_differential.rs` green BOTH configs — all four engine modes × both
    executors; coverage assertion output shows bespoke counts.
  - [ ] Fuzz: differential target with async weights ≥ 30 min clean; corpus minimized
    findings (if any) committed as regression tests.
  - [ ] Leak battery (Task 9) green; RSS evidence in `EXEC_RESULTS.md`.
  - [ ] Miri clean on `src/exec` (record the invocation).
  - [ ] `dbg_zero_cost_gate` green; spec/tw geomean ≥ 2×; `bench/EXEC_RESULTS.md` committed.
  - [ ] No `await`-across-borrow introduced (clippy deny carries this); no GC trace into
    exec handles (the executor holds plain boxed futures — confirm no `Trace` impl added).
  - [ ] Examples corpus runs clean under both executors (spot: `concurrency`,
    `structured_concurrency`, `workers_*`, `advanced/server_multicore` via its test).
  - [ ] `git grep -n "tokio::task::spawn_local" src/` returns only `src/exec/` + sanctioned
    test pins.
- [ ] **Step 4: Holistic review** (independent subagent over the whole branch: spec
  conformance S1–S14, the §5.2 inventory completeness, seam fallback correctness, docs).
  Close all findings in-branch.
- [ ] **Step 5: Merge** `feat/vm-executor` → `main` with `--no-ff`; final commit message
  records the headline A/B numbers. House trailer on every commit.
