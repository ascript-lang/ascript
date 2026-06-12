# AScript Bespoke Single-Thread VM Executor — Design (EXEC)

- **Status:** Draft for review — **EVIDENCE-GATED, not scheduled. Do not implement until the
  gate opens.** THE GATE: the post-LANE same-session re-profile (`bench/PROFILING_RESULTS.md`,
  the dated post-LANE section LANE's plan Task 9 produces) shows the **async-runtime share
  still ≥15% on the async corpus** (`goal-perf.md`, "Evidence-gated"). If the re-profile shows
  <15%, this spec is CLOSED with the evidence recorded — that is a legitimate, documented
  outcome, exactly like the JIT's.
- **Date:** 2026-06-12
- **Code:** EXEC (the first evidence-gated spec of the PERF campaign — see `goal-perf.md`)
- **Depends on (HARD gate):** **LANE merged** *and* **its re-profile run** *and* **the ≥15%
  residual measured**. All three; absent any one, this document is reference material only.
- **Depended on by:** nothing.
- **Engines:** **VM + tree-walker BOTH.** The spawning/await machinery is shared: the
  tree-walker spawns async-fn bodies at `src/interp.rs:5321,5908,5982`, the VM at
  `src/vm/run.rs:1747,5709`, both through the same `SharedFuture`/`ResultCell` split
  (`src/task.rs`) onto the same per-isolate `LocalSet` the entry points own
  (`src/lib.rs`, `src/main.rs:476–488`, `src/worker/isolate.rs:287–305`). The executor is
  **per-isolate infrastructure beneath both engines** — neither engine's dispatch changes.
- **Breaking:** **no.** No syntax, no semantics, no opcode, no `.aso` change
  (`ASO_FORMAT_VERSION` untouched). **Observable scheduling must remain byte-identical over
  the corpus + fuzzer — this is the hardest invariant in the spec and is treated the way the
  JIT spec treats deopt: the differential is the gate, the kill switch is permanent, and a
  divergence is always a bug in the executor, never a reason to relax the assertion.**

---

## 0. Read this first — the gate is the design

This spec exists so that the go/no-go decision after LANE is made **against a concrete plan
rather than an assumption** — the JIT-spec discipline (`2026-06-08-baseline-jit-design.md` §0)
applied to the scheduler. It is not a work order. Three things are true up front:

1. **LANE may make this spec unnecessary.** LANE removes the per-instruction async tax and
   completes resolved awaits inline; what EXEC targets is the *residual per-async-operation*
   cost of tokio's generality (§1). LANE's own spec names its post-merge re-profile as "what
   opens (or closes) EXEC's gate" (LANE §1, §8). If the residual async share drops below 15%
   on the async corpus, EXEC is closed with evidence and zero code written.
2. **The risk is concentrated in one invariant:** observable scheduling identity. Programs
   can observe task interleaving through side effects (`print` in spawned bodies, `race`
   winners, `max_inflight`). The executor must reproduce tokio current-thread's observable
   ordering over the entire corpus + fuzzer, in all four engine modes, or it does not ship.
   That proof harness (differential mode + fuzz axis + the leak/cancel battery, §6) is most
   of the cost of this spec — gating on evidence means we don't pay it for a win LANE may
   already have delivered.
3. **Cancel-on-drop and structured concurrency are the language's async soul** (M17,
   `src/task.rs` module doc; ADR `2026-05-30-async-generators.md` "Refinement"). The
   `ResultCell`/handle split — the task holds only the cell, the handle owns the abort,
   no cycle, last-drop genuinely cancels — is preserved **as-is**; only the type behind
   `set_abort` and the machinery the abort drives change (§4.2). The 8 MB-vs-130 MB
   un-awaited-loop precedent (ADR lines 117–133) is re-run as a shipping gate (§6.4).

## 1. Summary & motivation (the measured residual)

`bench/PROFILING_RESULTS.md` (Phase 0, 2026-06-06) attributes the async workloads:

| workload | async-runtime share | breakdown |
|---|---:|---|
| `async_inline` (400k trivial async calls) | **78%** | kevent/reactor park 55%, timer 6%, **tokio abort + ref_dec + notify + SharedFuture ~12%**; VM dispatch 9% |
| `async_concurrent` (200k gathers ×4) | **71%** | kevent 49%, SharedFuture::get 5%, notify+park; stdlib 8% |

LANE attacks the *between-suspension-points* slice (per-instruction async state machine) and
the *resolved-await* slice (inline `try_get`). What LANE deliberately does **not** change
(LANE §1, §9 "rejected") is *when and how tasks are scheduled*: eager `spawn_local`, tokio's
task harness, and the reactor. The residual EXEC targets, **per async operation**:

- **Per-spawn allocation & bookkeeping.** Every `async fn` call allocates a tokio task
  harness (the `RawTask` — future + scheduler header + atomically-refcounted state),
  a `JoinHandle`, an `AbortHandle`, and `LocalSet` queue bookkeeping — on top of the body
  future Box the engine builds anyway. Five engine spawn sites pay this on every async call
  (`src/interp.rs:5321,5908,5982`; `src/vm/run.rs:1747,5709`).
