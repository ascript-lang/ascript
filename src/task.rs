//! M17 async runtime: `SharedFuture`, the handle behind `Value::future`, with
//! **structured concurrency / cancel-on-drop**.
//!
//! A script `async fn` call schedules its body on the current-thread `LocalSet`
//! and hands back a `Value::future`. Structured-concurrency rule: the spawned
//! task's lifetime is bound to its handle(s). When the **last** `Value::future`
//! clone referring to a task is dropped, the task is **aborted** — so an
//! un-awaited result is cancelled rather than orphaned (the dual of the
//! consumer-driven generator design). `task.spawn(...)` is the one explicit way
//! to opt out (detach / fire-and-forget).
//!
//! To make cancel-on-drop possible WITHOUT a reference cycle, the result slot and
//! the lifetime owner are split into two `Rc`s:
//! - [`ResultCell`] — the completion slot. The spawned task holds a clone of THIS
//!   (only) to deposit its result. It deliberately does NOT hold the handle.
//! - [`SharedFuture`] (`Rc<HandleInner>`) — the handle behind `Value::future`. It
//!   owns the task's [`AbortHandle`] and aborts the task in its `Drop`. Because
//!   the task never holds the handle, the handle's refcount can fall to zero while
//!   the task is still running — which is exactly what triggers cancellation.
//!
//! The stored value is a `Result<Value, Control>` so a panic/propagation raised in
//! the spawned body crosses the task boundary and re-surfaces at the `await` site.

use crate::interp::Control;
use crate::value::Value;
use crate::exec::AbortHandle;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::Notify;

/// The completion slot shared between the spawned task (writer) and the handle
/// (reader). Holds NO task lifetime — dropping it never cancels anything.
struct CellInner {
    /// `None` until resolved; then `Some(result)`. First writer wins (e.g. `race`).
    slot: RefCell<Option<Result<Value, Control>>>,
    /// Notifies all `get()` waiters when the slot is filled.
    ready: Notify,
}

/// Cloneable handle to a completion slot. Given to a spawned task so it can
/// `resolve` the result without holding the cancel-owning [`SharedFuture`].
#[derive(Clone)]
pub struct ResultCell(Rc<CellInner>);

impl ResultCell {
    fn new() -> Self {
        ResultCell(Rc::new(CellInner {
            slot: RefCell::new(None),
            ready: Notify::new(),
        }))
    }

    /// Resolve the cell (first writer wins) and wake all waiters.
    pub fn resolve(&self, result: Result<Value, Control>) {
        let mut slot = self.0.slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(result);
            self.0.ready.notify_waiters();
        }
    }

    /// Non-blocking, non-consuming probe of the completion slot (LANE §4).
    ///
    /// Returns `Some(result)` if the slot is already filled, `None` if it is
    /// still pending. Never blocks, never notifies, never touches any abort
    /// handle. The result is cloned out of the slot so the cell stays filled
    /// for all future callers (`get` / `try_get` / `resolve` are unaffected).
    pub(crate) fn try_get(&self) -> Option<Result<Value, Control>> {
        self.0.slot.borrow().as_ref().cloned()
    }

    /// Await the cell's value, parking until it is resolved.
    async fn get(&self) -> Result<Value, Control> {
        loop {
            // Fast path: already resolved. Clone out so we don't hold the borrow
            // across the await below.
            {
                let slot = self.0.slot.borrow();
                if let Some(r) = slot.as_ref() {
                    return r.clone();
                }
            }
            // Park until the next resolve, then re-check.
            let fut = self.0.ready.notified();
            // Re-check after creating the wait future to avoid lost wakeups.
            if self.0.slot.borrow().is_some() {
                continue;
            }
            fut.await;
        }
    }
}

/// The lifetime owner behind `Value::future`. Owns the task's abort handle and
/// cancels the task when the last handle clone is dropped (unless detached).
struct HandleInner {
    cell: ResultCell,
    /// The backing task's abort handle, if any. `None` for a taskless future
    /// (`new`/`resolved`, e.g. `race`'s winner) or after `detach()`.
    abort: RefCell<Option<AbortHandle>>,
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        // Cancel-on-drop: when the last handle goes away, abort the backing task.
        // A taskless or detached future has no abort handle, so this is a no-op.
        if let Some(h) = self.abort.borrow_mut().take() {
            h.abort();
        }
    }
}

