//! Consumer-driven script generators (M17 Phase 4).
//!
//! A generator (`fn*` / `async fn*`) is NOT a spawned task. It is a boxed body
//! future stored on a [`GeneratorHandle`] and driven *synchronously by the
//! consumer*: `gen.next(v)` / `for await` call [`GeneratorHandle::resume`], which
//! polls the body future one step at a time. The body's `yield` expression
//! suspends the future (returns `Poll::Pending` with a value parked in `out`);
//! `resume` observes that and returns the value to the consumer.
//!
//! Why polled, not spawned: a spawned generator body parks forever inside `yield`
//! when the consumer abandons it (never iterated, `for await` + `break`, a partial
//! `next()`, or a throwaway `g()`). The entry-point drain (`local.await`) then
//! hangs at program exit waiting on that zombie task. A consumer-driven generator
//! has no task at all — an un-driven generator is just an unpolled future that
//! drops cleanly when its `Rc` goes away. Nothing for `local.await` to wait on.
//!
//! **Current-generator stack.** `yield` must find *its* handle to hand a value to.
//! Polling is synchronous and nested (A.resume polls A.body, which may call
//! B.resume, which polls B.body), so a thread-local STACK of handles gives correct
//! lexical scoping: `yield` reads the top, which is always the body being polled
//! right now. An RAII guard ([`CurrentGenGuard`]) pushes on poll-enter and pops on
//! poll-exit (including unwind), so the stack always reflects the live nesting.
//!
//! Borrow rule (clippy `await_holding_refcell_ref = deny`): `poll_fn`'s closure is
//! synchronous per poll, so borrowing the handle's cells inside it is fine; we
//! never hold a `RefCell` borrow across the outer `.await`.

use crate::interp::Control;
use crate::value::Value;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;

thread_local! {
    /// Stack of the generators currently being polled, innermost on top. `yield`
    /// reads the top; empty => `yield` used outside any generator (Tier-2 error).
    static CURRENT_GEN: RefCell<Vec<Rc<GeneratorHandle>>> = const { RefCell::new(Vec::new()) };
}

/// The body future a generator drives: produces the body's return value (or a
/// `Control` error if it panicked / propagated). `!Send` like the whole runtime.
type BodyFuture = Pin<Box<dyn Future<Output = Result<Value, Control>>>>;

/// The runtime handle behind `Value::Generator`. Owns the body future and the
/// value-passing slots. Identity equality (`Rc::ptr_eq`), like other handles.
pub struct GeneratorHandle {
    /// The body future, taken out during a poll and put back if it is still
    /// pending. `None` once the generator has finished (or been closed).
    body: RefCell<Option<BodyFuture>>,
    /// Producer -> consumer: the value a `yield` handed out this poll.
    out: RefCell<Option<Value>>,
    /// Consumer -> producer: the value `next(v)` passed in, returned by `yield`.
    inp: RefCell<Option<Value>>,
    /// First `resume` only starts the body; later ones feed `inp` in.
    started: Cell<bool>,
}

impl GeneratorHandle {
    /// Build a handle around an already-constructed body future. The body does not
    /// run until the first [`resume`](Self::resume).
    pub fn new(body: BodyFuture) -> Self {
        GeneratorHandle {
            body: RefCell::new(Some(body)),
            out: RefCell::new(None),
            inp: RefCell::new(None),
            started: Cell::new(false),
        }
    }

    /// Producer side, called from inside the body via the `yield` expression. Parks
    /// the value in `out` and suspends the body future until the consumer's next
    /// `resume` deposits a value in `inp`, which becomes `yield`'s result.
    pub async fn yield_(&self, v: Value) -> Value {
        *self.out.borrow_mut() = Some(v);
        std::future::poll_fn(|_cx| match self.inp.borrow_mut().take() {
            Some(r) => Poll::Ready(r),
            None => Poll::Pending,
        })
        .await
    }

