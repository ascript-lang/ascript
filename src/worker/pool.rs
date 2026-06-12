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
use std::collections::HashSet;
use std::rc::Rc;

thread_local! {
    /// The process-/thread-local pool. `None` until the first dispatch initializes it.
    static POOL: RefCell<Option<Pool>> = const { RefCell::new(None) };
}

/// A live isolate plus its caller-side in-flight job counter (for least-loaded
/// scheduling and idle detection) and a MIRROR of the isolate's own code cache.
///
/// The mirror (`loaded_fns` / `archive_loaded`) lets the pool ship each `worker fn`'s
/// slice bytes AND the bundled module archive AT MOST ONCE per isolate: the isolate
/// already dedups (it installs the archive once via `archive_installed` and caches each
/// slice by `fn_id` in its `loaded` set, ignoring re-sent bytes), so this slot-state is a
/// pure mirror of that cache. FIFO on the isolate's `Send` mpsc guarantees the FIRST
/// (bytes-carrying) request to a slot is processed — populating the isolate's cache —
/// before any LATER (cleared) request on the same channel reaches it.
struct Slot {
    isolate: Isolate,
    inflight: Rc<Cell<usize>>,
    /// `fn_id`s whose slice bytes this isolate has been shipped (and thus cached).
    loaded_fns: HashSet<u64>,
    /// Whether this isolate has been shipped (and thus installed) the module archive.
    archive_loaded: bool,
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
    ///
    /// GRACEFUL DEGRADATION: if no isolate exists and a new one CANNOT be spawned
    /// (memory / thread-limit pressure), this returns `Err(req)` — handing the request
    /// back so the caller runs the worker INLINE on its own thread (correct result,
    /// just not parallel). Once at least one isolate is live, jobs always queue onto an
    /// existing isolate (its mpsc gives FIFO backpressure), so a transient spawn
    /// failure never strands work.
    ///
    /// The `Err`-variant carries the whole `WorkerRequest` BY DESIGN (the graceful-
    /// degradation handoff hands the request back so the caller runs it inline), so the
    /// `large_err` lint is allowed here — boxing it would just add an alloc on the rare
    /// degradation path for no benefit.
    #[allow(clippy::result_large_err)]
    fn dispatch(&mut self, req: WorkerRequest) -> Result<Rc<Cell<usize>>, WorkerRequest> {
        // INDEX-based slot selection (not `&Slot` references): `send_to` needs `&mut Slot`
        // to update the per-isolate cache mirror, so each branch resolves a slot INDEX and
        // then borrows it mutably once. The policy is unchanged: idle → grow → least-loaded
        // → inline-degradation (`Err`). The inline-degradation `Err` returns BEFORE any
        // `send_to`, so the handed-back request is UNTOUCHED (full bytes) — inline always
        // has the bytes it needs.

        // 1. An idle isolate?
        if let Some(idx) = self.slots.iter().position(|s| s.inflight.get() == 0) {
            return Ok(Self::send_to(&mut self.slots[idx], req));
        }
        // 2. Room to grow? Try to spawn; on failure, fall through (don't grow).
        if self.slots.len() < self.cap {
            if let Ok(isolate) = Isolate::spawn() {
                self.slots.push(Slot {
                    isolate,
                    inflight: Rc::new(Cell::new(0)),
                    // A fresh isolate has an empty code cache — nothing loaded yet.
                    loaded_fns: HashSet::new(),
                    archive_loaded: false,
                });
                let idx = self.slots.len() - 1;
                return Ok(Self::send_to(&mut self.slots[idx], req));
            }
            // Spawn failed: if there is at least one live isolate, queue on it
            // (step 3). If there are NONE, degrade to inline (return the request).
            if self.slots.is_empty() {
                return Err(req);
            }
        }
        // 3. Least-loaded existing isolate (its mpsc queue provides FIFO backpressure).
        match self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.inflight.get())
            .map(|(i, _)| i)
        {
            Some(idx) => Ok(Self::send_to(&mut self.slots[idx], req)),
            // No isolates at all and at/over cap with none spawnable — run inline.
            None => Err(req),
        }
    }

    fn send_to(slot: &mut Slot, mut req: WorkerRequest) -> Rc<Cell<usize>> {
        slot.inflight.set(slot.inflight.get() + 1);
        // OPTIMIZATION: don't re-ship code the isolate already cached. It installs the
        // archive once (`archive_installed`) and caches each slice by `fn_id` (`loaded`);
        // it ignores re-sent bytes. FIFO on this channel guarantees the first
        // (bytes-carrying) request is processed before any later (cleared) one reaches the
        // isolate, so this slot-state mirrors its cache. Inline degradation can never be
        // stranded: `dispatch` returns `Err(req)` (full bytes) BEFORE any `send_to`.
        if slot.archive_loaded {
            req.archive_bytes = None;
        } else if req.archive_bytes.is_some() {
            slot.archive_loaded = true;
        }
        if slot.loaded_fns.contains(&req.fn_id) {
            req.slice_bytes = None;
        } else if req.slice_bytes.is_some() {
            slot.loaded_fns.insert(req.fn_id);
        }
        // The isolate thread is alive for the pool's lifetime; a send failure would
        // mean the isolate panicked — extremely unlikely. The bridge's reply oneshot
        // will simply never resolve in that case (the dropped sender surfaces as a
        // recoverable panic at the await), so we don't unwrap here.
        let _ = slot.isolate.tx.send(req);
        slot.inflight.clone()
    }
}