/// Shared async handle behind `Value::future`. `Rc`-shared, `!Send`. Cloning
/// shares the same task/slot; dropping the last clone cancels the task.
#[derive(Clone)]
pub struct SharedFuture(Rc<HandleInner>);

impl SharedFuture {
    /// A fresh, unresolved, **taskless** future (no cancel-on-drop). Used as a
    /// plain rendezvous cell, e.g. `race`'s `winner`.
    pub fn new() -> Self {
        SharedFuture(Rc::new(HandleInner {
            cell: ResultCell::new(),
            abort: RefCell::new(None),
        }))
    }

    /// An already-resolved future (used when a value is ready synchronously).
    pub fn resolved(result: Result<Value, Control>) -> Self {
        let f = SharedFuture::new();
        f.resolve(result);
        f
    }

    /// Resolve the cell (first writer wins) and wake all waiters.
    pub fn resolve(&self, result: Result<Value, Control>) {
        self.0.cell.resolve(result);
    }

    /// Non-blocking, non-consuming probe of the backing cell (LANE §4).
    ///
    /// Returns `Some(result)` if the future is already resolved, `None` if it
    /// is still pending. Never blocks the async executor, never notifies
    /// waiters, and never touches the abort handle — cancel-on-drop semantics
    /// are fully preserved. The result is cloned out so the slot stays live for
    /// any subsequent `get`/`try_get` call.
    pub fn try_get(&self) -> Option<Result<Value, Control>> {
        self.0.cell.try_get()
    }

    /// Await the cell's value, parking until it is resolved. Cloneable waiters all
    /// observe the same result.
    pub async fn get(&self) -> Result<Value, Control> {
        self.0.cell.get().await
    }

    /// Identity equality (`Value::future` is identity-equal, like other handles).
    pub fn ptr_eq(&self, other: &SharedFuture) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// The result cell to hand to the backing task. The task resolves THIS rather
    /// than a `SharedFuture` clone, so it never keeps the handle alive — letting
    /// the handle's `Drop` cancel the task once the last handle goes away.
    pub fn cell(&self) -> ResultCell {
        self.0.cell.clone()
    }

    /// Attach the backing task's abort handle so dropping the last handle cancels
    /// the task (structured concurrency / cancel-on-drop).
    pub fn set_abort(&self, abort: AbortHandle) {
        *self.0.abort.borrow_mut() = Some(abort);
    }

    /// Detach the backing task: it runs to completion regardless of handle drops
    /// (explicit fire-and-forget, used by `task.spawn`). Removes the abort handle
    /// from the shared state, so no clone will cancel it.
    pub fn detach(&self) {
        let _ = self.0.abort.borrow_mut().take();
    }
}

impl Default for SharedFuture {
    fn default() -> Self {
        SharedFuture::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AsError;

    // ── LANE §4 tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn try_get_pending_is_none_resolved_is_some() {
        let f = SharedFuture::new();
        assert!(f.try_get().is_none(), "pending future must probe as None");
        f.resolve(Ok(Value::float(7.0)));
        assert_eq!(f.try_get().unwrap().unwrap(), Value::float(7.0));
        assert_eq!(f.get().await.unwrap(), Value::float(7.0));
        // Still Some after get() consumed nothing:
        assert_eq!(f.try_get().unwrap().unwrap(), Value::float(7.0));
    }

