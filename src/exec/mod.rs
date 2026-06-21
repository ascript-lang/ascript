//! EXEC: the bespoke per-isolate task executor (spec
//! `superpowers/specs/2026-06-12-vm-executor-design.md`). Replaces tokio's task
//! HARNESS (spawn/wake/yield) for AScript tasks; tokio remains the I/O + timer
//! driver. `!Send` like the whole runtime; exactly one per isolate thread.
//! Task 1 = the core (slab + FIFO queue + tick). The Send-safe cross-thread
//! waker (same-thread fast path + lock-free injection) is Task 2; tokio
//! run_until/drain integration is Task 4.
//!
//! ## Task-1 scope and shortcuts (flagged for later tasks)
//!
//! * **Waker (Task 1 shortcut → Task 2).** A polled future needs a [`Waker`].
//!   For Task 1 the per-task waker is built from a SAFE [`std::task::Wake`] impl
//!   ([`TaskWaker`]) that, on wake, dedupes via its `queued` flag and then pushes
//!   `Inject::Wake(id)` onto the executor's mpsc injection channel. The wake is
//!   folded into the ready queue at the NEXT [`Core::drain_injected`] (tick
//!   start). This is correct and `Send`, but it always routes a self-wake
//!   through the channel rather than the same-thread fast path. Task 2 adds the
//!   `CURRENT_EXEC` thread-local fast path (push straight onto `ready` when the
//!   waking thread is the executor thread) + the lock-free cross-thread
//!   injection + the dedupe-during-poll protocol. The `queued`/`Shared`/
//!   `injected_tx`/`injected_rx`/`parked` fields are all in place now so Task 2/3/4
//!   EXTEND rather than restructure.
//! * **Abort (Task 1 partial → Task 3).** `Inject::Abort(id)` marks the task's
//!   `aborted` cell (gen-checked). A marked task is dropped + freed at its next
//!   dequeue in [`Core::poll_one`]. Task 3 wires immediate abort drop + the
//!   abort handle plumbing.
//! * **tokio integration (Task 4).** `Shared::parked` (the executor's parked
//!   tokio waker) and `Shared::thread` are unused in Task 1; Task 4's
//!   `run_until`/drain loop parks the tokio waker there and the cross-thread
//!   injection path wakes it.
//!
//! Task 1 ships the core in isolation — nothing OUTSIDE this module calls it yet
//! (the seam + spawn-site migration are Tasks 5–7), so the public surface is
//! `dead_code` until then. The blanket allow is REMOVED when Task 5 wires the
//! first consumer; do not let it mask genuinely-unused code added later.
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

/// The `Send + Sync` half shared with the (cross-thread, Task 2) waker.
struct Shared {
    /// The executor's home thread (Task 2's same-thread fast path keys on this).
    #[allow(dead_code)]
    thread: std::thread::ThreadId,
    injected_tx: Sender<Inject>,
    /// The executor's parked tokio waker (Task 4 uses it); uncontended std Mutex.
    #[allow(dead_code)]
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
        // the executor BEFORE it polls the task, so a wake that arrives during a
        // poll re-queues it (the swap returns false → we inject).
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        // A send only fails if the executor (and its Receiver) is gone; a
        // wake-after-shutdown is a no-op by design.
        let _ = self.shared.injected_tx.send(Inject::Wake(self.id));
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
}