/// Dispatch `req` onto the (lazily-initialized) pool. On success returns the chosen
/// isolate's shared in-flight counter so the caller's bridge task can decrement it on
/// reply. On `Err(req)` no isolate was available and none could be spawned — the
/// caller must run the worker inline (graceful degradation under resource pressure).
/// The `Err`-variant carries the whole request by design (see [`Pool::dispatch`]).
#[allow(clippy::result_large_err)]
pub fn dispatch(req: WorkerRequest) -> Result<Rc<Cell<usize>>, WorkerRequest> {
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
    use super::*;
    use crate::worker::isolate::{Isolate, WorkerReply, WorkerRequest};
    use tokio::sync::{mpsc, oneshot};

    /// The lazy-pool proof: on a fresh thread (this test thread, which never
    /// dispatches a worker), the pool is never initialized. A program with zero
    /// `worker fn` calls therefore spawns no isolate thread.
    #[test]
    fn pool_not_initialized_until_first_dispatch() {
        assert!(!crate::worker::pool_is_initialized());
    }

    /// Build a `WorkerRequest` carrying `Some` slice + archive bytes for `fn_id`, with
    /// throw-away reply/abort channels (this test never runs the request — it only
    /// inspects what `send_to` shipped).
    fn req_with_bytes(fn_id: u64) -> WorkerRequest {
        let (reply, _reply_rx) = oneshot::channel::<WorkerReply>();
        let (_abort_tx, abort) = oneshot::channel::<()>();
        WorkerRequest {
            fn_id,
            slice_bytes: Some(vec![1, 2, 3]),
            archive_bytes: Some(vec![4, 5, 6]),
            class_name: None,
            entry_name: "w".to_string(),
            args: Vec::new(),
            shared: Vec::new(),
            caps: Box::new(crate::stdlib::caps::CapSet::all_granted()),
            reply,
            abort,
        }
    }

    /// WHITE-BOX: `send_to` ships the slice + archive bytes ONCE per isolate and clears
    /// them on subsequent requests, while the slot's cache mirror (`loaded_fns`,
    /// `archive_loaded`) tracks exactly what the isolate has been shipped. We construct a
    /// `Slot` over a channel WE own (no real isolate thread) so we can inspect each
    /// received request directly.
    #[test]
    fn send_to_ships_bytes_once_per_isolate() {
        // A channel we own, wrapped as a fake `Isolate` (the thread handle is `None` —
        // `send_to` only touches `isolate.tx`).
        let (tx, mut rx) = mpsc::unbounded_channel::<WorkerRequest>();
        let mut slot = Slot {
            isolate: Isolate { tx, thread: None },
            inflight: Rc::new(Cell::new(0)),
            loaded_fns: HashSet::new(),
            archive_loaded: false,
        };

        // Before anything: the mirror is empty.
        assert!(!slot.archive_loaded);
        assert!(slot.loaded_fns.is_empty());

        // First request for fn_id=7: carries the bytes; the mirror records them.
        Pool::send_to(&mut slot, req_with_bytes(7));
        assert!(slot.archive_loaded, "archive_loaded must flip true after the first ship");
        assert!(slot.loaded_fns.contains(&7), "loaded_fns must record fn_id 7");
        let first = rx.try_recv().expect("first request was sent");
        assert!(first.slice_bytes.is_some(), "first request carries the slice bytes");
        assert!(first.archive_bytes.is_some(), "first request carries the archive bytes");

        // Second request for the SAME fn_id: bytes suppressed (the isolate already cached).
        Pool::send_to(&mut slot, req_with_bytes(7));
        let second = rx.try_recv().expect("second request was sent");
        assert!(second.slice_bytes.is_none(), "second request drops the (cached) slice");
        assert!(second.archive_bytes.is_none(), "second request drops the (installed) archive");

        // A DIFFERENT fn_id: its slice is shipped (new to the isolate) but the archive,
        // already installed, stays suppressed.
        Pool::send_to(&mut slot, req_with_bytes(9));
        assert!(slot.loaded_fns.contains(&9), "loaded_fns now also records fn_id 9");
        let third = rx.try_recv().expect("third request was sent");
        assert!(third.slice_bytes.is_some(), "a new fn_id still ships its slice");
        assert!(third.archive_bytes.is_none(), "the archive stays suppressed once installed");
    }
}
