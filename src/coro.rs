//! Single-consumer bidirectional yield rendezvous for script generators (M17 Phase 4).
//!
//! A generator body runs as its own `spawn_local` task. Communication with the
//! consumer (the `for await` loop or `gen.next(v)` caller) happens through a
//! [`YieldChannel`]: a hand-rolled rendezvous over two `tokio::sync::Notify`s.
//! The producer (`yield`) hands a value to the consumer and parks until it is
//! resumed; the consumer (`resume`) feeds a value into the parked `yield` and
//! parks until the next yield (or until the body finishes).
//!
//! The channel that the *currently executing body* should read from `yield` is
//! stored in a `tokio::task_local!` (`GEN_CHANNEL`). It MUST be a task-local
//! (not a field on `Interp`): several generators can be active at once
//! (composition — `for await (x in inner()) { yield f(x) }`), and each runs as a
//! separate task, so a shared slot would corrupt under interleaving. The
//! per-task `GEN_CHANNEL` of a running body always names that body's own channel;
//! a nested `for await` resumes the inner generator by its explicit handle, so
//! the scopes stay independent.
//!
//! Borrow rule (clippy `await_holding_refcell_ref = deny`): never hold any of the
//! cell's `RefCell` borrows across an `.await`. Each wait creates the
//! `notified()` future first, then takes/checks the cell in a scoped borrow that
//! drops before `.await`.

use crate::interp::Control;
use crate::value::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tokio::sync::Notify;

tokio::task_local! {
    /// The channel of the generator whose body is currently executing on THIS task.
    /// `yield` reads it; absent => `yield` used outside a generator (a Tier-2 error).
    pub static GEN_CHANNEL: YieldChannel;
}

/// A handle to a running generator's rendezvous channel. Cheap to clone (an `Rc`
/// bump); both the producer task and the consumer hold a clone.
#[derive(Clone)]
pub struct YieldChannel(Rc<Chan>);

struct Chan {
    out: RefCell<Option<Value>>, // producer -> consumer (the yielded value)
    inp: RefCell<Option<Value>>, // consumer -> producer (the resume value)
    /// Set by the body wrapper just before `finish()`; carries a body error
    /// (`Control::Panic`/`Propagate`) so it re-surfaces at the consumer.
    err: RefCell<Option<Control>>,
    done: Cell<bool>,
    started: Cell<bool>,
    to_consumer: Notify,
    to_producer: Notify,
}

impl YieldChannel {
    pub fn new() -> Self {
        YieldChannel(Rc::new(Chan {
            out: RefCell::new(None),
            inp: RefCell::new(None),
            err: RefCell::new(None),
            done: Cell::new(false),
            started: Cell::new(false),
            to_consumer: Notify::new(),
            to_producer: Notify::new(),
        }))
    }

    /// Producer side (called from inside the generator body, via `yield`). Hands
    /// `v` to the consumer and parks until the consumer resumes; returns the
    /// resume value the consumer passed to `next(v)` (`nil` for `next()`/`for await`).
    pub async fn yield_(&self, v: Value) -> Value {
        *self.0.out.borrow_mut() = Some(v);
        self.0.to_consumer.notify_waiters();
        loop {
            // Create the wait future BEFORE checking, to avoid a lost wakeup.
            let notified = self.0.to_producer.notified();
            if let Some(r) = self.0.inp.borrow_mut().take() {
                return r;
            }
            notified.await;
        }
    }

    /// Called once by the body-task wrapper right before the body starts, so the
    /// body parks until the first `resume`. Keeps generators lazy: nothing in the
    /// body runs until the first `next`/`for await`.
    pub async fn await_first_resume(&self) {
        loop {
            let notified = self.0.to_producer.notified();
            if self.0.started.get() {
                return;
            }
            notified.await;
        }
    }

    /// Record a body error so the consumer can re-raise it, then mark done.
    pub fn fail(&self, err: Control) {
        *self.0.err.borrow_mut() = Some(err);
        self.finish();
    }

    /// Mark the generator finished and wake any parked consumer.
    pub fn finish(&self) {
        self.0.done.set(true);
        self.0.to_consumer.notify_waiters();
    }

    /// Take the stored body error, if any (consumed once).
    pub fn take_error(&self) -> Option<Control> {
        self.0.err.borrow_mut().take()
    }

    /// Consumer side. Sends `input` into the paused `yield` (the very first call
    /// just starts the body and ignores `input`), then awaits the next yielded
    /// value. Returns `Some(value)` for a yield, or `None` when the generator is
    /// done. After `None`, check [`take_error`](Self::take_error) for a body error.
    pub async fn resume(&self, input: Value) -> Option<Value> {
        if !self.0.started.get() {
            self.0.started.set(true); // first resume just starts the body
        } else {
            *self.0.inp.borrow_mut() = Some(input);
        }
        self.0.to_producer.notify_waiters();
        loop {
            // Register interest BEFORE checking so a yield/finish that races
            // between the check and the await is not missed.
            let notified = self.0.to_consumer.notified();
            if let Some(v) = self.0.out.borrow_mut().take() {
                return Some(v);
            }
            if self.0.done.get() {
                return None;
            }
            notified.await;
        }
    }
}

impl Default for YieldChannel {
    fn default() -> Self {
        YieldChannel::new()
    }
}

