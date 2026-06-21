//! EXEC: the bespoke per-isolate task executor (spec
//! `superpowers/specs/2026-06-12-vm-executor-design.md`). Replaces tokio's task
//! HARNESS (spawn/wake/yield) for AScript tasks; tokio remains the I/O + timer
//! driver. `!Send` like the whole runtime; exactly one per isolate thread.
//! Task 1 = the core (slab + FIFO queue + tick). Task 2 = the Send-safe
//! cross-thread waker (same-thread fast path + lock-free injection); tokio
//! run_until/drain integration is Task 4.
//!
//! ## Waker design (Task 2, spec §3.2)
//!
//! A polled future needs a [`Waker`]. The per-task waker is built from a SAFE
//! [`std::task::Wake`] impl ([`TaskWaker`]) — zero `unsafe`, so there is nothing
//! for Miri to find in the waker itself. The `Waker` MUST be `Send + Sync`
//! because wakes genuinely arrive from OTHER threads here (`spawn_blocking`
//! completions, worker-isolate channel sends, reqwest/hyper DNS on a blocking
//! pool). The split that makes this safe: the `Send + Sync` [`Arc<TaskWaker>`]
//! holds only `Arc<Shared>` (never the `!Send` `Rc<Core>`).
//!
//! On wake ([`TaskWaker::signal`]):
//!
//! 1. **Dedupe** via the `queued` flag (a flurry of wakes enqueues the task at
//!    most once). `queued` is cleared by the executor in [`Core::poll_one`]
//!    BEFORE it polls, so a wake DURING a poll re-queues the task.
//! 2. **Same-thread fast path:** if the waking thread is the executor thread, we
//!    clone the `Rc<Core>` out of the [`CURRENT_EXEC`] thread-local (dropping the
//!    borrow immediately), confirm identity via `Arc::ptr_eq` on `shared`, and
//!    push straight onto `core.ready` — no channel round-trip. If the registry is
//!    empty or holds a DIFFERENT executor, we fall through to injection.
//! 3. **Cross-thread (or late / no-registry) path:** `injected_tx.send(Wake(id))`
//!    (folded into the ready queue at the next [`Core::drain_injected`]).
//!
//! Both paths then wake the executor's parked tokio waker ([`Shared::parked`]) if
//! present (Task 4 parks it there; for Task 2 it is usually `None` → a no-op).
//!
//! ## Thread-local registry
//!
//! [`CURRENT_EXEC`] holds the (at most one) live executor on this isolate thread.
//! One-executor-per-thread is an invariant, so the registry is never ambiguous;
//! [`install_current`] asserts an install never stomps a different executor.
//! Task 4/5 install it via [`CurrentExecGuard`] (RAII) at the entry points.
//!
//! ## Carry-forward shortcuts (flagged for later tasks)
//!
//! * **Abort (DONE — Task 3).** [`AbortHandle::abort`] is deferred-drop, matching
//!   tokio: it NEVER drops the future synchronously (mid-`poll` drop is UB). The
//!   three cases — idle (mark + enqueue → dropped at the next dequeue), self-abort
//!   of the running task (mark only → dropped by [`Core::poll_one`]'s post-poll
//!   re-check), and cross-thread (`Inject::Abort` → [`Core::drain_injected`] marks
//!   then enqueues) — all converge on the future actually dropping, at which point
//!   its wrapper drop guard resolves the [`JoinHandle`]'s [`JoinState`] to
//!   `Err(Aborted)` (first-writer-wins). The SAME guard makes whole-future
//!   cancel-on-drop resolve the join identically, with zero special-casing.
//! * **tokio integration (Task 4).** [`Shared::parked`] (the executor's parked
//!   tokio waker) is wired by [`TaskWaker::signal`] now but only ever holds `None`
//!   until Task 4's `run_until`/drain loop parks a real tokio waker there.
//!
//! Nothing OUTSIDE this module calls the core yet (the seam + spawn-site
//! migration are Tasks 5–7), so the public surface is `dead_code` until then. The
//! blanket allow is REMOVED when Task 5 wires the first consumer; do not let it
//! mask genuinely-unused code added later.
//!
//! ## Miri (the soundness proof)
//!
//! The whole module is `unsafe`-free, so Miri's job is to prove the cross-thread
//! `Arc`/`AtomicBool`/`mpsc` interactions are data-race-free. Verified clean with:
//!
//! ```text
//! MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test --lib exec::
//! ```
//!
//! (`-Zmiri-disable-isolation` lets the real-thread tests query OS time/thread
//! ids.) Tests split into a Miri-able set (covers the waker's cross-thread
//! send/recv + same-thread fast path + `Arc`/`AtomicBool` paths) and a native-only
//! stress set (`cross_thread_wake_storm`, `#[cfg_attr(miri, ignore)]` — too
//! real-thread-heavy for Miri).
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

/// Matches tokio's event interval; internal tunable, NOT user-visible.
const EXEC_TICK_BUDGET: u32 = 61;

thread_local! {
    /// The (at most one) live executor on this isolate thread. Installed by the
    /// entry points via [`CurrentExecGuard`] (Task 4/5). One-executor-per-thread
    /// is an invariant, so the registry is never ambiguous. The same-thread
    /// waker fast path ([`TaskWaker::signal`]) clones the `Rc<Core>` out of here.
    static CURRENT_EXEC: RefCell<Option<Rc<Core>>> = const { RefCell::new(None) };
}

/// Install `core` as the current-thread executor. By the one-per-thread
/// invariant the registry must be empty; installing OVER a different live
/// executor is a bug, so this asserts rather than silently stomping. Re-installing
/// the SAME `Rc<Core>` (pointer-equal) is a harmless idempotent no-op.
///
/// Prefer [`CurrentExecGuard`] (RAII) at call sites; this is the primitive.
pub(crate) fn install_current(core: &Rc<Core>) {
    CURRENT_EXEC.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_ref() {
            Some(existing) => assert!(
                Rc::ptr_eq(existing, core),
                "EXEC invariant: a different executor is already installed on this thread"
            ),
            None => *slot = Some(Rc::clone(core)),
        }
    });
}

/// Clear the current-thread executor registry. Idempotent (clearing an empty
/// registry is a no-op).
pub(crate) fn uninstall_current() {
    CURRENT_EXEC.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

/// RAII guard: installs `core` on construction, uninstalls on drop. The entry
/// points (Task 4/5) hold one of these for the lifetime of a `run_until`.
pub(crate) struct CurrentExecGuard;

impl CurrentExecGuard {
    pub(crate) fn install(core: &Rc<Core>) -> Self {
        install_current(core);
        CurrentExecGuard
    }
}

impl Drop for CurrentExecGuard {
    fn drop(&mut self) {
        uninstall_current();
    }
}

/// A slab handle: a slot `index` plus the `gen` it was minted at. A stale or
/// forged `TaskId` fails the gen-check at dequeue/enqueue and is a SILENT skip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TaskId {
    index: u32,
    gen: u32,
}

/// Injection variants drained at tick start. `Abort` is wired fully in Task 3;
/// in Task 1 it only marks the task's `aborted` cell.
enum Inject {
    Wake(TaskId),
    Abort(TaskId),
}

/// The `Send + Sync` half shared with the cross-thread waker. The waker holds
/// ONLY `Arc<Shared>` — never the `!Send` `Rc<Core>` — which is what keeps the
/// `Waker` `Send + Sync`.
struct Shared {
    /// The executor's home thread (the same-thread fast path keys on this).
    thread: std::thread::ThreadId,
    injected_tx: Sender<Inject>,
    /// The executor's parked tokio waker (Task 4 parks it here; holds `None`
    /// until then). Uncontended std `Mutex` — see the `lock` handling in
    /// [`TaskWaker::wake_parked`].
    parked: Mutex<Option<Waker>>,
}

/// The per-task `Wake` impl. On wake it dedupes through `queued` (so a flurry of
/// wakes enqueues the task at most once) and then injects `Wake(id)`.
struct TaskWaker {
    id: TaskId,
    queued: Arc<AtomicBool>,
    shared: Arc<Shared>,
}

impl TaskWaker {
    fn signal(&self) {
        // Dedupe: if it was already queued, do nothing. `queued` is cleared by
        // the executor in `poll_one` BEFORE it polls the task, so a wake that
        // arrives during a poll re-queues it (the swap returns false → we enqueue
        // it below, same-thread or injected).
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }

        // SAME-THREAD FAST PATH. If the waking thread is the executor's home
        // thread, try to push straight onto `core.ready` — no channel round-trip.
        if std::thread::current().id() == self.shared.thread {
            // Clone the `Rc<Core>` OUT of the thread-local and drop the borrow
            // immediately: the executor itself may borrow `CURRENT_EXEC` (or, more
            // to the point, the `Core`'s interior cells) while a future re-enters
            // and wakes — never hold this borrow across the `enqueue_local`.
            let core = CURRENT_EXEC.with(|slot| slot.borrow().clone());
            if let Some(core) = core {
                // Confirm it is OUR executor before touching its queue: identity
                // via `Arc::ptr_eq` on the shared half. A mismatch (a different
                // executor is currently installed — shouldn't happen under the
                // one-per-thread invariant, but never push onto the wrong queue)
                // falls through to injection.
                if Arc::ptr_eq(&core.shared, &self.shared) {
                    core.enqueue_local(self.id);
                    self.wake_parked();
                    return;
                }
            }
            // Registry empty or a different executor → fall through to inject.
        }