    #[tokio::test]
    async fn try_get_carries_stored_control() {
        let f = SharedFuture::new();
        f.resolve(Err(Control::Panic(AsError::new("boom"))));
        match f.try_get().unwrap() {
            Err(Control::Panic(e)) => assert_eq!(e.message, "boom"),
            other => panic!("expected stored panic, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn try_get_never_touches_the_abort_handle() {
        // A try_get probe on a pending future must NOT abort the backing task:
        // cancel-on-drop is only triggered by the LAST handle drop, never by
        // a non-consuming probe.
        use std::cell::Cell as StdCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let ran = Rc::new(StdCell::new(false));
                let ran2 = ran.clone();
                let f = SharedFuture::new();
                let cell = f.cell();
                let jh = crate::exec::spawn_local(async move {
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    ran2.set(true);
                    cell.resolve(Ok(Value::float(1.0)));
                });
                f.set_abort(jh.abort_handle());
                // Probe while still pending — must not abort:
                assert!(f.try_get().is_none());
                // Now drop the handle (last clone) — THIS triggers the abort:
                drop(f);
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }
                assert!(!ran.get(), "cancel-on-drop must survive a try_get probe");
            })
            .await;
    }

    // ── end LANE §4 tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn resolves_once_and_get_returns_value() {
        let f = SharedFuture::new();
        f.resolve(Ok(Value::float(1.0)));
        assert_eq!(f.get().await.unwrap(), Value::float(1.0));
        assert_eq!(f.get().await.unwrap(), Value::float(1.0));
    }

    #[tokio::test]
    async fn resolve_twice_keeps_first() {
        let f = SharedFuture::new();
        f.resolve(Ok(Value::float(1.0)));
        f.resolve(Ok(Value::float(2.0)));
        assert_eq!(f.get().await.unwrap(), Value::float(1.0));
    }

    #[tokio::test]
    async fn carries_error_across_boundary() {
        let f = SharedFuture::new();
        f.resolve(Err(Control::Panic(AsError::new("boom"))));
        match f.get().await {
            Err(Control::Panic(e)) => assert_eq!(e.message, "boom"),
            _ => panic!("expected panic"),
        }
    }

    #[tokio::test]
    async fn get_parks_until_resolved() {
        let f = SharedFuture::new();
        let cell = f.cell();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                crate::exec::spawn_local(async move {
                    tokio::task::yield_now().await;
                    cell.resolve(Ok(Value::float(99.0)));
                });
                assert_eq!(f.get().await.unwrap(), Value::float(99.0));
            })
            .await;
    }

    #[tokio::test]
    async fn ptr_eq_is_identity() {
        let a = SharedFuture::new();
        let b = a.clone();
        let c = SharedFuture::new();
        assert!(a.ptr_eq(&b));
        assert!(!a.ptr_eq(&c));
    }

    #[tokio::test]
    async fn dropping_last_handle_aborts_the_task() {
        // Dropping the last handle aborts the backing task: its body never finishes
        // (the post-suspension side effect does not run). Flag-based + timer-free
        // for determinism.
        use std::cell::Cell as StdCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let ran = Rc::new(StdCell::new(false));
                let ran2 = ran.clone();
                let f = SharedFuture::new();
                let cell = f.cell();
                let jh = crate::exec::spawn_local(async move {
                    // Two suspension points: the abort (issued before any poll)
                    // cancels the task before it can set the flag.
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    ran2.set(true);
                    cell.resolve(Ok(Value::float(1.0)));
                });
                f.set_abort(jh.abort_handle());
                drop(f); // last handle -> abort, before the task's first poll
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }
                assert!(!ran.get(), "aborted task body must not run to completion");
            })
            .await;
    }

    #[tokio::test]
    async fn detached_future_is_not_aborted_on_drop() {
        // After `detach()`, dropping all handles does NOT cancel the task: it runs
        // to completion (fire-and-forget).
        use std::cell::Cell as StdCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let ran = Rc::new(StdCell::new(false));
                let ran2 = ran.clone();
                let f = SharedFuture::new();
                let cell = f.cell();
                let jh = crate::exec::spawn_local(async move {
                    tokio::task::yield_now().await;
                    ran2.set(true);
                    cell.resolve(Ok(Value::float(7.0)));
                });
                f.set_abort(jh.abort_handle());
                f.detach(); // opt out of cancel-on-drop
                drop(f);
                for _ in 0..5 {
                    tokio::task::yield_now().await;
                }
                assert!(ran.get(), "detached task should run to completion");
                assert!(jh.is_finished());
            })
            .await;
    }
}