    /// Consumer side. Resumes the body with `input` (the very first call only
    /// starts the body and ignores `input`) and drives it to its next `yield`.
    /// Returns `Ok(Some(v))` for a yielded value, `Ok(None)` when the body is done,
    /// or `Err(c)` if the body panicked / propagated (surfaced to the consumer).
    ///
    /// `self` is an `Rc` so the handle can push itself onto `CURRENT_GEN` for the
    /// duration of each poll (so a nested `yield` finds it).
    pub async fn resume(self: &Rc<Self>, input: Value) -> Result<Option<Value>, Control> {
        if self.started.get() {
            *self.inp.borrow_mut() = Some(input);
        } else {
            self.started.set(true);
        }
        let this = self.clone();
        std::future::poll_fn(move |cx| {
            // Take the body OUT so the `body` borrow is not held across `poll`
            // (poll re-enters script evaluation, which may borrow these cells).
            let mut fut = match this.body.borrow_mut().take() {
                Some(f) => f,
                None => return Poll::Ready(Ok(None)), // already finished/closed
            };
            // Mark `this` as the current generator for the nested `yield` lookup,
            // popping on scope exit (incl. unwind) via the RAII guard.
            let polled = {
                let _guard = CurrentGenGuard::enter(this.clone());
                fut.as_mut().poll(cx)
            };
            match polled {
                // Body returned: iteration ends (the return value is discarded in
                // v1). Leave `body` as None so further resumes report done.
                Poll::Ready(Ok(_ret)) => Poll::Ready(Ok(None)),
                // Body errored: surface the Control to the consumer; body stays None.
                Poll::Ready(Err(c)) => Poll::Ready(Err(c)),
                Poll::Pending => {
                    // Keep the future for the next resume.
                    *this.body.borrow_mut() = Some(fut);
                    match this.out.borrow_mut().take() {
                        // A `yield` produced a value.
                        Some(v) => Poll::Ready(Ok(Some(v))),
                        // A real I/O `.await` inside the body is pending: forward it
                        // so the consumer's await suspends and the waker (registered
                        // via the real `cx`) reschedules us.
                        None => Poll::Pending,
                    }
                }
            }
        })
        .await
    }

    /// Close the generator: drop the body future so no more values are produced.
    /// A subsequent `resume` returns `Ok(None)`. Used by `gen.close()`.
    pub fn close(&self) {
        *self.body.borrow_mut() = None;
    }
}

/// RAII guard that pushes a generator onto `CURRENT_GEN` and pops it on drop
/// (including on panic/unwind), keeping the stack consistent across nested polls.
struct CurrentGenGuard;

impl CurrentGenGuard {
    fn enter(g: Rc<GeneratorHandle>) -> Self {
        CURRENT_GEN.with(|s| s.borrow_mut().push(g));
        CurrentGenGuard
    }
}

