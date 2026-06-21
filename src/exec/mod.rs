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
//! ## Task-1 carry-forward shortcuts (flagged for later tasks)
//!
//! * **Abort (Task 1 partial → Task 3).** `Inject::Abort(id)` marks the task's
//!   `aborted` cell (gen-checked). A marked task is dropped + freed at its next
//!   dequeue in [`Core::poll_one`]. Task 3 wires immediate abort drop + the
//!   abort handle plumbing.
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

    /// Drain the injection channel (non-blocking). `Wake(id)` → enqueue iff the
    /// slot's gen matches and its cell is live; `Abort(id)` → mark `aborted` iff
    /// the gen matches (the drop/free happens at next dequeue in Task 1).
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
                    let tasks = self.tasks.borrow();
                    if let Some(slot) = tasks.get(id.index as usize) {
                        if slot.gen == id.gen {
                            if let Some(cell) = &slot.cell {
                                cell.aborted.set(true);
                            }
                        }
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
                // Put the (still-live) future back; a wake re-queues it.
                let mut tasks = self.tasks.borrow_mut();
                if let Some(cell) = tasks
                    .get_mut(id.index as usize)
                    .and_then(|s| s.cell.as_mut())
                {
                    cell.future = future;
                }
                // else: the cell was freed under us (abort during poll) → drop future.
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
}