        // CROSS-THREAD (or late / no-registry) PATH. The injection send only
        // fails if the executor (and its `Receiver`) is gone; a wake-after-
        // shutdown is a silent no-op by design.
        let _ = self.shared.injected_tx.send(Inject::Wake(self.id));
        self.wake_parked();
    }

    /// Wake the executor's parked tokio waker if one is parked there. Until Task 4
    /// parks a real waker, this slot holds `None` → a no-op. Poisoning only
    /// happens on a panic-while-held, which never occurs on this uncontended slot;
    /// we still use `if let Ok(..)` to avoid any panic path on the wake.
    fn wake_parked(&self) {
        if let Ok(mut guard) = self.shared.parked.lock() {
            if let Some(waker) = guard.take() {
                waker.wake();
            }
        }
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.signal();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.signal();
    }
}

/// The ENTIRE cross-thread soundness of the executor rests on the waker being
/// `Send + Sync` (cross-thread wakes are real — spawn_blocking, worker channels,
/// DNS; spec §3.2). `Waker::from(Arc<W>)` already requires `W: Send + Sync`, but
/// pin it explicitly so a future `!Send`/`!Sync` field added to `TaskWaker` or
/// `Shared` fails the BUILD here with a clear message rather than silently
/// regressing the design.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TaskWaker>();
    assert_send_sync::<Shared>();
    assert_send_sync::<Waker>();
};

struct TaskCell {
    future: Option<Pin<Box<dyn Future<Output = ()>>>>,
    /// Built once per task.
    waker: Waker,
    /// Dedupe flag, shared with the waker.
    queued: Arc<AtomicBool>,
    aborted: Cell<bool>,
}

struct Slot {
    gen: u32,
    cell: Option<TaskCell>,
}

/// The per-isolate task executor core: a generational slab of tasks, a FIFO
/// ready queue, and a budgeted tick loop.
pub(crate) struct Core {
    tasks: RefCell<Vec<Slot>>,
    free: RefCell<Vec<u32>>,
    ready: RefCell<VecDeque<TaskId>>,
    injected_rx: Receiver<Inject>,
    shared: Arc<Shared>,
    running: Cell<Option<TaskId>>,
    live: Cell<usize>,
    spawned_total: Cell<u64>,
}

/// The verdict of one budgeted [`Core::poll_tick`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TickOutcome {
    /// Ready queue drained but `live > 0`: tasks are parked Pending awaiting an
    /// external wake. The caller should park (await the reactor) and re-poll.
    Idle,
    /// The budget was exhausted with the ready queue still non-empty. The caller
    /// must self-yield (cooperatively) then re-poll immediately.
    MoreWork,
    /// `live == 0`: every task has retired. Nothing left to drive.
    AllDone,
}

impl Core {
    /// Build the channel pair + `Shared` keyed to the current thread.
    pub(crate) fn new() -> Rc<Core> {
        let (injected_tx, injected_rx) = std::sync::mpsc::channel();
        let shared = Arc::new(Shared {
            thread: std::thread::current().id(),
            injected_tx,
            parked: Mutex::new(None),
        });
        Rc::new(Core {
            tasks: RefCell::new(Vec::new()),
            free: RefCell::new(Vec::new()),
            ready: RefCell::new(VecDeque::new()),
            injected_rx,
            shared,
            running: Cell::new(None),
            live: Cell::new(0),
            spawned_total: Cell::new(0),
        })
    }

    /// Slab-insert a future: reuse a free slot (whose `gen` was already bumped at
    /// free time) else push a fresh one; build the task's waker; enqueue (back of
    /// the ready FIFO); bump `live` and `spawned_total`. Returns the `TaskId`.
    ///
    /// INVARIANT S1: spawn is ENQUEUE, never an inline first poll.
    pub(crate) fn insert(self: &Rc<Core>, future: Pin<Box<dyn Future<Output = ()>>>) -> TaskId {
        let queued = Arc::new(AtomicBool::new(true)); // queued from birth (we push it below)

        let id;
        {
            let mut tasks = self.tasks.borrow_mut();
            let mut free = self.free.borrow_mut();
            let index = if let Some(i) = free.pop() {
                i
            } else {
                let i = tasks.len() as u32;
                tasks.push(Slot { gen: 0, cell: None });
                i
            };
            let gen = tasks[index as usize].gen;
            id = TaskId { index, gen };

            let waker = Waker::from(Arc::new(TaskWaker {
                id,
                queued: Arc::clone(&queued),
                shared: Arc::clone(&self.shared),
            }));
            tasks[index as usize].cell = Some(TaskCell {
                future: Some(future),
                waker,
                queued,
                aborted: Cell::new(false),
            });
        }

        self.ready.borrow_mut().push_back(id);
        self.live.set(self.live.get() + 1);
        self.spawned_total.set(self.spawned_total.get() + 1);
        id
    }

    /// Push an id onto the ready FIFO (back). Gen-checked at DEQUEUE, not here.
    pub(crate) fn enqueue_local(&self, id: TaskId) {
        self.ready.borrow_mut().push_back(id);
    }

    /// Same-thread abort request (S3). NEVER drops the future synchronously
    /// (dropping a future mid-`poll` is instant UB); marks + arranges a dequeue,
    /// and the executor drops it at `poll_one`. Cases:
    ///
    /// 1. Task NOT currently running → mark `aborted` (gen-checked) AND enqueue,
    ///    so `poll_one` dequeues it, sees the mark, and `retire`s (drops it).
    /// 2. Task IS the currently-running one (self-abort, `running == Some(id)`) →
    ///    mark ONLY (it is mid-poll); `poll_one`'s post-poll re-check drops it.
    /// 3. Stale/forged/already-freed id → gen-check fails → silent no-op
    ///    (double-abort / abort-after-complete idempotency).
    fn request_abort(&self, id: TaskId) {
        // Mark `aborted` iff the slot is gen-matching and live.
        let marked = {
            let tasks = self.tasks.borrow();
            match tasks.get(id.index as usize) {
                Some(slot) if slot.gen == id.gen => match &slot.cell {
                    Some(cell) => {
                        cell.aborted.set(true);
                        true
                    }
                    None => false,
                },
                _ => false,
            }
        };
        if !marked {
            return; // stale / freed → no-op
        }
        // Case 2: self-abort of the running task → mark only (mid-poll).
        if self.running.get() == Some(id) {
            return;
        }
        // Case 1: not running → enqueue so `poll_one` dequeues + drops it. A
        // double-enqueue is harmless (the second dequeue gen-fails after the
        // first retire frees the slot).
        self.enqueue_local(id);
    }

    /// Drain the injection channel (non-blocking). `Wake(id)` → enqueue iff the
    /// slot's gen matches and its cell is live; `Abort(id)` → mark `aborted` AND
    /// enqueue (gen-checked) so `poll_one` dequeues + `retire`s (drops) it — its
    /// drop guard then resolves the join to `Err(Aborted)` (Task 3).
    pub(crate) fn drain_injected(&self) {
        while let Ok(msg) = self.injected_rx.try_recv() {
            match msg {
                Inject::Wake(id) => {
                    if self.slot_is_live(id) {
                        self.ready.borrow_mut().push_back(id);
                    }
                    // else: stale gen / completed cell → wake-after-complete no-op.
                }
                Inject::Abort(id) => {
                    // Mark `aborted` (gen-checked) AND enqueue so `poll_one`
                    // dequeues it and `retire`s (drops the future → its drop
                    // guard resolves the join to `Err(Aborted)`). Without the
                    // enqueue a parked task would never be dequeued/dropped.
                    let mark = {
                        let tasks = self.tasks.borrow();
                        match tasks.get(id.index as usize) {
                            Some(slot) if slot.gen == id.gen => {
                                if let Some(cell) = &slot.cell {
                                    cell.aborted.set(true);
                                    true
                                } else {
                                    false
                                }
                            }
                            _ => false,
                        }
                    };
                    if mark {
                        self.ready.borrow_mut().push_back(id);
                    }
                }
            }
        }
    }

    /// True iff `id` names a live (gen-matching, cell-present) slot.
    fn slot_is_live(&self, id: TaskId) -> bool {
        let tasks = self.tasks.borrow();
        matches!(tasks.get(id.index as usize), Some(s) if s.gen == id.gen && s.cell.is_some())
    }

    /// Free a slot: drop its cell, bump `gen` (wrapping — this is what makes a
    /// stale `TaskId` fail the dequeue gen-check), push the index on the free list.
    fn free_slot(&self, index: u32) {
        let mut tasks = self.tasks.borrow_mut();
        if let Some(slot) = tasks.get_mut(index as usize) {
            slot.cell = None;
            slot.gen = slot.gen.wrapping_add(1);
        }
        drop(tasks);
        self.free.borrow_mut().push(index);
    }