/// The runtime handle behind `Value::Generator`. Holds the rendezvous channel to
/// the spawned body task. Identity equality (`Rc::ptr_eq`), like other handles.
pub struct GeneratorHandle {
    pub chan: YieldChannel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AsError;
    use tokio::task::LocalSet;

    /// Drive `f` to completion on a current-thread LocalSet (the runtime shape
    /// the interpreter uses), then return its value.
    async fn on_localset<F, T>(f: F) -> T
    where
        F: std::future::Future<Output = T> + 'static,
        T: 'static,
    {
        let local = LocalSet::new();
        local.run_until(f).await
    }

    /// Spawn a body that yields each of `vals` in turn, scoped under `GEN_CHANNEL`.
    fn spawn_yielding(chan: YieldChannel, vals: Vec<f64>) {
        let body = chan.clone();
        tokio::task::spawn_local(GEN_CHANNEL.scope(body.clone(), async move {
            body.await_first_resume().await;
            let c = GEN_CHANNEL.with(|c| c.clone());
            for v in vals {
                c.yield_(Value::Number(v)).await;
            }
            body.finish();
        }));
    }

    #[tokio::test]
    async fn three_value_generator_yields_then_done() {
        on_localset(async {
            let chan = YieldChannel::new();
            spawn_yielding(chan.clone(), vec![1.0, 2.0, 3.0]);
            assert_eq!(chan.resume(Value::Nil).await, Some(Value::Number(1.0)));
            assert_eq!(chan.resume(Value::Nil).await, Some(Value::Number(2.0)));
            assert_eq!(chan.resume(Value::Nil).await, Some(Value::Number(3.0)));
            assert_eq!(chan.resume(Value::Nil).await, None);
            // Idempotent: still None after done.
            assert_eq!(chan.resume(Value::Nil).await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn empty_generator_first_resume_is_none() {
        on_localset(async {
            let chan = YieldChannel::new();
            spawn_yielding(chan.clone(), vec![]);
            assert_eq!(chan.resume(Value::Nil).await, None);
        })
        .await;
    }

    #[tokio::test]
    async fn bidirectional_resume_passes_value_back_into_yield() {
        on_localset(async {
            let chan = YieldChannel::new();
            let body = chan.clone();
            // Body: a = yield 10; record a; b = yield 20; record b.
            let seen: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(Vec::new()));
            let seen2 = seen.clone();
            tokio::task::spawn_local(GEN_CHANNEL.scope(body.clone(), async move {
                body.await_first_resume().await;
                let c = GEN_CHANNEL.with(|c| c.clone());
                let a = c.yield_(Value::Number(10.0)).await;
                seen2.borrow_mut().push(a);
                let b = c.yield_(Value::Number(20.0)).await;
                seen2.borrow_mut().push(b);
                body.finish();
            }));
            assert_eq!(chan.resume(Value::Nil).await, Some(Value::Number(10.0)));
            assert_eq!(chan.resume(Value::Str("first".into())).await, Some(Value::Number(20.0)));
            assert_eq!(chan.resume(Value::Str("second".into())).await, None);
            let s = seen.borrow();
            assert_eq!(s.as_slice(), &[Value::Str("first".into()), Value::Str("second".into())]);
        })
        .await;
    }

    #[tokio::test]
    async fn lazy_start_body_does_not_run_before_first_resume() {
        on_localset(async {
            let chan = YieldChannel::new();
            let body = chan.clone();
            let ran = Rc::new(Cell::new(false));
            let ran2 = ran.clone();
            tokio::task::spawn_local(GEN_CHANNEL.scope(body.clone(), async move {
                body.await_first_resume().await;
                ran2.set(true);
                body.finish();
            }));
            // Give the spawned task a chance to run up to await_first_resume.
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            assert!(!ran.get(), "body must not run before the first resume");
            assert_eq!(chan.resume(Value::Nil).await, None);
            assert!(ran.get(), "body should have run after first resume");
        })
        .await;
    }

    #[tokio::test]
    async fn early_drop_does_not_hang() {
        on_localset(async {
            let chan = YieldChannel::new();
            spawn_yielding(chan.clone(), vec![1.0, 2.0, 3.0]);
            // Consume one value, then stop resuming. The body task is left parked
            // inside `yield_`; dropping the LocalSet reclaims it. No hang.
            assert_eq!(chan.resume(Value::Nil).await, Some(Value::Number(1.0)));
        })
        .await;
    }

    #[tokio::test]
    async fn body_error_surfaces_via_take_error() {
        on_localset(async {
            let chan = YieldChannel::new();
            let body = chan.clone();
            tokio::task::spawn_local(GEN_CHANNEL.scope(body.clone(), async move {
                body.await_first_resume().await;
                let c = GEN_CHANNEL.with(|c| c.clone());
                c.yield_(Value::Number(1.0)).await;
                body.fail(Control::Panic(AsError::new("boom")));
            }));
            assert_eq!(chan.resume(Value::Nil).await, Some(Value::Number(1.0)));
            assert_eq!(chan.resume(Value::Nil).await, None);
            match chan.take_error() {
                Some(Control::Panic(e)) => assert_eq!(e.message, "boom"),
                other => panic!("expected stored panic, got {other:?}"),
            }
            // Error consumed once.
            assert!(chan.take_error().is_none());
        })
        .await;
    }
}
