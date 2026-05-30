//! Futures for M17 Phase 2 — real async.
//!
//! A [`SharedFuture`] is a shared, single-assignment completion cell with an
//! async `get()`. It backs `Value::Future`: calling a script `async fn` spawns
//! the body onto the current-thread `LocalSet` and hands back a `SharedFuture`
//! that `await` drives. The cell stores a `Result<Value, Control>` so a panic (or
//! a `?`-propagation) raised inside the spawned task crosses the task boundary and
//! re-surfaces at the awaiting site.
//!
//! Borrow rule (clippy `await_holding_refcell_ref = deny`): never hold the slot's
//! `RefCell` borrow across the `.await` — `get()` clones the completed result out
//! before awaiting on the next notification.

use crate::interp::Control;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::Notify;

/// A shared completion cell carrying either the produced value or the `Control`
/// error that aborted the producing task. Cheap to `clone` (just an `Rc` bump);
/// equality is by identity (`ptr_eq`), like the other mutable `Value` containers.
#[derive(Clone)]
pub struct SharedFuture(Rc<Inner>);

struct Inner {
    slot: RefCell<Option<Result<Value, Control>>>,
    ready: Notify,
}

impl SharedFuture {
    /// A new, unresolved future.
    pub fn new() -> Self {
        SharedFuture(Rc::new(Inner { slot: RefCell::new(None), ready: Notify::new() }))
    }

    /// An already-resolved future carrying `r`. Used by `task.spawn` when the
    /// supplied function returns a plain value rather than a future.
    pub fn resolved(r: Result<Value, Control>) -> Self {
        let f = SharedFuture::new();
        f.resolve(r);
        f
    }

    /// Complete the future. The first resolution wins (later ones are ignored, so
    /// `race` can let several producers call `resolve` and keep only the winner).
    /// Always notifies waiters so a `get()` parked on `notified()` wakes up.
    pub fn resolve(&self, r: Result<Value, Control>) {
        if self.0.slot.borrow().is_none() {
            *self.0.slot.borrow_mut() = Some(r);
        }
        self.0.ready.notify_waiters();
    }

    /// Await the result. Returns a clone of the stored result; repeated `get()`s
    /// return the same cached result. Never holds the slot borrow across `.await`.
    pub async fn get(&self) -> Result<Value, Control> {
        loop {
            // Register interest BEFORE checking the slot so a `resolve()` that
            // races between the check and the await can't be missed.
            let notified = self.0.ready.notified();
            if let Some(r) = self.0.slot.borrow().clone() {
                return r;
            }
            notified.await;
        }
    }

    /// Identity equality (two handles to the same cell).
    pub fn ptr_eq(&self, other: &SharedFuture) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
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

    #[tokio::test]
    async fn resolves_once_and_get_returns_value() {
        let f = SharedFuture::new();
        f.resolve(Ok(Value::Number(7.0)));
        assert_eq!(f.get().await.unwrap(), Value::Number(7.0));
    }

    #[tokio::test]
    async fn second_get_returns_cached_value() {
        let f = SharedFuture::new();
        f.resolve(Ok(Value::Number(1.0)));
        assert_eq!(f.get().await.unwrap(), Value::Number(1.0));
        assert_eq!(f.get().await.unwrap(), Value::Number(1.0));
    }

    #[tokio::test]
    async fn resolve_twice_keeps_first() {
        let f = SharedFuture::new();
        f.resolve(Ok(Value::Number(1.0)));
        f.resolve(Ok(Value::Number(2.0)));
        assert_eq!(f.get().await.unwrap(), Value::Number(1.0));
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
        let f2 = f.clone();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                tokio::task::spawn_local(async move {
                    tokio::task::yield_now().await;
                    f2.resolve(Ok(Value::Number(99.0)));
                });
                assert_eq!(f.get().await.unwrap(), Value::Number(99.0));
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
    async fn resolved_constructor_is_ready() {
        let f = SharedFuture::resolved(Ok(Value::Bool(true)));
        assert_eq!(f.get().await.unwrap(), Value::Bool(true));
    }
}