    /// Pop the front of the ready queue and drive it once.
    ///
    /// Return-bool contract: `true` iff a task was actually POLLED or RETIRED
    /// this call (so [`Core::poll_tick`] counts it against the budget); `false`
    /// for a no-op — an empty queue, or a stale/forged/completed id that the
    /// gen-check silently skipped.
    fn poll_one(&self) -> bool {
        let id = match self.ready.borrow_mut().pop_front() {
            Some(id) => id,
            None => return false,
        };

        // Gen-check + None-cell silent skip. Bounds-checked: a forged huge index
        // hits `get` → None → skip, never a panic.
        {
            let tasks = self.tasks.borrow();
            match tasks.get(id.index as usize) {
                Some(slot) if slot.gen == id.gen && slot.cell.is_some() => {}
                _ => return false, // stale / forged / completed → silent skip
            }
        }

        // Clear `queued` BEFORE polling so a wake DURING the poll re-queues.
        // Snapshot the waker (cheap clone) and the aborted flag without holding
        // the `tasks` borrow across the poll (the future may re-enter the Core).
        let (waker, aborted) = {
            let tasks = self.tasks.borrow();
            let cell = tasks[id.index as usize]
                .cell
                .as_ref()
                .expect("checked Some above");
            cell.queued.store(false, Ordering::Release);
            (cell.waker.clone(), cell.aborted.get())
        };

        self.running.set(Some(id));

        if aborted {
            // Task 1: a marked-abort task is dropped/freed at its dequeue.
            self.retire(id.index);
            self.running.set(None);
            return true;
        }

        // Take the future out so the slab borrow isn't held across the poll.
        let mut future = {
            let mut tasks = self.tasks.borrow_mut();
            tasks[id.index as usize]
                .cell
                .as_mut()
                .expect("checked Some above")
                .future
                .take()
        };

        let mut ctx = Context::from_waker(&waker);
        let poll = match future.as_mut() {
            Some(fut) => fut.as_mut().poll(&mut ctx),
            // A re-entered/already-taken future (shouldn't happen single-thread,
            // but never panic): treat as nothing to do.
            None => Poll::Ready(()),
        };

        match poll {
            Poll::Ready(()) => {
                drop(future); // drop the completed future before freeing the slot
                self.retire(id.index);
            }
            Poll::Pending => {
                // Post-poll abort re-check (S3 self-abort case). A task that
                // aborted ITS OWN handle mid-poll cannot be dropped during the
                // poll (dropping a future mid-`poll` is instant UB), so
                // `request_abort` only marked the cell. Now that the poll has
                // returned, if the abort flag is newly set we `retire` (drop the
                // future here) instead of re-parking a marked-but-live task that
                // would only drop on a later wake (a latent leak). The future's
                // drop guard resolves the join to `Err(Aborted)`.
                let aborted_now = {
                    let tasks = self.tasks.borrow();
                    tasks
                        .get(id.index as usize)
                        .and_then(|s| s.cell.as_ref())
                        .map(|c| c.aborted.get())
                        .unwrap_or(false)
                };
                if aborted_now {
                    drop(future); // drop the pending future before freeing the slot
                    self.retire(id.index);
                } else {
                    // Put the (still-live) future back; a wake re-queues it.
                    let mut tasks = self.tasks.borrow_mut();
                    if let Some(cell) = tasks
                        .get_mut(id.index as usize)
                        .and_then(|s| s.cell.as_mut())
                    {
                        cell.future = future;
                    }
                    // else: the cell was freed under us → drop future.
                }
            }
        }

        self.running.set(None);
        true
    }

    /// Drop a live task and reclaim its slot: `live -= 1`, then `free_slot`.
    fn retire(&self, index: u32) {
        if self.live.get() > 0 {
            self.live.set(self.live.get() - 1);
        }
        self.free_slot(index);
    }

    /// Drain injected wakes, then poll up to `budget` ready tasks. See
    /// [`TickOutcome`] for the verdict semantics.
    pub(crate) fn poll_tick(&self, budget: u32) -> TickOutcome {
        self.drain_injected();

        let mut used = 0u32;
        while used < budget {
            if self.ready.borrow().is_empty() {
                break;
            }
            if self.poll_one() {
                used += 1;
            }
            // A no-op `poll_one` (stale id) did not consume budget; loop continues.
        }

        if self.live.get() == 0 {
            TickOutcome::AllDone
        } else if !self.ready.borrow().is_empty() {
            TickOutcome::MoreWork
        } else {
            TickOutcome::Idle
        }
    }

    /// Total tasks ever inserted (for the later coverage assertion).
    pub(crate) fn spawned_total(&self) -> u64 {
        self.spawned_total.get()
    }

    /// Test-only probe: current length of the ready FIFO (no drain).
    #[cfg(test)]
    fn ready_len(&self) -> usize {
        self.ready.borrow().len()
    }

    /// Test-only probe: true iff the injection channel currently has NO pending
    /// message. Used to prove the same-thread fast path bypassed injection.
    /// Non-destructive: it cannot peek the mpsc channel without draining, so it
    /// drains into `ready` exactly as `drain_injected` would and reports whether
    /// anything was found. Call it ONLY when asserting "injection was empty".
    #[cfg(test)]
    fn drain_injected_was_empty(&self) -> bool {
        let before = self.ready.borrow().len();
        self.drain_injected();
        self.ready.borrow().len() == before
    }
}

// ---------------------------------------------------------------------------
// Task 3: the handle surface — JoinHandle<T> / AbortHandle / spawn_local_on,
// and deferred-drop abort (S3).
// ---------------------------------------------------------------------------

/// The error a [`JoinHandle`] yields when its task was aborted (explicit abort
/// or cancel-on-drop). Unit type — the bespoke analogue of tokio's cancelled
/// `JoinError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Aborted;

/// Per-task join channel: the output slot + the parked joiner's waker + a
/// finished flag (for `is_finished()`). `Rc` — every join site is same-thread
/// (spec §5.2 inventory).
struct JoinState<T> {
    slot: RefCell<Option<Result<T, Aborted>>>,
    waiter: RefCell<Option<Waker>>,
    finished: Cell<bool>,
}

impl<T> JoinState<T> {
    fn new() -> Rc<Self> {
        Rc::new(JoinState {
            slot: RefCell::new(None),
            waiter: RefCell::new(None),
            finished: Cell::new(false),
        })
    }

    /// First-writer-wins resolution of the join slot, then wake the parked
    /// joiner. Used both by the wrapper on `Ready(output)` (with `Ok`) and by
    /// the drop guard on an un-finished drop (with `Err(Aborted)`).
    fn resolve(&self, result: Result<T, Aborted>) {
        {
            let mut slot = self.slot.borrow_mut();
            if slot.is_some() {
                return; // first-writer-wins
            }
            *slot = Some(result);
        }
        // Wake the parked joiner (if any) outside the slot borrow.
        if let Some(waker) = self.waiter.borrow_mut().take() {
            waker.wake();
        }
    }
}

/// The await/abort surface for a spawned task. `!Send` (every call site is
/// same-thread; spec §5.2). Dropping a `JoinHandle` does NOT abort the task
/// (tokio parity — cancel-on-drop lives at the higher SharedFuture layer).
pub(crate) struct JoinHandle<T> {
    inner: JoinInner<T>,
}

enum JoinInner<T> {
    Bespoke {
        state: Rc<JoinState<T>>,
        id: TaskId,
        shared: Arc<Shared>,
    },
    /// Seam fallback (Task 5 wires it; the variant exists now).
    Tokio(tokio::task::JoinHandle<T>),
}

impl<T> JoinHandle<T> {
    /// A cheap, `Send + Sync` handle that can abort this task from any thread.
    pub(crate) fn abort_handle(&self) -> AbortHandle {
        match &self.inner {
            JoinInner::Bespoke { id, shared, .. } => AbortHandle {
                inner: AbortInner::Bespoke {
                    id: *id,
                    shared: Arc::clone(shared),
                },
            },
            JoinInner::Tokio(h) => AbortHandle {
                inner: AbortInner::Tokio(h.abort_handle()),
            },
        }
    }

    /// Abort the task (deferred-drop; never drops the future synchronously).
    pub(crate) fn abort(&self) {
        self.abort_handle().abort();
    }