- **Wake round-trips.** A completed AScript future waking its awaiter (`ResultCell::resolve`
  → `Notify::notify_waiters` → tokio waker → schedule) never needs the reactor, yet pays
  tokio's full cross-thread-capable wake path (atomic state transitions, ref-count traffic).
  The measured "abort + ref_dec + notify + SharedFuture ~12%" slice is exactly this plus the
  abort-handle lifecycle.
- **Reactor park/unpark when no I/O is pending.** tokio current-thread polls the I/O driver
  between task batches (the event interval) and parks on kevent whenever its queue drains
  momentarily — the kevent 49–55% slice. A pure-compute async program (the `async_inline`
  shape) has nothing for kevent to ever deliver.

A purpose-built `!Send` executor makes **spawn a slab insert** and **a same-thread wake a
queue push**, and only touches the reactor when something can actually arrive from it. tokio
**remains the I/O + timer driver** — `tokio::net`/`tokio::time`/`spawn_blocking` futures stay
tokio-native and unchanged (§2.1).

No speedup is promised. Expectations: the ~12% abort/notify/SharedFuture slice and the
park-between-batches share of the kevent slice are addressable; the ship threshold is a
measured **≥10% geomean win on the async corpus** with zero regression elsewhere (§7).

## 2. Architecture — two integrations, evaluated honestly

### 2.1 What must NOT be replaced

Tokio's **leaf resources** stay: `tokio::net::{TcpListener,TcpStream,UdpSocket}`
(`src/stdlib/net_tcp.rs:23`, `net_udp.rs:26`, `http_server.rs:99`), `tokio::time::{sleep,
interval,timeout}` (`src/stdlib/mod.rs:792`, `time_timers.rs:99`, `task_mod.rs:186`),
`tokio::sync::{Notify,mpsc,oneshot}` (`src/task.rs:28`, `src/worker/{actor,isolate}.rs`),
`tokio::select!` (`task_mod.rs:181`), `spawn_blocking` (`src/worker/mod.rs:459`,
`worker/testrun.rs:151`, `http_server.rs:1622`), hyper/reqwest/tokio-postgres/redis. A leaf
future does not care who polls it — it registers whatever `Waker` is in the `Context` it is
polled with. Replacing the drivers is **rejected for v1** (io_uring is not macOS; rewriting
the net/timer stack re-proves the entire stdlib for no measured need).

### 2.2 Architecture A — full replacement (the outer loop is ours)

Our loop owns the thread: poll the bespoke ready queue to exhaustion; when empty, park by
handing control to tokio so the I/O/timer driver runs. **Evaluated against tokio's public
API honestly:** tokio exposes no stable "turn the reactor once without parking forever" —
`Runtime::turn` was removed pre-1.0; `Runtime::block_on`/`Handle::block_on` park until their
future completes. The workable shape is *park-on-a-rendezvous*:

```rust
loop {
    self.run_ready_to_exhaustion();           // our queue, our wakers
    if self.all_tasks_done() { break; }
    // Park: block_on a future that resolves when ANY of our wakers fires.
    // The park inside block_on polls kqueue + the timer wheel; driver events
    // call OUR registered wakers, which notify this rendezvous.
    rt.block_on(self.injected_or_local_wake.notified());
}
```

This works, but every cross-source wake must additionally wake the rendezvous (one more
notify hop), and the entry points (`run_until` + drain at `src/lib.rs:146–148,2089–2095`,
the REPL's per-line `LocalSet` at `src/repl.rs:247,418`, the DAP debuggee, every
`#[tokio::test]`) must be restructured around a non-`LocalSet` driver. The integration risk
is real and the *additional* win over Architecture B is one task-poll indirection per
executor wake — measurable only after B exists.

**Verdict: recorded as v2-on-evidence.** Implement only if B ships, the gate's follow-up
re-profile still shows material park/dispatch overhead attributable to the outer tokio layer,
and a spike proves the delta.

### 2.3 Architecture B — replace SPAWNING + WAKING inside one long-lived tokio task (v1, RECOMMENDED)

Tokio stays the outer runtime exactly as today (`new_current_thread().enable_all()` +
`LocalSet`, unchanged at every entry point). The bespoke executor is **one ordinary future**
driven by the existing `local.run_until(..)`: it owns the task slab and ready queue and
manually polls AScript tasks; tokio sees a single task. Mechanics:

- `exec::spawn_local(fut)` inserts into the slab and pushes the id on the ready queue —
  **no tokio task harness, no `JoinHandle`/`AbortHandle`/`LocalSet` bookkeeping**.
- A task awaiting a tokio leaf future (sleep, TCP read, oneshot) registers **our** waker
  with the driver. When the event fires, the driver calls our waker → ready-queue push +
  wake the executor's own (tokio) waker. Tokio's machinery runs **once per executor
  wake-up**, not once per task wake.
- A same-thread wake (the dominant case: `ResultCell::resolve` → `notify_waiters` → awaiter's
  waker, fired while the executor is mid-tick) is **an `AtomicBool` swap + a `VecDeque`
  push** — no reactor, no tokio scheduling, no ref-count storm.
- When the ready queue is exhausted, the executor returns `Pending` with its context waker
  parked — tokio then polls the driver/parks on kevent **only when there is genuinely
  nothing runnable**, which is the correct time to do so.