impl Drop for CurrentGenGuard {
    fn drop(&mut self) {
        CURRENT_GEN.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

/// The generator handle currently being polled (top of the stack), or `None` if
/// `yield` was used outside any generator body. Returns an `Rc` clone.
pub fn current_generator() -> Option<Rc<GeneratorHandle>> {
    CURRENT_GEN.with(|s| s.borrow().last().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AsError;
    use tokio::task::LocalSet;

    /// Drive `f` to completion on a current-thread LocalSet (the runtime shape the
    /// interpreter uses), then return its value.
    async fn on_localset<F, T>(f: F) -> T
    where
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        let local = LocalSet::new();
        local.run_until(f).await
    }

    /// A generator whose body yields each of `vals` in turn (via the current-gen
    /// lookup, exactly as the `yield` expression does at runtime).
    fn make_gen(vals: Vec<f64>) -> Rc<GeneratorHandle> {
        let body: BodyFuture = Box::pin(async move {
            for v in vals {
                let g = current_generator().expect("inside a generator");
                g.yield_(Value::Number(v)).await;
            }
            Ok(Value::Nil)
        });
        Rc::new(GeneratorHandle::new(body))
    }

    #[tokio::test]
    async fn three_value_generator_yields_then_done() {
        on_localset(async {
            let g = make_gen(vec![1.0, 2.0, 3.0]);
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Number(1.0))
            );
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Number(2.0))
            );
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Number(3.0))
            );
            assert_eq!(g.resume(Value::Nil).await.unwrap(), None);
            // Idempotent: still None after done.
            assert_eq!(g.resume(Value::Nil).await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn empty_generator_first_resume_is_none() {
        on_localset(async {
            let g = make_gen(vec![]);
            assert_eq!(g.resume(Value::Nil).await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn bidirectional_resume_passes_value_back_into_yield() {
        on_localset(async {
            // Body records the resume value each `yield` returns.
            let seen: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(Vec::new()));
            let seen2 = seen.clone();
            let body: BodyFuture = Box::pin(async move {
                let g = current_generator().unwrap();
                let a = g.yield_(Value::Number(10.0)).await;
                seen2.borrow_mut().push(a);
                let g = current_generator().unwrap();
                let b = g.yield_(Value::Number(20.0)).await;
                seen2.borrow_mut().push(b);
                Ok(Value::Nil)
            });
            let g = Rc::new(GeneratorHandle::new(body));
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Number(10.0))
            );
            assert_eq!(
                g.resume(Value::Str("first".into())).await.unwrap(),
                Some(Value::Number(20.0))
            );
            assert_eq!(g.resume(Value::Str("second".into())).await.unwrap(), None);
            let s = seen.borrow();
            assert_eq!(
                s.as_slice(),
                &[Value::Str("first".into()), Value::Str("second".into())]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn lazy_start_body_does_not_run_before_first_resume() {
        on_localset(async {
            let ran = Rc::new(Cell::new(false));
            let ran2 = ran.clone();
            let body: BodyFuture = Box::pin(async move {
                ran2.set(true);
                Ok(Value::Nil)
            });
            let g = Rc::new(GeneratorHandle::new(body));
            // The body is an unpolled future: nothing has run yet.
            assert!(!ran.get(), "body must not run before the first resume");
            assert_eq!(g.resume(Value::Nil).await.unwrap(), None);
            assert!(ran.get(), "body should have run after the first resume");
        })
        .await;
    }

    #[tokio::test]
    async fn abandoning_after_one_value_drops_cleanly() {
        // Consume one value, then drop the handle. Because the body is just an
        // unpolled future (no task), dropping it reclaims everything — and the
        // surrounding LocalSet completes (this test returning IS that proof).
        on_localset(async {
            let g = make_gen(vec![1.0, 2.0, 3.0]);
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Number(1.0))
            );
            drop(g);
        })
        .await;
    }

    #[tokio::test]
    async fn body_error_surfaces_at_consumer() {
        on_localset(async {
            let body: BodyFuture = Box::pin(async move {
                let g = current_generator().unwrap();
                g.yield_(Value::Number(1.0)).await;
                Err(Control::Panic(AsError::new("boom")))
            });
            let g = Rc::new(GeneratorHandle::new(body));
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Number(1.0))
            );
            match g.resume(Value::Nil).await {
                Err(Control::Panic(e)) => assert_eq!(e.message, "boom"),
                other => panic!("expected a panic surfaced to the consumer, got {other:?}"),
            }
            // After the error the generator is done.
            assert_eq!(g.resume(Value::Nil).await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn close_then_resume_is_none() {
        on_localset(async {
            let g = make_gen(vec![1.0, 2.0, 3.0]);
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Number(1.0))
            );
            g.close();
            assert_eq!(g.resume(Value::Nil).await.unwrap(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn nested_generators_use_stack_scoping() {
        // An outer generator drives an inner one and re-yields doubled values,
        // exactly like `for await (x in inner()) { yield x * 2 }`. The current-gen
        // stack must route each `yield` to the right handle.
        on_localset(async {
            let inner = make_gen(vec![1.0, 2.0]);
            let inner_for_body = inner.clone();
            let outer_body: BodyFuture = Box::pin(async move {
                while let Some(Value::Number(n)) = inner_for_body.resume(Value::Nil).await? {
                    let g = current_generator().unwrap();
                    g.yield_(Value::Number(n * 2.0)).await;
                }
                Ok(Value::Nil)
            });
            let outer = Rc::new(GeneratorHandle::new(outer_body));
            assert_eq!(
                outer.resume(Value::Nil).await.unwrap(),
                Some(Value::Number(2.0))
            );
            assert_eq!(
                outer.resume(Value::Nil).await.unwrap(),
                Some(Value::Number(4.0))
            );
            assert_eq!(outer.resume(Value::Nil).await.unwrap(), None);
            // The stack is balanced after all polling.
            assert!(current_generator().is_none());
        })
        .await;
    }
}