    /// True once the task has finished (completed OR resolved as aborted).
    pub(crate) fn is_finished(&self) -> bool {
        match &self.inner {
            JoinInner::Bespoke { state, .. } => state.finished.get(),
            JoinInner::Tokio(h) => h.is_finished(),
        }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, Aborted>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `JoinHandle` is `Unpin` for the bespoke arm (the fields are `Rc`/`Arc`/
        // `TaskId`); the tokio arm's `JoinHandle` is itself `Unpin`.
        match &mut self.get_mut().inner {
            JoinInner::Bespoke { state, .. } => {
                if let Some(result) = state.slot.borrow_mut().take() {
                    return Poll::Ready(result);
                }
                *state.waiter.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
            JoinInner::Tokio(h) => match Pin::new(h).poll(cx) {
                Poll::Ready(Ok(v)) => Poll::Ready(Ok(v)),
                // A cancelled/panicked tokio task maps to `Aborted` (the bespoke
                // surface has no panic-carrying variant).
                Poll::Ready(Err(_)) => Poll::Ready(Err(Aborted)),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

/// A cheap, clonable abort handle. MUST stay `Send + Sync` (tokio's is) — a
/// cross-thread abort lands via the injection channel.
#[derive(Clone)]
pub(crate) struct AbortHandle {
    inner: AbortInner,
}

#[derive(Clone)]
enum AbortInner {
    Bespoke { id: TaskId, shared: Arc<Shared> },
    Tokio(tokio::task::AbortHandle),
}

impl AbortHandle {
    /// Request abort. Deferred-drop: NEVER drops the future synchronously.
    ///
    /// * Same-thread, task not running → mark + enqueue (drops at next dequeue).
    /// * Same-thread, task IS running (self-abort) → mark only (poll_one's
    ///   post-poll re-check drops it).
    /// * Cross-thread → inject `Abort(id)`; `drain_injected` marks + enqueues.
    /// * Stale/forged/completed id → gen-checked silent no-op (double-abort /
    ///   abort-after-complete idempotency).
    pub(crate) fn abort(&self) {
        match &self.inner {
            AbortInner::Bespoke { id, shared } => {
                // Same-thread fast path: route through the installed `Core` so we
                // can distinguish the self-abort (running) case and avoid the
                // channel round-trip. Confirm identity via `Arc::ptr_eq`.
                if std::thread::current().id() == shared.thread {
                    let core = CURRENT_EXEC.with(|slot| slot.borrow().clone());
                    if let Some(core) = core {
                        if Arc::ptr_eq(&core.shared, shared) {
                            core.request_abort(*id);
                            return;
                        }
                    }
                    // Registry empty / different executor → fall through to inject.
                }
                // Cross-thread (or no-registry) path: inject. `drain_injected`
                // marks + enqueues (gen-checked). A send-after-shutdown is a
                // silent no-op (the executor + its receiver are gone).
                let _ = shared.injected_tx.send(Inject::Abort(*id));
            }
            AbortInner::Tokio(h) => h.abort(),
        }
    }
}

/// `AbortHandle` is fired from foreign threads (worker channels, blocking-pool
/// completions). Pin `Send + Sync` so a future non-`Send`/`!Sync` field fails
/// the BUILD here rather than silently regressing the design.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AbortHandle>();
};

/// Spawn `fut` onto `core`. The user future (`Output = T`) is wrapped into a
/// `Future<Output = ()>` for [`Core::insert`] that (a) on `Ready(output)` stores
/// `Ok(output)` into the join state, sets `finished`, wakes the joiner, and
/// DISARMS the drop guard; and (b) carries a drop guard that, if the wrapper
/// drops UN-finished (abort-at-dequeue OR whole-future cancel-on-drop), resolves
/// the join to `Err(Aborted)` (first-writer-wins) and wakes the joiner — so both
/// abort paths resolve the join with zero special-casing, exactly at the drop
/// point (spec §3.1).
pub(crate) fn spawn_local_on<T, F>(core: &Rc<Core>, fut: F) -> JoinHandle<T>
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    let state = JoinState::<T>::new();

    let wrapper = JoinWrapper {
        fut: Box::pin(fut),
        guard: AbortDropGuard {
            state: Rc::clone(&state),
            armed: true,
        },
    };

    let id = core.insert(Box::pin(wrapper));

    JoinHandle {
        inner: JoinInner::Bespoke {
            state,
            id,
            shared: Arc::clone(&core.shared),
        },
    }
}

/// The wrapper future stored in the slab. Drives the user future; on completion
/// resolves the join `Ok` and disarms the guard; on ANY drop while still armed
/// the guard resolves the join `Err(Aborted)`.
struct JoinWrapper<T> {
    fut: Pin<Box<dyn Future<Output = T>>>,
    guard: AbortDropGuard<T>,
}

/// Resolves the join to `Err(Aborted)` on drop unless disarmed (the wrapper
/// disarms it on normal completion). This is the single mechanism that makes
/// BOTH abort-at-dequeue and whole-future cancel-on-drop resolve the join.
struct AbortDropGuard<T> {
    state: Rc<JoinState<T>>,
    armed: bool,
}

impl<T> Drop for AbortDropGuard<T> {
    fn drop(&mut self) {
        if self.armed {
            // Un-finished-normally drop → aborted. Set `finished` (an aborted
            // task IS finished — tokio's `is_finished()` is true post-cancel)
            // then resolve `Err(Aborted)` (first-writer-wins inside `resolve`).
            self.state.finished.set(true);
            self.state.resolve(Err(Aborted));
        }
    }
}

impl<T> Future for JoinWrapper<T> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // `JoinWrapper` is `Unpin` (the boxed future is `Unpin`, the guard holds
        // only `Rc`/`bool`), so `get_mut` is safe.
        let this = self.get_mut();
        match this.fut.as_mut().poll(cx) {
            Poll::Ready(output) => {
                this.guard.state.finished.set(true);
                this.guard.state.resolve(Ok(output));
                this.guard.armed = false; // disarm: normal completion, not abort
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// Task 4: the executor wired as ONE long-lived future driven by the existing
// `LocalSet` (Architecture B). `run_until` is the bespoke analogue of
// `LocalSet::run_until`, `drain` of the trailing `local.await`; both carry the
// tick-budget driver-liveness yield (§2.3) and the lost-wakeup park re-check
// (mirroring `ResultCell::get`, `src/task.rs`). tokio stays the I/O + timer
// driver — the executor is just an ordinary future.
// ---------------------------------------------------------------------------

/// The bespoke per-isolate executor: a thin `Rc<Core>` wrapper that an entry
/// point drives via [`exec_run_until`] inside the unchanged
/// `local.run_until(...)`. `!Send` like everything in the isolate.
pub(crate) struct Executor {
    core: Rc<Core>,
}

impl Executor {
    pub(crate) fn new() -> Self {
        Executor { core: Core::new() }
    }

    /// The shared `Core` (the GC root / spawn target for Task 5's seam).
    pub(crate) fn core(&self) -> &Rc<Core> {
        &self.core
    }

    /// RAII-install this executor into [`CURRENT_EXEC`] for the isolate thread
    /// (so same-thread wakes take the fast path). The guard uninstalls on drop.
    pub(crate) fn install(&self) -> CurrentExecGuard {
        CurrentExecGuard::install(&self.core)
    }

    /// Spawn `fut` onto THIS executor. (Task 5's `spawn_local` seam routes
    /// through [`CURRENT_EXEC`]; this is the direct form used by tests + the
    /// entry point that owns the `Executor`.)
    pub(crate) fn spawn<F>(&self, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        spawn_local_on(&self.core, fut)
    }

    /// Drive `root` to completion while ticking the spawned-task FIFO — the
    /// bespoke analogue of `LocalSet::run_until`. Returns the moment ROOT
    /// completes; any still-live spawned tasks are left for [`drain`](Self::drain)
    /// (exactly like `LocalSet::run_until` followed by `local.await`).
    ///
    /// On each executor poll (see the inline numbered steps): un-park, drain
    /// injected wakes, poll `root` FIRST, then `poll_tick` the FIFO; `MoreWork`
    /// self-yields so tokio's I/O + timer driver gets a turn (§2.3 driver
    /// liveness), `Idle`/`AllDone`-with-root-pending parks with the lost-wakeup
    /// re-check.
    pub(crate) async fn run_until<F: Future>(&self, root: F) -> F::Output {
        let core = Rc::clone(&self.core);
        let mut root = Box::pin(root);
        std::future::poll_fn(move |cx| {
            // 1. Un-park: we are running, not parked. Clear any stale waker so a
            //    wake that arrives WHILE we run re-queues (onto `ready`) rather
            //    than firing a no-longer-parked waker.
            core.clear_parked();
            // 2. Fold cross-thread/late wakes into `ready`.
            core.drain_injected();
            // 3. Poll the root FIRST (the `run_until` shape: root is driven
            //    before the FIFO on each wake).
            if let Poll::Ready(out) = root.as_mut().poll(cx) {
                return Poll::Ready(out);
            }
            // 4. Process spawned tasks under the budget.
            match core.poll_tick(EXEC_TICK_BUDGET) {
                // 5a. Budget exhausted, queue non-empty → self-yield so tokio
                //     gives the I/O + timer driver a turn, then re-polls us.
                TickOutcome::MoreWork => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                // 5b. Idle (or AllDone but root still Pending) → park with the
                //     lost-wakeup re-check.
                TickOutcome::Idle | TickOutcome::AllDone => {
                    core.park_with_recheck(cx);
                    Poll::Pending
                }
            }
        })
        .await
    }

    /// Drive the spawned-task FIFO until every task has retired (`live == 0`) —
    /// the bespoke analogue of the trailing `local.await`. Same loop as
    /// [`run_until`](Self::run_until) WITHOUT a root: `AllDone` → ready,
    /// `MoreWork` → self-yield, `Idle` → park with the lost-wakeup re-check.
    pub(crate) async fn drain(&self) {
        let core = Rc::clone(&self.core);
        std::future::poll_fn(move |cx| {
            core.clear_parked();
            core.drain_injected();
            match core.poll_tick(EXEC_TICK_BUDGET) {
                TickOutcome::AllDone => Poll::Ready(()),
                TickOutcome::MoreWork => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                TickOutcome::Idle => {
                    core.park_with_recheck(cx);
                    Poll::Pending
                }
            }
        })
        .await
    }
}

/// Caller-facing wrapper applying the coop opt-out (spec §2.3). tokio's
/// cooperative-scheduling budget would, after ~128 ready-polls, force OUR
/// `poll_fn` to return `Pending` (via the task budget) even when the executor
/// has real work and root is unfinished — stalling the isolate until the next
/// tokio tick. `unconstrained` opts the executor future out of that budget: the
/// executor runs its OWN budget (`EXEC_TICK_BUDGET` + the `MoreWork`
/// self-yield), which is the driver-liveness mechanism here. Task 5's entry
/// points call THIS, not `run_until` directly.
pub(crate) fn exec_run_until<'a, F: Future + 'a>(
    exec: &'a Executor,
    root: F,
) -> impl Future<Output = F::Output> + 'a {
    tokio::task::unconstrained(exec.run_until(root))
}

impl Core {
    /// Take + drop the executor's parked tokio waker (we are running, not
    /// parked). Idempotent; poisoning never occurs on this uncontended slot but
    /// is handled without panicking regardless.
    fn clear_parked(&self) {
        if let Ok(mut guard) = self.shared.parked.lock() {
            *guard = None;
        }
    }

    /// Park the executor: store this poll's tokio waker into [`Shared::parked`],
    /// THEN re-check for work that landed in the window between the last drain
    /// and storing the waker (the lost-wakeup guard — mirrors `ResultCell::get`,
    /// `src/task.rs`). If a wake DID land, don't truly park: self-wake + Pending
    /// so tokio re-polls us immediately. Otherwise genuinely park (a future leaf
    /// or cross-thread wake will [`TaskWaker::wake_parked`] → re-poll).
    fn park_with_recheck(&self, cx: &mut Context<'_>) {
        // Store the parking waker FIRST so any wake from here on can find it.
        if let Ok(mut guard) = self.shared.parked.lock() {
            *guard = Some(cx.waker().clone());
        }
        // Re-check: a same-thread wake may have pushed onto `ready`, or a
        // cross-thread wake may sit on the injection channel, AFTER our last
        // drain but BEFORE we stored the waker above. Fold injected in and look.
        self.drain_injected();
        if !self.ready.borrow().is_empty() {
            // A wake landed in the window — we have work; don't truly park.
            cx.waker().wake_by_ref();
        }
        // The caller returns `Poll::Pending`.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell as StdRefCell;
    use std::task::Poll;

    /// A future that runs `f` exactly once on first poll, then is Ready.
    struct OnceFut<F: FnMut()> {
        f: Option<F>,
    }
    fn once<F: FnMut() + Unpin>(f: F) -> Pin<Box<OnceFut<F>>> {
        Box::pin(OnceFut { f: Some(f) })
    }
    // `OnceFut<F>` is `Unpin` (a closure is `Unpin`), so `get_mut` is safe.
    impl<F: FnMut() + Unpin> Future for OnceFut<F> {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
            if let Some(mut f) = self.get_mut().f.take() {
                f();
            }
            Poll::Ready(())
        }
    }

    /// S1: spawn is enqueue, never an inline first poll.
    #[test]
    fn spawn_is_enqueue_not_run() {
        let core = Core::new();
        let flag = Rc::new(Cell::new(false));
        let f = Rc::clone(&flag);
        core.insert(once(move || f.set(true)));
        assert!(!flag.get(), "insert must NOT poll the future");
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);
        assert!(flag.get(), "tick must run the task");
    }

    /// Three tasks run in INSERTION order.
    #[test]
    fn fifo_order() {
        let core = Core::new();
        let log: Rc<StdRefCell<Vec<u32>>> = Rc::new(StdRefCell::new(Vec::new()));
        for n in 0..3u32 {
            let l = Rc::clone(&log);
            core.insert(once(move || l.borrow_mut().push(n)));
        }
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);
        assert_eq!(*log.borrow(), vec![0, 1, 2]);
    }

    /// Completion frees the slot, bumps gen, and a stale TaskId is rejected.
    #[test]
    fn completion_frees_slot_and_bumps_gen() {
        let core = Core::new();
        let id = core.insert(once(|| {}));
        let stale = id; // captured before completion
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);

        // Slot index is on the free list and gen incremented.
        assert!(core.free.borrow().contains(&id.index));
        assert_eq!(core.tasks.borrow()[id.index as usize].gen, id.gen + 1);

        // A retained stale TaskId is rejected at enqueue+dequeue (no panic).
        core.enqueue_local(stale);
        let out = core.poll_tick(EXEC_TICK_BUDGET);
        // live was already 0; the stale id is a silent skip → AllDone, no panic.
        assert_eq!(out, TickOutcome::AllDone);
    }

    /// A self-waking 2-poll task re-runs only after later-queued tasks (FIFO,
    /// back-of-queue).
    #[test]
    fn wake_requeues_at_back() {
        // First poll: wake self + return Pending. Second poll: log + Ready.
        struct TwoPoll {
            polls: u32,
            log: Rc<StdRefCell<Vec<&'static str>>>,
        }
        impl Future for TwoPoll {
            type Output = ();
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                let this = self.get_mut(); // TwoPoll is Unpin
                this.polls += 1;
                if this.polls == 1 {
                    this.log.borrow_mut().push("A-first");
                    cx.waker().wake_by_ref(); // self-wake → injected
                    Poll::Pending
                } else {
                    this.log.borrow_mut().push("A-second");
                    Poll::Ready(())
                }
            }
        }

        let core = Core::new();
        let log: Rc<StdRefCell<Vec<&'static str>>> = Rc::new(StdRefCell::new(Vec::new()));
        core.insert(Box::pin(TwoPoll {
            polls: 0,
            log: Rc::clone(&log),
        }));
        // A later-inserted task that must run BEFORE A's second poll.
        let l = Rc::clone(&log);
        core.insert(once(move || l.borrow_mut().push("B")));

        // First tick: polls A (Pending, self-wakes via channel) then B (Ready).
        // A's wake is folded in at the NEXT drain; A is not yet re-run.
        let _ = core.poll_tick(EXEC_TICK_BUDGET);
        // Second tick: drains A's injected wake, re-polls A.
        let _ = core.poll_tick(EXEC_TICK_BUDGET);

        assert_eq!(*log.borrow(), vec!["A-first", "B", "A-second"]);
    }

    /// More than budget tasks ready → one tick retires at most `budget` and
    /// reports MoreWork.
    #[test]
    fn tick_budget_yields() {
        let core = Core::new();
        let count = EXEC_TICK_BUDGET + 5;
        let log: Rc<StdRefCell<Vec<u32>>> = Rc::new(StdRefCell::new(Vec::new()));
        for n in 0..count {
            let l = Rc::clone(&log);
            core.insert(once(move || l.borrow_mut().push(n)));
        }
        let out = core.poll_tick(EXEC_TICK_BUDGET);
        assert_eq!(out, TickOutcome::MoreWork);
        assert_eq!(log.borrow().len() as u32, EXEC_TICK_BUDGET);

        // The remainder retires on the next tick.
        let out2 = core.poll_tick(EXEC_TICK_BUDGET);
        assert_eq!(out2, TickOutcome::AllDone);
        assert_eq!(log.borrow().len() as u32, count);
    }

    /// A forged huge-index TaskId is a bounds-checked silent skip, not a panic.
    #[test]
    fn forged_id_is_silent_skip() {
        let core = Core::new();
        // No tasks exist; enqueue a wildly out-of-range id.
        core.enqueue_local(TaskId {
            index: u32::MAX,
            gen: 0,
        });
        let out = core.poll_tick(EXEC_TICK_BUDGET);
        assert_eq!(out, TickOutcome::AllDone); // live == 0, no panic
        assert_eq!(core.spawned_total(), 0);
    }

    /// An aborted task is dropped/freed at its next dequeue (Task 1 behavior).
    #[test]
    fn abort_marks_then_frees_at_dequeue() {
        let core = Core::new();
        let ran = Rc::new(Cell::new(false));
        let r = Rc::clone(&ran);
        let id = core.insert(once(move || r.set(true)));
        // Mark it aborted via the injection channel.
        core.shared.injected_tx.send(Inject::Abort(id)).unwrap();
        let out = core.poll_tick(EXEC_TICK_BUDGET);
        assert_eq!(out, TickOutcome::AllDone);
        assert!(!ran.get(), "aborted task must not run its body");
        assert!(core.free.borrow().contains(&id.index));
    }

    // ------------------------------------------------------------------------
    // Task 2: the Send-safe waker (same-thread fast path + cross-thread inject).
    // ------------------------------------------------------------------------

    /// A future that is Pending until a shared `AtomicBool` flag is set, and then
    /// Ready. It does NOT self-wake; it captures its waker out to a slot so a
    /// foreign thread (or the same thread) can wake it. Used by the waker tests.
    struct CapturingFut {
        flag: Arc<AtomicBool>,
        done: Rc<Cell<bool>>,
        captured: Rc<StdRefCell<Option<Waker>>>,
    }
    impl Future for CapturingFut {
        type Output = ();
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            let this = self.get_mut();
            if this.flag.load(Ordering::Acquire) {
                this.done.set(true);
                Poll::Ready(())
            } else {
                *this.captured.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    /// install/uninstall the thread-local registry; re-installing the SAME core
    /// is idempotent.
    #[test]
    fn registry_install_uninstall() {
        let core = Core::new();
        install_current(&core);
        CURRENT_EXEC.with(|s| assert!(s.borrow().is_some()));
        // Idempotent re-install of the same core (must not panic).
        install_current(&core);
        uninstall_current();
        CURRENT_EXEC.with(|s| assert!(s.borrow().is_none()));
        // The RAII guard installs then uninstalls on drop.
        {
            let _g = CurrentExecGuard::install(&core);
            CURRENT_EXEC.with(|s| assert!(s.borrow().is_some()));
        }
        CURRENT_EXEC.with(|s| assert!(s.borrow().is_none()));
    }

    /// Installing a DIFFERENT executor over a live one is an invariant violation.
    #[test]
    #[should_panic(expected = "a different executor is already installed")]
    fn registry_double_install_different_panics() {
        let a = Core::new();
        let b = Core::new();
        let _g = CurrentExecGuard::install(&a);
        install_current(&b); // must panic
    }

    /// A same-thread wake (the executor installed in `CURRENT_EXEC`) pushes
    /// STRAIGHT onto `ready` and does NOT go through the injection channel.
    #[test]
    fn same_thread_wake_pushes_local() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        let flag = Arc::new(AtomicBool::new(false));
        let done = Rc::new(Cell::new(false));
        let captured: Rc<StdRefCell<Option<Waker>>> = Rc::new(StdRefCell::new(None));
        core.insert(Box::pin(CapturingFut {
            flag: Arc::clone(&flag),
            done: Rc::clone(&done),
            captured: Rc::clone(&captured),
        }));

        // First tick: the task polls Pending and captures its waker. `live > 0`,
        // ready is now empty → Idle.
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::Idle);
        assert_eq!(core.ready_len(), 0);

        // Wake it from the SAME thread (we ARE the executor thread, and the core
        // is installed). The dedupe flag was cleared at poll, so this re-queues.
        let waker = captured.borrow().clone().expect("waker captured");
        flag.store(true, Ordering::Release);
        waker.wake();

        // PROOF the fast path was taken: the id is already on `ready` directly,
        // AND the injection channel is empty (drain finds nothing).
        assert_eq!(core.ready_len(), 1, "fast path pushed straight onto ready");
        assert!(
            core.drain_injected_was_empty(),
            "same-thread wake must NOT use the injection channel"
        );

        // Re-poll completes the task.
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);
        assert!(done.get());
    }