This captures the spawn-allocation, wake-path, and park-between-batches costs with **zero
runtime-integration risk**: the entry points, the test suite's hundreds of
`#[tokio::test]` + `LocalSet` harnesses, the REPL, the DAP server, and the worker isolates
keep their exact shape. **This is v1.**

Two tokio-interaction details, named precisely because they are correctness-bearing:

- **Coop budget.** Tokio gives each task poll a cooperative budget (consumed by leaf
  resources); our executor poll would burn one budget across ALL nested task polls, causing
  spurious `Pending`s from leaf futures deep in a tick. The executor future is therefore
  wrapped in **`tokio::task::unconstrained`** (public API, exactly this use case).
- **Driver liveness.** Unconstrained + poll-to-exhaustion means a self-sustaining wake cycle
  (tasks ping-ponging through `Notify`/`yield_now`) could keep the executor's queue non-empty
  forever, never returning to tokio — the I/O driver and timer wheel would starve (today the
  event interval guarantees a driver poll every 61 task polls). The executor therefore has a
  **tick budget**: after `EXEC_TICK_BUDGET` task polls in one executor poll (initial value
  61, matching tokio's event interval; a tunable const, NOT user-visible), it self-wakes
  (`cx.waker().wake_by_ref()`) and returns `Pending`, giving tokio a driver turn. A
  regression test pins this: two mutually-waking tasks + a `sleep` that must still fire.

## 3. Task representation & the waker (the soundness core)

### 3.1 The slab

Per-executor (per-isolate), all `!Send`:

```rust
struct Core {
    /// Task cells, slot-indexed; freed slots go on a free list and bump `gen`
    /// so a stale TaskId (a waker outliving its task) can never touch a reused slot.
    tasks: RefCell<Vec<Slot>>,           // Slot { gen: u32, cell: Option<TaskCell> }
    free: RefCell<Vec<u32>>,
    /// FIFO ready queue of TaskIds. Spawn pushes back; wake pushes back; the
    /// dedupe flag (§3.2) guarantees at most one queue entry per task.
    ready: RefCell<VecDeque<TaskId>>,
    /// Cross-thread injection (wakes + aborts) — drained at tick start (§3.2).
    injected_rx: std::sync::mpsc::Receiver<Inject>,
    /// The Send half shared with every waker/abort-handle (§3.2).
    shared: Arc<Shared>,
    /// The id of the task currently being polled (None between polls) — the
    /// self-abort deferral check (§4.2).
    running: Cell<Option<TaskId>>,
    /// Live (spawned, not yet completed/aborted) task count — drives `drain`.
    live: Cell<usize>,
    /// Anti-false-green counter (§6.5): total tasks ever spawned on this executor.
    spawned_total: Cell<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TaskId { index: u32, gen: u32 }

struct TaskCell {
    /// The task body. `None` after completion or (deferred) abort-drop.
    future: Option<Pin<Box<dyn Future<Output = ()>>>>,
    /// Built once per task from `Arc<ExecWaker>` — one allocation per TASK,
    /// not per wake (tokio re-derives wakers per poll from the RawTask).
    waker: Waker,
    /// Abort requested; the future is dropped at the next dequeue (§4.2).
    aborted: Cell<bool>,
}
```

The ready queue is a `VecDeque<TaskId>` for v1; intrusive links are a v2-if-measured
refinement (the queue entries are 8 bytes and the deque never shrinks below its high-water
mark — there is no per-wake allocation in steady state).

`JoinHandle<T>` (the tokio-parity surface the call sites need — awaitable join,
`is_finished()`, `abort()`, `abort_handle()`): `spawn_local` wraps the user future so the
output lands in an `Rc<JoinState<T>>` (slot + parked joiner waker); the handle is `!Send`
(every existing call site is same-thread — verified inventory §5.2). `AbortHandle` IS
`Send + Sync` (tokio's is; keeping that contract costs nothing and avoids latent breakage):
it carries `(TaskId, Arc<Shared>)` and routes same-thread aborts directly, cross-thread
aborts through the injection queue.

### 3.2 The waker — Send-safety designed from verified wake sources

**The claim "current_thread means all wakes are same-thread" is FALSE, verified:** with a
current-thread runtime, *driver-delivered* events (kqueue, timer wheel) do call wakers on
the runtime thread (the parked thread polls the driver itself) — but three cross-thread wake
sources exist in this codebase today:

1. **`spawn_blocking` completions** — the blocking-pool thread wakes the joining task
   (`src/worker/mod.rs:459`, `src/worker/testrun.rs:151`, `src/stdlib/http_server.rs:1622`).
2. **Worker-isolate channel sends** — `tokio::sync::oneshot::Sender::send` /
   `mpsc::UnboundedSender::send` invoke the receiver's stored waker **from the isolate
   thread** (`src/worker/isolate.rs:120–123`, `actor.rs:29,86`; the bridges at
   `src/worker/mod.rs:115–116,166,236,285,454` await these on the main isolate).
3. **reqwest/hyper internals** (DNS resolution on a blocking pool under the `net` feature).

So an `Rc`-thin waker is **unsound** and rejected. The design:

```rust
/// Send + Sync. The waker is built via `std::task::Wake` over `Arc` — SAFE code,
/// no hand-rolled RawWaker vtable, nothing for Miri to find in the waker itself.
struct ExecWaker {
    id: TaskId,
    /// Wake dedupe: at most one ready-queue entry per task. Cleared by the
    /// executor immediately BEFORE polling the task (the standard protocol:
    /// wakes DURING a poll re-queue the task).
    queued: AtomicBool,
    shared: Arc<Shared>,
}

/// The Send half. One per executor.
struct Shared {
    /// The executor's home thread — the same-thread fast-path discriminator.
    thread: std::thread::ThreadId,
    /// Lock-free MPSC (std::sync::mpsc is the crossbeam-based lock-free channel;
    /// `Sender` is `Sync` since Rust 1.72). Carries cross-thread wakes AND aborts.
    injected_tx: std::sync::mpsc::Sender<Inject>,
    /// The executor's parked tokio waker. Written once per executor park, taken
    /// by whichever wake arrives first. An uncontended std Mutex on a rare-ish
    /// path — deliberately chosen over a hand-rolled AtomicWaker: zero unsafe,
    /// Miri-trivial, and the hot same-thread mid-tick wake finds it `None` and
    /// never blocks. A lock-free waker slot is a recorded v2-if-profiled item.
    parked: std::sync::Mutex<Option<Waker>>,
}

impl std::task::Wake for ExecWaker {
    fn wake(self: Arc<Self>) { self.wake_by_ref(); }
    fn wake_by_ref(self: &Arc<Self>) {
        if self.queued.swap(true, Ordering::AcqRel) { return; }   // already queued
        if std::thread::current().id() == self.shared.thread {
            // SAME-THREAD FAST PATH: push directly onto the executor's local
            // ready queue via the thread-local current-executor registry.
            // Falls back to injection if the executor is gone (late waker).
            if CURRENT_EXEC.with(|c| /* push self.id; true if executor live */) {
                // wake the parked executor (no-op mid-tick: slot is None)
                if let Some(w) = self.shared.parked.lock().unwrap().take() { w.wake(); }
                return;
            }
        }
        // CROSS-THREAD (or late) path: lock-free injection + unpark.
        let _ = self.shared.injected_tx.send(Inject::Wake(self.id));
        if let Some(w) = self.shared.parked.lock().unwrap().take() { w.wake(); }
    }
}
```

- `CURRENT_EXEC` is a `thread_local! { RefCell<Option<Rc<Core>>> }` installed by the entry
  point for the isolate's lifetime — **one executor per isolate thread** is an invariant
  (the per-isolate construction sites, §5.3), so the registry is never ambiguous. The
  `Send + Sync` `Arc<ExecWaker>` never holds the `!Send` `Rc<Core>`; same-thread access goes
  through the registry, which is what makes the fast path sound.
- A wake for a completed/freed task is made safe by the **generation check at dequeue**: the
  executor pops a `TaskId`, compares `slot.gen`, and skips silently on mismatch or a `None`
  cell (wake-after-complete is a no-op, exactly as tokio's).
- The executor's tick: clear-and-drain `injected_rx` into `ready` → pop → clear `queued` →
  set `running` → poll → handle outcome → repeat, up to `EXEC_TICK_BUDGET` (§2.3).
- **Verification posture:** the waker core is 100% safe Rust over std-verified primitives
  (`Arc`, `AtomicBool`, std mpsc, std `Mutex`) — there is no hand-rolled lock-free unsafe for
  **loom** to verify, so loom is *rejected with justification*. The concurrency battery is:
  a cross-thread wake stress test (N threads hammering wakes/aborts during execution), the
  abort/wake/drop race tests (§6.4), and **Miri over the entire `src/exec` unit-test module**
  (the DBG Miri-clean precedent) as a standing CI-able check.

## 4. Semantics preserved EXACTLY (the invariant table)

Each invariant below is restated as a testable obligation; the plan maps each to a named
test. The differential (§6) covers them all end-to-end; these are the focused guards.

| # | Invariant | Precise statement (verified against today's code) | Test |
|---|---|---|---|
| S1 | **Eager scheduling, not eager execution** | A script `async fn` call **enqueues** the body and returns the `Value::Future` immediately; the body does NOT run synchronously to its first await — it runs when the spawner next suspends and FIFO order reaches it. (Verified: `tokio::task::spawn_local` queues; `run.rs:1699–1713` "hand back a Value::Future IMMEDIATELY"; `interp.rs:5302–5305`.) `exec::spawn_local` = slab insert + push-back, **never an inline first poll**. | `exec_spawn_is_enqueue_not_run` + the ordering corpus |
| S2 | **Cancel-on-drop** | Dropping the last `Value::Future` handle aborts the backing task; the `ResultCell`/handle split (`task.rs:33–106`) is byte-preserved — the only change is the type stored by `set_abort` (`task.rs:150`): a seam `exec::AbortHandle` instead of `tokio::task::AbortHandle`, same `Drop`-driven `abort()` (`task.rs:93–101`). | port of `dropping_last_handle_aborts_the_task` (`task.rs:225`) on bespoke + the leak battery |
| S3 | **Abort = deferred drop** (matches tokio) | `abort()` marks the cell and enqueues; the future is **dropped by the executor at dequeue**, never synchronously inside `abort()`. This uniformly handles the self-abort case (a running task causing its own abort): `running == id` → mark only, drop after the poll returns — dropping a future mid-`poll` is instant UB and the deferral is the guard. | `abort_during_own_poll_defers_drop`, `abort_idle_task_drops_at_dequeue` |
| S4 | **Detach** | `task.spawn` detaches (`task_mod.rs:75–110` → `SharedFuture::detach`, `task.rs:157`): the abort handle is removed; the task runs to completion under the drain. Unchanged — detach never touches the executor. | port of `detached_future_is_not_aborted_on_drop` (`task.rs:256`) |
| S5 | **Race loser cancellation** | `task.race` spawns resolver tasks guarded by `AbortOnDrop` (`task_mod.rs:20–28,148–152`); winner resolution order and loser cancellation are FIFO-faithful. | `race_*` suite under both executors + differential |
| S6 | **Timeout** | `task.timeout` is `tokio::select!` over `fut.get()` vs `tokio::time::sleep` (`task_mod.rs:181–189`) — both are leaf futures polled inside a bespoke task; cancellation of the timed-out work is S2. Untouched code; covered by the battery. | existing `timeout` tests under bespoke |
| S7 | **Gather ordering** | `task.gather` awaits elements **in input order** (`task_mod.rs:114–128`); completion order of the underlying tasks is FIFO-faithful per S1. | existing gather tests + differential |
| S8 | **`maybe_yield_for_inflight` fairness** | The cooperative yield fires only at `inflight ≥ INFLIGHT_YIELD_CAP` (= 256, `interp.rs:651,1283–1287`), via `tokio::task::yield_now()` — which is wake-then-`Pending`, so under the bespoke executor it re-queues the current task at the BACK of the FIFO, the identical fairness effect. The cap, the call sites, and the `InflightGuard` counter (`interp.rs:1270`) are **untouched** (changing fairness constants is out of scope, §8). | `inflight_cap_reaps_under_bespoke` (the >256 un-awaited-loop guard, lane-style) |
| S9 | **Generator non-involvement** | Generators are consumer-driven boxed futures, NOT spawned tasks (`src/coro.rs:1–26,44`); they never touch the executor on ANY path (`GenImpl::{Body,Vm}` are polled in-line by `resume`; `GenImpl::Worker` awaits channel leaf futures). Asserted untouched: zero `src/coro.rs` diff. | `git diff --stat` check + generator corpus under both executors |
| S10 | **Per-isolate executors** | Every isolate thread builds its OWN executor beside its own `Interp`/`Vm` (`run_isolate_thread`, `worker/isolate.rs:287–305`; `dispatch.rs:1035`; `run_on_worker_stack`, `lib.rs:85–105`; `main.rs:476–488`; the REPL session). Nothing executor-related crosses the airlock. | workers corpus under both executors |
| S11 | **Drain semantics** | Entry points do `run_until(root)` then drain (`lib.rs:146–148`, `lib.rs:2089–2095` `local.await`): tasks are driven while the root is awaited; survivors (detached, un-awaited) run to completion afterward. The bespoke `run_until` drives root-as-task + queue FIFO until root completes; `drain()` runs until `live == 0`. A forever-parked task hangs the drain — exactly today's behavior. | drain-parity tests incl. detached-after-root |
| S12 | **Task-locals (telemetry + RESIL)** | `tokio::task_local!` `TELEMETRY_CURRENT` (`interp.rs:54–68`) works via `LocalKey::scope(value, future)` — a future WRAPPER that sets/unsets around each poll, executor-agnostic. Corrected state of the world: the **tree-walker** spawn sites wrap `telemetry_scope` (`interp.rs:5326`), but the VM's two async spawn sites (`run.rs:1747`/`:5709` at RESIL's drafting) do **NOT** — RESIL fixes them while adding its deadline task-local. The invariant going forward: **all five user-code spawn sites wrap scope-locals (telemetry + RESIL's task-locals)**; the VM-site gap is fixed by RESIL (or by EXEC if it lands first — coordinate). Pinned by test under the `telemetry` feature. | `telemetry_span_isolation_under_bespoke` (extended to the VM spawn sites once wrapped) |
| S13 | **Panic/`Control` propagation across tasks** | A body's `Result<Value, Control>` lands in the `ResultCell` and re-surfaces at the await site — entirely `task.rs`/engine code above the executor. Unchanged. | corpus + panic batteries |
| S14 | **Recursion-depth + stacker** | `call_depth`/`grow_future` (`src/vm/stack.rs`) operate inside the body future regardless of who polls it. Unchanged. | `vm_limits.rs` under bespoke |

## 5. The seam — one spawn chokepoint, dual handles

### 5.1 `exec::spawn_local`

A new module `src/exec/` exposing the tokio-shaped surface:

```rust
/// THE seam. Routes to the installed bespoke executor for this thread, else
/// falls back to `tokio::task::spawn_local` (no executor installed — e.g. the
/// hundreds of #[tokio::test] harnesses, which keep working unchanged).
pub fn spawn_local<F: Future + 'static>(fut: F) -> JoinHandle<F::Output>;

pub struct JoinHandle<T> { /* Bespoke(..) | Tokio(tokio::task::JoinHandle<T>) */ }
impl<T> JoinHandle<T> {
    pub fn abort_handle(&self) -> AbortHandle;
    pub fn abort(&self);
    pub fn is_finished(&self) -> bool;
}
impl<T> Future for JoinHandle<T> { type Output = Result<T, Aborted>; .. }

#[derive(Clone)]
pub struct AbortHandle { /* Bespoke(TaskId, Arc<Shared>) | Tokio(tokio::task::AbortHandle) */ }
impl AbortHandle { pub fn abort(&self); }   // Send + Sync, like tokio's
```

The dual (enum-backed) handles are what make the **kill switch total**: under
`ASCRIPT_EXECUTOR=tokio` every call site takes the `Tokio` variant and the runtime is
byte-for-byte today's — the same physical tokio code path, not an emulation (the
`--no-specialize` discipline).

### 5.2 Call-site inventory (complete, verified by grep 2026-06-12)

Every `tokio::task::spawn_local` in `src/` routes through the seam — keeping ALL tasks on
**one FIFO queue** so relative interleaving is preserved by construction (a split
engine-on-bespoke/stdlib-on-tokio world would create two queues whose merge order differs
from today's single queue — rejected for exactly that reason, §8):

- **Engine async-fn spawns (the hot five):** `interp.rs:5321` (`call_function` async),
  `interp.rs:5908,5982` (async method paths), `run.rs:1747` (`Op::Call` async closure),
  `run.rs:5709` (`invoke_compiled_static` async). Pattern at every site:
  `SharedFuture::new` + `cell` + `inflight_guard` + spawn + `set_abort` + `maybe_yield` —
  only the spawn call and the abort-handle type change.
- **Worker bridges:** `worker/mod.rs:166,236,285,454`; `interp.rs:2369,2468` (set_abort at
  `2382,2509`).
- **stdlib:** `task_mod.rs:148` (race resolvers; `AbortOnDrop` wraps `exec::AbortHandle`),
  `time_timers.rs:248,354` (debounce/throttle pending-task aborts at `:66,235,261,364`),
  `sync.rs:749` (rate-limiter window timer), `http_server.rs:1389` (per-connection tasks;
  the `Vec<JoinHandle<()>>` inflight list at `:1303`), `postgres.rs:73–78` (driver task),
  `stdlib/ai/mod.rs`, `lib.rs:556` (test-runner tasks).
- **`task.rs`:** `HandleInner.abort` becomes `RefCell<Option<exec::AbortHandle>>`
  (`task.rs:90,150`); `Drop` (`task.rs:93–101`), `detach` (`:157`), and everything else
  byte-identical. **SharedFuture's public semantics are frozen** (§8) — this is the
  sanctioned internals-only change.

### 5.3 Installation, kill switch, per-isolate construction

- **Construction sites** (each becomes "build executor → install in `CURRENT_EXEC` →
  run → uninstall"): the `run`/`test`/`repl` entry futures in `src/lib.rs` (the
  `LocalSet::new()` sites — `run_file*`, `run_tests*`, `run_file_on_vm*`, `vm_run_source_cfg`
  and friends), `run_on_worker_stack` (`lib.rs:85–105`), the REPL session
  (`repl.rs:247,418`), worker isolates (`isolate.rs:287–305`, `dispatch.rs:1035`). An RAII
  guard uninstalls on scope exit so a panicking run never leaves a stale registry entry.
- **Kill switch (permanent, Gate 15):** `ASCRIPT_EXECUTOR=tokio` skips installation — the
  seam falls back to real `tokio::task::spawn_local` everywhere. Default (unset or
  `bespoke`) installs the executor. Read at entry-point/isolate construction, so worker
  isolates inherit it process-wide, like `ASCRIPT_NO_SPECIALIZE` (`lib.rs:2066–2067`).
  Tests use explicit entry points (`vm_run_source_tokio_exec`, mirroring
  `vm_run_source_generic`/`vm_run_source_armed_idle`, `lib.rs:2240–2266`), never the env.
- **Orthogonality:** the executor composes with `specialize`, `sync_lane` (LANE), and the
  instrument seam with no special casing — it sits below the engines entirely.

## 6. Correctness — the differential is the gate

### 6.1 Ordering discipline (stated so divergences are debuggable)

One FIFO ready queue; spawn/wake/yield all push-back; the root is driven as part of the same
FIFO discipline; at most one queue entry per task (the `queued` flag). This is the model of
tokio current-thread's local queue. Tokio details deliberately NOT replicated: the coop
budget (replaced by `unconstrained` + the tick budget, §2.3) and the event interval's
mid-batch driver polls (their absence is the win; their only observable effect is wall-clock
timer granularity, which is not byte-deterministic today either). **If the differential or
fuzzer finds any program where this discipline diverges observably, the executor's
discipline is adjusted to match — fix the executor, never the assertion.**

### 6.2 Differential modes (Gates 1 + 15)

`tests/vm_differential.rs` gains the executor axis. Over the expression battery, program
battery, goldens, and the whole `examples/**` corpus, in BOTH feature configs:

> tree-walker == specialized-VM == generic-VM (each under executor=bespoke, the default)
> == specialized-VM(executor=tokio)

plus the scheduling-sensitive battery (programs whose OUTPUT depends on task interleaving:
spawn-order prints, race winners over deterministic-latency tasks, gather over mixed
ready/pending, un-awaited side-effecting calls, nested spawn-in-spawn, cancel-mid-stream)
asserted **bespoke == tokio** in all four engine modes. The tree-walker is never relaxed.

### 6.3 The fuzz axis (same PR)

`fuzz/fuzz_targets/differential.rs` + `tests/property.rs` gain the executor=tokio
projection, AND the generator's grammar weights gain an **async-heavy profile** (async fns,
`await`, `task.spawn/gather/race`, un-awaited calls) so the fuzzer actually exercises
scheduling, not just expressions. Any bespoke/tokio output divergence on a generated program
is a crash with the program attached.

### 6.4 The no-leak / cancel / race battery

- **The M17 memory bar, re-run:** the ADR's un-awaited-loop scenario (130 MB → 8 MB,
  `2026-05-30-async-generators.md:117–133`) re-measured under bespoke: `max_inflight`
  bounded (the existing `run_program_max_inflight` harness, `run.rs:7774`) AND peak RSS flat
  (~8 MB class) via `/usr/bin/time -l`. A leak here is a shipping blocker.
- **Race/edge tests:** `abort_during_own_poll_defers_drop` (S3), `wake_after_complete_is_noop`
  (generation check), `drop_waker_on_foreign_thread` (a worker-thread-held waker dropped
  after the executor is gone), `cross_thread_wake_storm` (N threads × M wakes/aborts during
  execution), `abort_then_wake_interleave`, `spawn_during_drain`, double-abort, abort of a
  detached-then-dropped handle.
- **Miri** over the `src/exec` unit/stress tests (the DBG precedent); **loom rejected with
  justification** (§3.2 — no custom lock-free unsafe exists to model).

### 6.5 The coverage assertion (anti-false-green, the JIT §5.1 rule)

A seam that silently fell back to tokio everywhere would pass every differential while
testing nothing. So: `Core.spawned_total` is exposed through a `#[doc(hidden)]` test entry
(`vm_run_source_exec_stats`); the differential asserts (a) on the async corpus under the
default, **bespoke spawned_total > 0** and is reported (count printed in CI logs); (b) under
executor=tokio, bespoke spawned_total == 0 (the switch genuinely switches); (c) a focused
program spawns ≥ 10,000 tasks through the bespoke path.

## 7. Measurement protocol & ship threshold (Gates 12, 16, 17, 18)

Same-session A/B (the `bench/ab.sh` harness from LANE Task 0), both binaries from the same
commit, executor toggled by env:

1. **The async corpus:** `async_inline`, `async_concurrent`, plus LANE's Task-0 additions
   (`func_pipeline`, `call_heavy`, `server_request`) — per-workload medians, geomean,
   peak RSS (`/usr/bin/time -l`).
2. **Attribution replication:** `bench/profiling/run.sh` + `parse_sample.py` re-run so the
   report shows the same breakdown axes as `PROFILING_RESULTS.md` (kevent/park vs
   abort+ref_dec+notify+SharedFuture vs dispatch) before/after — proving the win comes from
   the targeted slices, not noise.
3. **Spawn/wake microbenchmark:** a deterministic `bench/profiling/spawn_wake.as`
   (spawn-await throughput in tasks/sec; resolved-await and pending-await round-trip
   latency), measured with the shipped profiler where possible (Gate 16 dogfooding).
4. **Non-async corpus:** the full vm_bench compute corpus + `object_churn`/`json_roundtrip`
   — **zero regression** (the executor must be invisible where no task is spawned);
   `dbg_zero_cost_gate` re-run; spec/tw geomean ≥2× floor re-run (Gate 17).

**Ship only when ALL hold:** (a) async-corpus geomean win **≥10%**; (b) zero regression on
the non-async corpus and on peak RSS (Gate 18); (c) the full §6 battery green in both
feature configs, Miri clean. Results recorded in `bench/EXEC_RESULTS.md`. If (a) fails, the
branch is closed and the numbers recorded in `goal-perf.md` — same as a failed gate.

## 8. Scope & rejected

**In scope (when the gate opens):** `src/exec/` (slab, FIFO queue, safe `Wake` waker with
same-thread fast path + lock-free cross-thread injection, tick budget, run_until/drain,
JoinHandle/AbortHandle parity); the total spawn seam over the §5.2 inventory; the
`task.rs` abort-type swap; per-isolate installation; `ASCRIPT_EXECUTOR` kill switch + test
entries; differential mode + async-weighted fuzz axis + coverage assertion; the §6.4
battery + Miri; the §7 measurement artifacts.

**Out of scope / rejected:**
- **A multi-thread executor / work stealing — FORBIDDEN.** The `!Send`-per-isolate model is
  the language's concurrency soul (`CLAUDE.md` M17); parallelism is isolation, full stop.
- **Replacing tokio's I/O + timer drivers** — rejected v1 (§2.1); leaf futures stay
  tokio-native. Revisit only with post-EXEC evidence. **io_uring** — not macOS; out.
- **Architecture A (full outer-loop replacement)** — v2-on-evidence only (§2.2).
- **Changing `SharedFuture`'s public semantics** — forbidden; only its internals' allocation
  profile (the abort-handle type; LANE already added `try_get`) may change.
- **Changing `maybe_yield_for_inflight` constants/placement** — out of scope; fairness
  semantics frozen (S8).
- **Splitting the queue (engine tasks bespoke, stdlib tasks tokio)** — rejected: two queues
  whose merge order differs from today's single queue is a scheduling-identity hazard for
  zero benefit (§5.2).
- **Rc-thin wakers (no atomics)** — rejected as unsound: cross-thread wakes exist (§3.2).
- **Inline first-poll on spawn** ("run to first await synchronously") — rejected: diverges
  from the M17 eager-*scheduling* model (S1) and is observably distinguishable.
- **loom for the executor core** — rejected with justification (§3.2): no hand-rolled
  lock-free unsafe exists; std primitives + Miri + the stress battery are the proof tools.

## 9. Grounding (verified file:line, 2026-06-12 — re-grep before relying on these)

- `src/task.rs` — `CellInner:33`, `ResultCell:43`, `resolve:54`, `get` (resolved fast path
  before any await) `:63–81`, `HandleInner:86–91`, cancel-on-drop `Drop:93–101`,
  `SharedFuture:106`, `resolved:119`, `get:132`, `cell:144`, `set_abort:150`, `detach:157`;
  tests `dropping_last_handle_aborts_the_task:225`, `detached_future_is_not_aborted:256`.
- `src/interp.rs` — `INFLIGHT_YIELD_CAP = 256:651`, `inflight_guard:1270`,
  `maybe_yield_for_inflight:1283–1287`, async-fn spawn `:5306–5341` (spawn `:5321`,
  `set_abort:5336`, `maybe_yield:5339`), method spawns `:5908(5922), :5982(5994)`, worker
  bridges `:2369(2382), :2468(2509)`, `tokio::task_local! TELEMETRY_CURRENT:54–68`.
- `src/vm/run.rs` — `Op::Call` async-closure arm `:1728–1755` (eager-scheduling comment
  `:1699–1713`, spawn `:1747`, `set_abort:1753`, `maybe_yield:1754`),
  `invoke_compiled_static` async `:5704–5716` (spawn `:5709`), test helpers
  `spawn_vm_future:7758`, `run_program_max_inflight:7774`.
- Entry points / runtimes — `src/main.rs:476–488` (worker thread + `new_current_thread`);
  `src/lib.rs` `run_on_worker_stack:85–105`, tree-walker run `:146–148` (run_until +
  `local.await` drain), VM run `:2089–2095`, kill-switch precedent `ASCRIPT_NO_SPECIALIZE
  :2066–2067`, test entries `:2240–2269`, test-runner spawn `:556`; `src/repl.rs:247,418`.
- Workers — `src/worker/isolate.rs` `run_isolate_thread:287–305` (per-isolate rt `:293`,
  `LocalSet:300`, `Vm::new:303`), `WorkerRequest.reply/abort` oneshots `:120–123`,
  `bootstrap:268`; `src/worker/mod.rs` bridges `:166,236,285,454`, oneshots `:115–116`,
  std-mpsc reply `:367`, `spawn_blocking:459`; `worker/testrun.rs:151`; `actor.rs:29,86,126`;
  `dispatch.rs:1035`.
- stdlib spawn/leaf sites — `task_mod.rs` `AbortOnDrop:20–28`, spawn(detach) `:75–110`,
  gather `:114–128`, race `:136–163` (spawn `:148`), timeout select+sleep `:181–189`;
  `time_timers.rs:54–66,99,235,248,261,338–366`; `sync.rs:749`; `http_server.rs:99,1303,
  1389,1622`; `postgres.rs:73–78,101`; `stdlib/mod.rs:792` (sleep); `net_tcp.rs:23`,
  `net_udp.rs:26`.
- Generators untouched — `src/coro.rs:1–26` (module doc: consumer-driven, NOT a spawned
  task), `BodyFuture:44`, `GenImpl:54–108`.
- Evidence — `bench/PROFILING_RESULTS.md` (the 71–78% / kevent 49–55% / ~12%
  abort+ref_dec+notify+SharedFuture attribution); the M17 leak precedent
  `superpowers/specs/adr/2026-05-30-async-generators.md:117–133` (and `:110–115` — the ADR
  explicitly names "an owned single-threaded cooperative scheduler replacing tokio's
  `LocalSet`/`spawn_local`" as the one thing SP9 did NOT build: EXEC is that item, now
  evidence-gated).
- Posture precedents — `superpowers/specs/2026-06-08-baseline-jit-design.md` (§0 deferral-
  is-the-design; §5.1 anti-false-green); `superpowers/specs/2026-06-12-two-lane-engine-
  design.md` (the corpus, the kill-switch discipline, the re-profile that is THIS spec's
  gate input); `goal-perf.md` gates 15–18; `goal.md` gates 1–14.
- Tokio public-API facts relied on (re-verify against the pinned tokio 1.x at
  implementation time): `LocalSet`/`spawn_local`/`block_on` shapes; `task::unconstrained`
  (coop opt-out); the absence of a public single-turn driver API; `std::task::Wake` (safe
  waker over `Arc`); `std::sync::mpsc` lock-free + `Sender: Sync` (Rust ≥1.72; repo is on
  1.96 per `bench/PROFILING_RESULTS.md`).
