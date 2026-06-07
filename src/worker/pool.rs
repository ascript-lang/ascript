//! The lazy, demand-grown isolate pool + scheduling.
//!
//! The pool lives on the CALLER thread behind a `thread_local!` `OnceCell` — it is
//! created on the FIRST `worker fn` dispatch and never before (a program with zero
//! worker calls spawns no thread; see `pool_is_initialized`). It owns the live
//! isolates and routes each job to one:
//!
//!   - an IDLE isolate (no in-flight jobs) if one exists; else
//!   - a NEW isolate if `live < cap` (demand growth); else
//!   - the LEAST-LOADED isolate (its `Send` mpsc queue holds the job FIFO until the
//!     isolate frees up — this is the backpressure/oversubscription path: more jobs
//!     than `cap` all complete as the per-isolate queues drain).
//!
//! `cap` = `$ASCRIPT_WORKERS` (if a positive integer) else `num_cpus::get()` (min 1).
//!
//! Each dispatched job increments the chosen isolate's in-flight counter; the
//! caller-side bridge task decrements it when the reply arrives (or the future is
//! dropped). The counter is an `Rc<Cell<usize>>` shared with the bridge, so it stays
//! on the caller thread (never crosses the channel).

use super::isolate::{Isolate, WorkerRequest};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    /// The process-/thread-local pool. `None` until the first dispatch initializes it.
    static POOL: RefCell<Option<Pool>> = const { RefCell::new(None) };
}

/// A live isolate plus its caller-side in-flight job counter (for least-loaded
/// scheduling and idle detection).
struct Slot {
    isolate: Isolate,
    inflight: Rc<Cell<usize>>,
}

/// The isolate pool. Caller-thread-owned (`!Send`); isolates run on their own threads.
pub struct Pool {
    /// Max live isolates (demand growth stops here; further jobs queue on isolates).
    cap: usize,
    slots: Vec<Slot>,
}

impl Pool {
    fn new() -> Pool {
        let cap = std::env::var("ASCRIPT_WORKERS")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|&n| n >= 1)
            .unwrap_or_else(num_cpus::get)
            .max(1);
        Pool {
            cap,
            slots: Vec::new(),
        }
    }

    /// Pick the slot to run `req` on, applying the idle → grow → least-loaded policy,
    /// and return its in-flight counter (already incremented for this job). The
    /// request is SENT here; the caller only wires the reply bridge.
    fn dispatch(&mut self, req: WorkerRequest) -> Rc<Cell<usize>> {
        // 1. An idle isolate?
        if let Some(slot) = self.slots.iter().find(|s| s.inflight.get() == 0) {
            return Self::send_to(slot, req);
        }
        // 2. Room to grow?
        if self.slots.len() < self.cap {
            let slot = Slot {
                isolate: Isolate::spawn(),
                inflight: Rc::new(Cell::new(0)),
            };
            self.slots.push(slot);
            let slot = self.slots.last().unwrap();
            return Self::send_to(slot, req);
        }
        // 3. Least-loaded (its mpsc queue provides FIFO backpressure).
        let slot = self
            .slots
            .iter()
            .min_by_key(|s| s.inflight.get())
            .expect("pool has at least one isolate once cap >= 1");
        Self::send_to(slot, req)
    }

    fn send_to(slot: &Slot, req: WorkerRequest) -> Rc<Cell<usize>> {
        slot.inflight.set(slot.inflight.get() + 1);
        // The isolate thread is alive for the pool's lifetime; a send failure would
        // mean the isolate panicked — extremely unlikely. The bridge's reply oneshot
        // will simply never resolve in that case (the dropped sender surfaces as a
        // recoverable panic at the await), so we don't unwrap here.
        let _ = slot.isolate.tx.send(req);
        slot.inflight.clone()
    }
}

/// Dispatch `req` onto the (lazily-initialized) pool, returning the chosen isolate's
/// shared in-flight counter so the caller's bridge task can decrement it on reply.
pub fn dispatch(req: WorkerRequest) -> Rc<Cell<usize>> {
    POOL.with(|cell| {
        let mut guard = cell.borrow_mut();
        let pool = guard.get_or_insert_with(Pool::new);
        pool.dispatch(req)
    })
}

/// Whether the pool has been initialized (the lazy-pool proof: a program with no
/// `worker fn` call never trips this). Test hook.
pub fn pool_is_initialized() -> bool {
    POOL.with(|cell| cell.borrow().is_some())
}

/// Whether the current thread is inside a worker isolate (inline-nesting decision).
pub fn in_isolate() -> bool {
    super::isolate::in_isolate()
}

#[cfg(test)]
mod tests {
    /// The lazy-pool proof: on a fresh thread (this test thread, which never
    /// dispatches a worker), the pool is never initialized. A program with zero
    /// `worker fn` calls therefore spawns no isolate thread.
    #[test]
    fn pool_not_initialized_until_first_dispatch() {
        assert!(!crate::worker::pool_is_initialized());
    }
}