    /// With NO executor installed, a same-thread wake falls through to injection
    /// (the registry is empty → cannot take the fast path).
    #[test]
    fn same_thread_wake_no_registry_injects() {
        let core = Core::new();
        // Deliberately do NOT install into CURRENT_EXEC.

        let flag = Arc::new(AtomicBool::new(false));
        let done = Rc::new(Cell::new(false));
        let captured: Rc<StdRefCell<Option<Waker>>> = Rc::new(StdRefCell::new(None));
        core.insert(Box::pin(CapturingFut {
            flag: Arc::clone(&flag),
            done: Rc::clone(&done),
            captured: Rc::clone(&captured),
        }));
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::Idle);

        let waker = captured.borrow().clone().expect("waker captured");
        flag.store(true, Ordering::Release);
        waker.wake();

        // No registry → the wake went through injection; `ready` is still empty
        // until the next drain.
        assert_eq!(core.ready_len(), 0, "no fast path without a registry");
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);
        assert!(done.get());
    }

    /// A wake from ANOTHER thread reaches the task via injection; a poll loop
    /// that drains injected completes it. The thread is joined (no leak).
    #[test]
    fn cross_thread_wake_lands() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        let flag = Arc::new(AtomicBool::new(false));
        let done = Rc::new(Cell::new(false));
        let captured: Rc<StdRefCell<Option<Waker>>> = Rc::new(StdRefCell::new(None));
        core.insert(Box::pin(CapturingFut {
            flag: Arc::clone(&flag),
            done: Rc::clone(&done),
            captured: Rc::clone(&captured),
        }));
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::Idle);

        // Hand a CLONE of the waker to a foreign thread; it sets the flag + wakes.
        let waker = captured.borrow().clone().expect("waker captured");
        let f = Arc::clone(&flag);
        let handle = std::thread::spawn(move || {
            f.store(true, Ordering::Release);
            waker.wake();
        });
        handle.join().expect("foreign thread joined");

        // The cross-thread wake landed on the injection channel; a tick drains it.
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);
        assert!(done.get(), "cross-thread wake completed the task");
    }

    /// A retained waker for a COMPLETED task: waking it must not panic, not
    /// requeue, and not disturb a NEW task reusing the freed slot (gen check).
    #[test]
    fn wake_after_complete_is_noop() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        let captured: Rc<StdRefCell<Option<Waker>>> = Rc::new(StdRefCell::new(None));
        // A future that captures its waker on poll then completes immediately.
        struct CaptureThenDone {
            captured: Rc<StdRefCell<Option<Waker>>>,
        }
        impl Future for CaptureThenDone {
            type Output = ();
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                *self.get_mut().captured.borrow_mut() = Some(cx.waker().clone());
                Poll::Ready(())
            }
        }
        let id = core.insert(Box::pin(CaptureThenDone {
            captured: Rc::clone(&captured),
        }));
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);

        // The slot is now free; insert a NEW task that reuses index `id.index`.
        let new_done = Rc::new(Cell::new(false));
        let nd = Rc::clone(&new_done);
        let new_id = core.insert(once(move || nd.set(true)));
        assert_eq!(new_id.index, id.index, "slot reused");
        assert_ne!(new_id.gen, id.gen, "gen bumped");

        // Fire the STALE waker (gen mismatch). No panic, no requeue.
        let stale = captured.borrow().clone().expect("waker captured");
        stale.wake();
        // The fast path's `enqueue_local` is gen-checked only at dequeue, so the
        // stale id may sit on ready; the poll silently skips it. Either way the
        // NEW task must still run exactly once and the run is undisturbed.
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);
        assert!(new_done.get(), "the new task in the reused slot ran");
    }

    /// Two wakes before the next poll yield exactly ONE ready-queue entry.
    #[test]
    fn wake_dedupes() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        let flag = Arc::new(AtomicBool::new(false));
        let done = Rc::new(Cell::new(false));
        let captured: Rc<StdRefCell<Option<Waker>>> = Rc::new(StdRefCell::new(None));
        core.insert(Box::pin(CapturingFut {
            flag: Arc::clone(&flag),
            done: Rc::clone(&done),
            captured: Rc::clone(&captured),
        }));
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::Idle);

        let waker = captured.borrow().clone().expect("waker captured");
        // Two wakes before the next drain/poll. Dedupe → at most one ready entry.
        waker.wake_by_ref();
        waker.wake_by_ref();
        assert_eq!(core.ready_len(), 1, "two wakes dedupe to one ready entry");
        assert!(core.drain_injected_was_empty());
    }

    /// A foreign-thread waker fired AND dropped AFTER the `Core` (and its
    /// `Receiver`) is gone — the `send` error is swallowed, no panic.
    #[test]
    fn late_wake_after_executor_drop() {
        // Build a core off-registry, capture a waker, then DROP the core.
        let waker = {
            let core = Core::new();
            let flag = Arc::new(AtomicBool::new(false));
            let done = Rc::new(Cell::new(false));
            let captured: Rc<StdRefCell<Option<Waker>>> = Rc::new(StdRefCell::new(None));
            core.insert(Box::pin(CapturingFut {
                flag,
                done,
                captured: Rc::clone(&captured),
            }));
            assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::Idle);
            let w = captured.borrow().clone().expect("waker captured");
            w // core dropped at end of block (Receiver gone)
        };
        // Wake from a foreign thread after the executor is gone: must not panic.
        let handle = std::thread::spawn(move || {
            waker.wake(); // injected_tx.send fails silently; wake_parked no-op
        });
        handle.join().expect("late wake did not panic");
    }

    /// A task that wakes itself mid-poll (same thread, registry installed) and
    /// returns Pending is re-queued and re-polled to completion.
    #[test]
    fn wake_during_poll_requeues() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        struct SelfWakeOnce {
            polls: u32,
            done: Rc<Cell<bool>>,
        }
        impl Future for SelfWakeOnce {
            type Output = ();
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                let this = self.get_mut();
                this.polls += 1;
                if this.polls == 1 {
                    // Mid-poll self-wake: `queued` was cleared before this poll,
                    // so the wake re-queues us via the same-thread fast path.
                    cx.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    this.done.set(true);
                    Poll::Ready(())
                }
            }
        }
        let done = Rc::new(Cell::new(false));
        core.insert(Box::pin(SelfWakeOnce {
            polls: 0,
            done: Rc::clone(&done),
        }));

        // The mid-poll same-thread wake pushes the task back onto `ready` within
        // the SAME tick, so one tick drives both polls to completion.
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);
        assert!(done.get(), "the self-waking task re-polled and completed");
    }

    /// Native-only stress: 4 threads × ~10k wakes/aborts each against a churning
    /// executor. Deterministic iteration count, no timing asserts. Too
    /// real-thread-heavy for Miri.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn cross_thread_wake_storm() {
        const THREADS: usize = 4;
        const PER_THREAD: usize = 10_000;

        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        // A pool of long-lived tasks that re-park (Pending) on each poll, each
        // exposing its waker via a shared, thread-safe slot. Foreign threads pull
        // a waker and fire it. The main thread churns: spawn/complete + drain.
        let wakers: Arc<Mutex<Vec<Waker>>> = Arc::new(Mutex::new(Vec::new()));

        // A future that re-registers its waker (into the shared pool) every poll
        // and never completes on its own — driven purely by external wakes.
        struct Parker {
            pool: Arc<Mutex<Vec<Waker>>>,
            polls: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Future for Parker {
            type Output = ();
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                let this = self.get_mut();
                this.polls.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut pool) = this.pool.lock() {
                    pool.push(cx.waker().clone());
                }
                Poll::Pending
            }
        }

        let poll_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..THREADS {
            core.insert(Box::pin(Parker {
                pool: Arc::clone(&wakers),
                polls: Arc::clone(&poll_count),
            }));
        }
        // Prime: poll once so each Parker registers a waker.
        let _ = core.poll_tick(EXEC_TICK_BUDGET);

        // Foreign wakers.
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let pool = Arc::clone(&wakers);
            handles.push(std::thread::spawn(move || {
                for _ in 0..PER_THREAD {
                    // Pull whatever waker is available (may be none transiently);
                    // wake it. Dedupe + gen checks make every wake safe.
                    let w = pool.lock().ok().and_then(|mut p| p.pop());
                    if let Some(w) = w {
                        w.wake();
                    }
                }
            }));
        }

        // Main thread churns: repeatedly drain + poll so Parkers re-register and
        // the ready queue stays bounded. Fixed iteration count (deterministic).
        for _ in 0..(THREADS * PER_THREAD) {
            let _ = core.poll_tick(EXEC_TICK_BUDGET);
            // Bound check: ready never exceeds the number of live tasks (a task
            // is enqueued at most once thanks to dedupe).
            assert!(core.ready_len() <= THREADS, "ready queue stayed bounded");
        }

        for h in handles {
            h.join().expect("storm thread joined");
        }
        // Final drain — no panic, executor still consistent.
        let _ = core.poll_tick(EXEC_TICK_BUDGET);
        // The Parkers never complete; live count is still the original 4.
        assert_eq!(core.live.get(), THREADS, "all parker tasks still live");
    }

    // ------------------------------------------------------------------------
    // Task 3: JoinHandle / AbortHandle parity + deferred-drop abort (S2/S3).
    // ------------------------------------------------------------------------

    /// Drive `core` to `AllDone` (or `MoreWork` exhaustion-safe), then poll the
    /// `JoinHandle` once with a throwaway no-op waker and return its verdict.
    /// The join state is resolved synchronously inside the wrapper before the
    /// executor reports `AllDone`, so one poll after draining always observes it.
    fn drain_then_poll_join<T>(core: &Rc<Core>, handle: &mut JoinHandle<T>) -> Poll<Result<T, Aborted>>
    where
        T: Unpin,
    {
        while let TickOutcome::MoreWork = core.poll_tick(EXEC_TICK_BUDGET) {}
        let waker = Waker::noop().clone();
        let mut cx = Context::from_waker(&waker);
        Pin::new(handle).poll(&mut cx)
    }

    /// A future whose `Drop` flips a shared flag — proves WHEN the future drops.
    struct DropTracked {
        dropped: Rc<Cell<bool>>,
        polls: u32,
        ready_after: u32,
    }
    impl Future for DropTracked {
        type Output = i32;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<i32> {
            let this = self.get_mut();
            this.polls += 1;
            if this.polls >= this.ready_after {
                Poll::Ready(42)
            } else {
                // Park: never self-wakes (driven only by an external abort).
                let _ = cx;
                Poll::Pending
            }
        }
    }
    impl Drop for DropTracked {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }

    /// S2: spawn_local_on returns a JoinHandle that yields the output; is_finished
    /// flips true after completion.
    #[test]
    fn join_handle_awaits_output() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        let dropped = Rc::new(Cell::new(false));
        let mut handle = spawn_local_on(
            &core,
            DropTracked {
                dropped: Rc::clone(&dropped),
                polls: 0,
                ready_after: 1, // Ready on first poll
            },
        );
        assert!(!handle.is_finished(), "not finished before any tick");

        let verdict = drain_then_poll_join(&core, &mut handle);
        assert_eq!(verdict, Poll::Ready(Ok(42)));
        assert!(handle.is_finished(), "finished after completion");
    }

    /// S3: aborting a queued-but-not-running task drops the future at the NEXT
    /// tick (NOT synchronously inside abort()); the join resolves Err(Aborted).
    #[test]
    fn abort_idle_task_drops_at_dequeue() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        let dropped = Rc::new(Cell::new(false));
        let mut handle = spawn_local_on(
            &core,
            DropTracked {
                dropped: Rc::clone(&dropped),
                polls: 0,
                ready_after: 9999, // never completes on its own
            },
        );

        // The task is queued (spawned) but NOT yet polled/running.
        let abort = handle.abort_handle();
        abort.abort();
        // Deferred-drop: the future has NOT dropped synchronously inside abort().
        assert!(
            !dropped.get(),
            "abort() must NOT drop the future synchronously"
        );

        // Next tick dequeues the marked task and retires it (drops the future).
        let verdict = drain_then_poll_join(&core, &mut handle);
        assert!(dropped.get(), "future dropped at the next tick");
        assert_eq!(verdict, Poll::Ready(Err(Aborted)));
        assert!(handle.is_finished(), "aborted task counts as finished");
    }

    /// S3 UB guard: a task that aborts its OWN handle mid-poll. The mark is set,
    /// the poll returns (future NOT dropped during the poll), the future drops
    /// AFTER the poll via poll_one's post-poll re-check; join resolves Aborted.
    #[test]
    fn abort_during_own_poll_defers_drop() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        // Smuggle the abort handle into the future via a shared slot.
        let handle_slot: Rc<StdRefCell<Option<AbortHandle>>> = Rc::new(StdRefCell::new(None));

        struct SelfAbort {
            abort: Rc<StdRefCell<Option<AbortHandle>>>,
            dropped: Rc<Cell<bool>>,
            dropped_during_poll: Rc<Cell<bool>>,
        }
        impl Future for SelfAbort {
            type Output = i32;
            fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<i32> {
                let this = self.get_mut();
                // Abort ourselves mid-poll.
                if let Some(a) = this.abort.borrow().as_ref() {
                    a.abort();
                }
                // If a synchronous drop happened, it would have flipped `dropped`
                // BEFORE this poll returns — record that observation.
                this.dropped_during_poll.set(this.dropped.get());
                Poll::Pending
            }
        }
        impl Drop for SelfAbort {
            fn drop(&mut self) {
                self.dropped.set(true);
            }
        }

        let dropped = Rc::new(Cell::new(false));
        let dropped_during_poll = Rc::new(Cell::new(false));
        let mut handle = spawn_local_on(
            &core,
            SelfAbort {
                abort: Rc::clone(&handle_slot),
                dropped: Rc::clone(&dropped),
                dropped_during_poll: Rc::clone(&dropped_during_poll),
            },
        );
        // Now wire the abort handle in (after spawn so the id is known).
        *handle_slot.borrow_mut() = Some(handle.abort_handle());

        let verdict = drain_then_poll_join(&core, &mut handle);
        assert!(
            !dropped_during_poll.get(),
            "future must NOT be dropped during its own poll (UB guard)"
        );
        assert!(dropped.get(), "future dropped after the poll (post-poll re-check)");
        assert_eq!(verdict, Poll::Ready(Err(Aborted)));
    }

    /// AbortHandle is Send + Sync (compile-time), and a cross-thread abort lands
    /// via injection and cancels the task. The thread is joined (no leak).
    #[test]
    fn cross_thread_abort() {
        // Compile-time Send+Sync assertion (the const _ block also asserts it).
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AbortHandle>();

        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        let dropped = Rc::new(Cell::new(false));
        let mut handle = spawn_local_on(
            &core,
            DropTracked {
                dropped: Rc::clone(&dropped),
                polls: 0,
                ready_after: 9999, // never completes on its own
            },
        );
        // Poll it once so it parks Pending (running == None afterwards).
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::Idle);

        // Abort from ANOTHER thread → lands on the injection channel.
        let abort = handle.abort_handle();
        let th = std::thread::spawn(move || {
            abort.abort();
        });
        th.join().expect("foreign abort thread joined");

        // A tick drains the injected Abort: marks + enqueues + retires (drops).
        let verdict = drain_then_poll_join(&core, &mut handle);
        assert!(dropped.get(), "cross-thread abort dropped the future");
        assert_eq!(verdict, Poll::Ready(Err(Aborted)));
    }

    /// Double-abort and abort-after-complete are silent no-ops (no panic, the
    /// resolved verdict is unchanged — first-writer-wins).
    #[test]
    fn double_abort_and_abort_after_complete_are_noops() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        // --- double abort of an idle task ---
        let mut h1 = spawn_local_on(
            &core,
            DropTracked {
                dropped: Rc::new(Cell::new(false)),
                polls: 0,
                ready_after: 9999,
            },
        );
        let a = h1.abort_handle();
        a.abort();
        a.abort(); // second abort: idempotent (flag already set; re-enqueue gen-fails)
        let v1 = drain_then_poll_join(&core, &mut h1);
        assert_eq!(v1, Poll::Ready(Err(Aborted)));

        // --- abort AFTER completion ---
        let mut h2 = spawn_local_on(
            &core,
            DropTracked {
                dropped: Rc::new(Cell::new(false)),
                polls: 0,
                ready_after: 1, // completes immediately
            },
        );
        let a2 = h2.abort_handle();
        // Drive to completion first.
        while let TickOutcome::MoreWork = core.poll_tick(EXEC_TICK_BUDGET) {}
        assert!(h2.is_finished());
        // Abort after complete: slot freed → gen-checked no-op, verdict unchanged.
        a2.abort();
        let waker = Waker::noop().clone();
        let mut cx = Context::from_waker(&waker);
        assert_eq!(Pin::new(&mut h2).poll(&mut cx), Poll::Ready(Ok(42)));
    }

    /// Dropping the WHOLE wrapper future (cancel-on-drop, simulated by dropping
    /// the Core while a task is parked) resolves the join to Err(Aborted) via the
    /// drop guard.
    #[test]
    fn join_resolves_aborted_on_cancel_drop() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        let dropped = Rc::new(Cell::new(false));
        let mut handle = spawn_local_on(
            &core,
            DropTracked {
                dropped: Rc::clone(&dropped),
                polls: 0,
                ready_after: 9999, // never completes
            },
        );
        // Park it.
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::Idle);
        assert!(!dropped.get());

        // Drop the executor: its slab drops every parked future → each wrapper's
        // drop guard fires (armed) → the join resolves Err(Aborted).
        drop(_g);
        drop(core);
        assert!(dropped.get(), "dropping the Core dropped the parked future");

        let waker = Waker::noop().clone();
        let mut cx = Context::from_waker(&waker);
        assert_eq!(
            Pin::new(&mut handle).poll(&mut cx),
            Poll::Ready(Err(Aborted)),
            "cancel-on-drop resolved the join to Aborted"
        );
    }

    /// Dropping a JoinHandle does NOT abort the task (tokio parity).
    #[test]
    fn join_handle_drop_does_not_abort() {
        let core = Core::new();
        let _g = CurrentExecGuard::install(&core);

        let ran = Rc::new(Cell::new(false));
        let r = Rc::clone(&ran);
        struct Marker {
            ran: Rc<Cell<bool>>,
        }
        impl Future for Marker {
            type Output = i32;
            fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<i32> {
                self.ran.set(true);
                Poll::Ready(7)
            }
        }
        let handle = spawn_local_on(&core, Marker { ran: r });
        // Drop the handle WITHOUT awaiting — the task must still run.
        drop(handle);
        assert!(!ran.get(), "not run before a tick");
        assert_eq!(core.poll_tick(EXEC_TICK_BUDGET), TickOutcome::AllDone);
        assert!(ran.get(), "task ran despite the JoinHandle being dropped");
    }

    // ------------------------------------------------------------------------
    // Task 4: run_until / drain inside the LocalSet — tick budget + lost-wakeup
    // re-check. These need a REAL tokio runtime (timer wheel, blocking pool,
    // cross-thread wakes), so they run under `#[tokio::test]` + a `LocalSet` and
    // are `#[cfg_attr(miri, ignore)]` (Miri cannot drive the tokio runtime; the
    // Task 1–3 core tests remain the Miri coverage).
    // ------------------------------------------------------------------------

    use std::time::Duration;

    /// Drive `fut` to completion on a `LocalSet` over the ambient
    /// `#[tokio::test]` current-thread runtime. The `LocalSet` is what makes the
    /// `!Send` executor future (and any `spawn_local` inside) legal — mirrors the
    /// Task 5 entry-point shape (`local.run_until(...).await`), so the tests
    /// exercise the production driving path. NOT a nested runtime (that would
    /// panic) — it borrows the test's runtime.
    async fn on_local<F: Future>(fut: F) -> F::Output {
        tokio::task::LocalSet::new().run_until(fut).await
    }

    /// `run_until` drives the root AND the spawned FIFO; the output is the root's,
    /// and the spawned tasks ran (their side effects landed) before root returned
    /// since root awaits a rendezvous each task resolves.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn run_until_drives_root_and_tasks() {
        let out = on_local(async {
            let exec = Executor::new();
            let _g = exec.install();

            let log: Rc<StdRefCell<Vec<u32>>> = Rc::new(StdRefCell::new(Vec::new()));
            // Each spawned task pushes its id then signals a per-task oneshot the
            // root awaits — proving the root and the FIFO are co-driven.
            let mut dones = Vec::new();
            for n in 0..3u32 {
                let (tx, rx) = tokio::sync::oneshot::channel::<()>();
                dones.push(rx);
                let l = Rc::clone(&log);
                exec.spawn(async move {
                    l.borrow_mut().push(n);
                    let _ = tx.send(());
                });
            }

            let log_root = Rc::clone(&log);
            let root = async move {
                for rx in dones {
                    let _ = rx.await;
                }
                log_root.borrow().clone()
            };
            exec_run_until(&exec, root).await
        })
        .await;
        // All three tasks ran; order is insertion (FIFO).
        assert_eq!(out, vec![0, 1, 2]);
    }

    /// S11: a detached / un-awaited task spawned by root completes during
    /// `drain()` AFTER root returned (run_until leaves survivors for drain).
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn drain_runs_survivors() {
        on_local(async {
            let exec = Executor::new();
            let _g = exec.install();

            let survivor_ran = Rc::new(Cell::new(false));
            let sr = Rc::clone(&survivor_ran);

            // Root spawns a detached task that needs a tokio tick (a 0ms sleep)
            // to complete, then returns immediately WITHOUT awaiting it.
            let root = async {
                exec.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    sr.set(true);
                });
            };
            exec_run_until(&exec, root).await;

            // The survivor has NOT necessarily completed yet (root returned first).
            // drain() drives it to completion.
            exec.drain().await;
            assert!(
                survivor_ran.get(),
                "detached survivor completed during drain()"
            );
        })
        .await;
    }

    /// A spawned task that awaits a tokio timer completes — proving the leaf
    /// future registered OUR waker with tokio's timer wheel and the park
    /// delivered the wake.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn sleep_inside_bespoke_task_fires() {
        on_local(async {
            let exec = Executor::new();
            let _g = exec.install();

            let fired = Rc::new(Cell::new(false));
            let f = Rc::clone(&fired);
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            exec.spawn(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                f.set(true);
                let _ = tx.send(());
            });

            let root = async move {
                let _ = rx.await;
            };
            exec_run_until(&exec, root).await;
            assert!(fired.get(), "timer fired inside a bespoke task");
        })
        .await;
    }

    /// §2.3 driver liveness: two tasks ping-pong via `Notify` in a bounded loop
    /// (keeping `ready` non-empty across ticks), PLUS a sleep task whose flag is
    /// the deterministic exit. If the tick budget did NOT yield to the driver the
    /// timer wheel would starve and this would hang.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn driver_liveness_under_ping_pong() {
        on_local(async {
            let exec = Executor::new();
            let _g = exec.install();

            let ping = Rc::new(tokio::sync::Notify::new());
            let pong = Rc::new(tokio::sync::Notify::new());

            // ~1000 ping-pong rounds: enough ready-poll volume to exceed the
            // budget many times over, so the timer ONLY fires if the budget
            // self-yield gives the driver turns.
            const ROUNDS: usize = 1000;
            {
                let ping = Rc::clone(&ping);
                let pong = Rc::clone(&pong);
                exec.spawn(async move {
                    for _ in 0..ROUNDS {
                        ping.notify_one();
                        pong.notified().await;
                    }
                });
            }
            {
                let ping = Rc::clone(&ping);
                let pong = Rc::clone(&pong);
                exec.spawn(async move {
                    for _ in 0..ROUNDS {
                        ping.notified().await;
                        pong.notify_one();
                    }
                });
            }

            // The deterministic exit: a timer that MUST fire despite the churn.
            let timer_fired = Rc::new(Cell::new(false));
            let tf = Rc::clone(&timer_fired);
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            exec.spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                tf.set(true);
                let _ = tx.send(());
            });

            let root = async move {
                let _ = rx.await;
            };
            exec_run_until(&exec, root).await;
            assert!(
                timer_fired.get(),
                "timer fired under ping-pong churn → budget yielded to the driver"
            );
        })
        .await;
    }

    /// Cross-thread wake end-to-end: a spawned task awaiting `spawn_blocking`
    /// completes (the blocking-pool completion wakes OUR task from another thread
    /// → injection + `wake_parked` → re-poll).
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn spawn_blocking_wakes_bespoke_task() {
        let got = on_local(async {
            let exec = Executor::new();
            let _g = exec.install();

            let (tx, rx) = tokio::sync::oneshot::channel::<i32>();
            exec.spawn(async move {
                let v = tokio::task::spawn_blocking(|| 7).await.expect("blocking");
                let _ = tx.send(v);
            });

            let root = async move { rx.await.expect("got value") };
            exec_run_until(&exec, root).await
        })
        .await;
        assert_eq!(got, 7, "cross-thread wake delivered the blocking result");
    }

    /// The targeted lost-wakeup guard: a stress loop of spawn → cross-thread wake
    /// → await. A wake landing right as the executor decides to park would be
    /// lost (and the run would hang) WITHOUT the park re-check. 100 iterations,
    /// each gated on a real cross-thread `spawn_blocking` completion.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn lost_wakeup_window_closes() {
        on_local(async {
            let exec = Executor::new();
            let _g = exec.install();

            for i in 0..100i32 {
                let (tx, rx) = tokio::sync::oneshot::channel::<i32>();
                exec.spawn(async move {
                    // The completion fires from the blocking pool (another
                    // thread) — the wake may land in the park window.
                    let v = tokio::task::spawn_blocking(move || i).await.unwrap_or(-1);
                    let _ = tx.send(v);
                });
                let root = async move { rx.await.expect("no hang") };
                let v = exec_run_until(&exec, root).await;
                assert_eq!(v, i, "iteration {i} completed without hanging");
            }
        })
        .await;
    }
}
