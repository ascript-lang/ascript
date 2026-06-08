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
use std::rc::{Rc, Weak};
use std::task::Poll;

thread_local! {
    /// Stack of the generators currently being polled, innermost on top. `yield`
    /// reads the top; empty => `yield` used outside any generator (Tier-2 error).
    static CURRENT_GEN: RefCell<Vec<Rc<GeneratorHandle>>> = const { RefCell::new(Vec::new()) };
}

/// The body future a generator drives: produces the body's return value (or a
/// `Control` error if it panicked / propagated). `!Send` like the whole runtime.
type BodyFuture = Pin<Box<dyn Future<Output = Result<Value, Control>>>>;

/// The two interchangeable engines behind a [`GeneratorHandle`].
///
/// `Body` is the original tree-walker generator: a polled body future driven via
/// the `CURRENT_GEN` stack + `poll_fn` parking. `Vm` is the bytecode VM
/// generator: a *Suspended Fiber* whose frames stay live between yields (no
/// `poll_fn`, no `CURRENT_GEN` — `yield` is the `Op::Yield` opcode and `resume`
/// drives `Vm::run` to the next yield). The two share one `resume`/`close`
/// dispatch so the interp's `GeneratorMethod` plumbing is engine-agnostic.
enum GenImpl {
    /// Tree-walker path (UNCHANGED from the original `GeneratorHandle`).
    Body {
        /// The body future, taken out during a poll and put back if it is still
        /// pending. `None` once the generator has finished (or been closed).
        body: RefCell<Option<BodyFuture>>,
        /// Producer -> consumer: the value a `yield` handed out this poll.
        out: RefCell<Option<Value>>,
        /// Consumer -> producer: the value `next(v)` passed in, returned by `yield`.
        inp: RefCell<Option<Value>>,
        /// First `resume` only starts the body; later ones feed `inp` in.
        started: Cell<bool>,
    },
    /// Bytecode VM path: a Suspended Fiber. The `Fiber` lives in an `Option` so it
    /// can be *moved out* across the `Vm::run(&mut fiber).await` (the
    /// take-out-then-await-then-return pattern that keeps a `RefCell` borrow off
    /// the await — `clippy::await_holding_refcell_ref` stays clean). `None` while
    /// running (taken out) or once done/closed.
    Vm {
        fiber: RefCell<Option<crate::vm::Fiber>>,
        vm: Weak<crate::vm::Vm>,
        /// First `resume` starts the fiber from ip 0 and ignores its `input`;
        /// later resumes push `input` as the suspended `yield` expression's value.
        started: Cell<bool>,
        /// Once the fiber returned / errored / was closed: further resumes are
        /// the `None` done sentinel.
        done: Cell<bool>,
    },
    /// Cross-thread streaming generator (`worker fn*`, Spec B Task 6). The producer
    /// body runs in a DEDICATED isolate; THIS side is a demand-driven driver. Each
    /// `resume(input)` ENCODES `input` (the value the producer's `yield` expression
    /// returns — bidirectional), sends one demand credit over the
    /// [`crate::worker::stream::StreamDriver`]'s channel, awaits the next serialized
    /// yield, and DECODES it against the consumer's `Interp`. A bounded prefetch
    /// buffer (default 1 = strict pull) gives backpressure (the producer runs at most
    /// one step ahead of demand). Transparent behind `Value::Generator`, so
    /// `for await`/`.next(v)`/`.close()` work unchanged.
    Worker {
        /// The cross-thread driver, moved OUT across the `.await` (take-out-then-
        /// await-then-return, like `Vm`'s `fiber`) so no `RefCell` borrow rides the
        /// await. `None` while resuming (taken out) or once done/closed (dropped =
        /// isolate teardown).
        driver: RefCell<Option<Box<crate::worker::stream::StreamDriver>>>,
        /// The consumer's interp, to encode the injected `next(v)` and decode each
        /// yielded value (the boundary is `Send` bytes; the `Value`s are rebuilt on
        /// THIS thread against THIS interp).
        interp: Weak<crate::interp::Interp>,
        /// First `resume` ignores `input` (local-generator first-`next` semantics),
        /// matching `Body`/`Vm`.
        started: Cell<bool>,
        /// Once the producer is done / errored / closed: further resumes are the
        /// `None` done sentinel.
        done: Cell<bool>,
    },
}

/// The runtime handle behind `Value::Generator`. Wraps either engine (tree-walker
/// body future or VM Suspended Fiber). Identity equality (`Rc::ptr_eq`), like
/// other handles.
pub struct GeneratorHandle {
    inner: GenImpl,
}

impl GeneratorHandle {
    /// Build a (tree-walker) handle around an already-constructed body future. The
    /// body does not run until the first [`resume`](Self::resume).
    pub fn new(body: BodyFuture) -> Self {
        GeneratorHandle {
            inner: GenImpl::Body {
                body: RefCell::new(Some(body)),
                out: RefCell::new(None),
                inp: RefCell::new(None),
                started: Cell::new(false),
            },
        }
    }

    /// Build a VM-backed handle around a NOT-STARTED [`Fiber`](crate::vm::Fiber)
    /// (its sole frame is the generator closure with args bound, ip 0). `vm` is a
    /// weak ref to the VM that drives it (upgraded to an owned `Rc` before each
    /// `Vm::run` await). The fiber does not run until the first
    /// [`resume`](Self::resume).
    pub fn new_vm(fiber: crate::vm::Fiber, vm: Weak<crate::vm::Vm>) -> Self {
        GeneratorHandle {
            inner: GenImpl::Vm {
                fiber: RefCell::new(Some(fiber)),
                vm,
                started: Cell::new(false),
                done: Cell::new(false),
            },
        }
    }

    /// Build a streaming-worker handle (`worker fn*`, Spec B Task 6) around an
    /// already-`Init`'d cross-thread [`StreamDriver`](crate::worker::stream::StreamDriver).
    /// The producer body runs in the driver's dedicated isolate; this side is the
    /// demand-driven consumer. `interp` is a weak ref to the consumer's interp (upgraded
    /// to encode/decode the boundary `Value`s on each resume). No body runs until the
    /// first [`resume`](Self::resume).
    pub fn new_worker(
        driver: Box<crate::worker::stream::StreamDriver>,
        interp: Weak<crate::interp::Interp>,
    ) -> Self {
        GeneratorHandle {
            inner: GenImpl::Worker {
                driver: RefCell::new(Some(driver)),
                interp,
                started: Cell::new(false),
                done: Cell::new(false),
            },
        }
    }

    /// Producer side, called from inside a TREE-WALKER body via the `yield`
    /// expression. Parks the value in `out` and suspends the body future until the
    /// consumer's next `resume` deposits a value in `inp`, which becomes `yield`'s
    /// result.
    ///
    /// # Panics
    /// If called on a non-tree-walker handle (a VM `yield` is the `Op::Yield` opcode; a
    /// worker producer yields in ITS OWN isolate, not via this handle — a wiring bug if
    /// it ever reaches here).
    pub async fn yield_(&self, v: Value) -> Value {
        let (out, inp) = match &self.inner {
            GenImpl::Body { out, inp, .. } => (out, inp),
            GenImpl::Vm { .. } => {
                unreachable!(
                    "GeneratorHandle::yield_ called on a VM-backed generator (use Op::Yield)"
                )
            }
            GenImpl::Worker { .. } => {
                unreachable!(
                    "GeneratorHandle::yield_ called on a worker generator (the producer \
                     yields in its own isolate, not via the caller-side handle)"
                )
            }
        };
        *out.borrow_mut() = Some(v);
        std::future::poll_fn(|_cx| match inp.borrow_mut().take() {
            Some(r) => Poll::Ready(r),
            None => Poll::Pending,
        })
        .await
    }

    /// Consumer side. Resumes the generator with `input` (the very first call only
    /// starts it and ignores `input`) and drives it to its next `yield`. Returns
    /// `Ok(Some(v))` for a yielded value, `Ok(None)` when done, or `Err(c)` on a
    /// panic / propagation (surfaced to the consumer). Dispatches on the engine.
    pub async fn resume(self: &Rc<Self>, input: Value) -> Result<Option<Value>, Control> {
        match &self.inner {
            GenImpl::Body { .. } => self.resume_body(input).await,
            GenImpl::Vm { .. } => self.resume_vm(input).await,
            GenImpl::Worker { .. } => self.resume_worker(input).await,
        }
    }

    /// The tree-walker resume path — BYTE-IDENTICAL to the pre-refactor `resume`.
    async fn resume_body(self: &Rc<Self>, input: Value) -> Result<Option<Value>, Control> {
        let (body, out, inp, started) = match &self.inner {
            GenImpl::Body {
                body,
                out,
                inp,
                started,
            } => (body, out, inp, started),
            _ => unreachable!("resume_body on a non-tree-walker generator"),
        };
        if started.get() {
            *inp.borrow_mut() = Some(input);
        } else {
            started.set(true);
        }
        let this = self.clone();
        std::future::poll_fn(move |cx| {
            // Take the body OUT so the `body` borrow is not held across `poll`
            // (poll re-enters script evaluation, which may borrow these cells).
            let mut fut = match body.borrow_mut().take() {
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
                    *body.borrow_mut() = Some(fut);
                    match out.borrow_mut().take() {
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

    /// The VM resume path: drive the Suspended Fiber to its next `Op::Yield`.
    ///
    /// AWAIT DISCIPLINE: the `Fiber` is *moved out* of its `RefCell<Option<..>>`
    /// before `Vm::run(&mut fiber).await` and put back after, so no `RefCell`
    /// borrow is ever held across the await. The `vm` weak is upgraded to an owned
    /// `Rc<Vm>` first, also outside any borrow. Mirrors the
    /// take-out-then-await-then-return pattern the rest of the runtime uses for
    /// native resources.
    async fn resume_vm(&self, input: Value) -> Result<Option<Value>, Control> {
        let (fiber_cell, vm_weak, started, done) = match &self.inner {
            GenImpl::Vm {
                fiber,
                vm,
                started,
                done,
            } => (fiber, vm, started, done),
            _ => unreachable!("resume_vm on a non-VM generator"),
        };
        if done.get() {
            return Ok(None);
        }
        // Take the fiber out (no borrow held across the await below). `None` here
        // means a re-entrant resume of a generator already running — a misuse the
        // surface language cannot express, treated as done.
        let mut fiber = match fiber_cell.borrow_mut().take() {
            Some(f) => f,
            None => return Ok(None),
        };
        if started.get() {
            // The value `next(v)` passed in becomes the result of the `yield`
            // expression that suspended us: push it where the bytecode after
            // `Op::Yield` expects the yield expression's value on TOS.
            fiber.push(input);
        } else {
            // First resume: start the fiber from ip 0; `input` is ignored (matches
            // the tree-walker's first-next semantics).
            started.set(true);
        }
        // Upgrade the weak VM to an owned Rc before the await. The VM outlives any
        // live generator (the handle is reachable only while a program runs), so a
        // failed upgrade is a wiring bug.
        let vm = vm_weak
            .upgrade()
            .expect("VM dropped while a generator is still live (wiring bug)");
        // `Vm::run` may re-enter `resume` (a `for await` inside a generator body
        // drives a sub-generator via `Op::IterNext`), forming the async cycle
        // run → resume → resume_vm → run. SP9 §1: this is the native re-entry funnel
        // for nested generator composition (a generator whose body drives another) —
        // `grow_future` boxes the edge AND grows the native stack per poll so deep
        // composition reaches the logical cap cleanly instead of SIGABRTing.
        let outcome = crate::vm::stack::grow_future(vm.run(&mut fiber)).await;
        match outcome {
            Ok(crate::vm::RunOutcome::Yielded(v)) => {
                // Still suspended: put the fiber back for the next resume.
                *fiber_cell.borrow_mut() = Some(fiber);
                Ok(Some(v))
            }
            Ok(crate::vm::RunOutcome::Done(_ret)) => {
                // The generator body returned: iteration ends. The return value is
                // DISCARDED (matches the tree-walker — `next()` returns nil at
                // completion, not the body's return value). Fiber stays taken out.
                done.set(true);
                Ok(None)
            }
            Err(c) => {
                // A panic / propagation: surface to the consumer; the generator is
                // done (fiber stays taken out).
                done.set(true);
                Err(c)
            }
        }
    }

    /// The streaming-worker resume path (`worker fn*`, Spec B Task 6): send one demand
    /// credit across the isolate boundary and await the next serialized yield.
    ///
    /// ENCODE/DECODE happen on THIS (consumer) thread against the consumer's interp:
    /// `input` (the `.next(v)` value) is encoded into `Send` bytes for the producer's
    /// suspended `yield`; the yielded value comes back as bytes and is decoded here.
    ///
    /// AWAIT DISCIPLINE: the `Box<StreamDriver>` is MOVED OUT of its
    /// `RefCell<Option<..>>` before `driver.resume(..).await` and put back after (the
    /// take-out-then-await-then-return pattern), so no `RefCell` borrow rides the await
    /// (`clippy::await_holding_refcell_ref` stays clean). The `interp` weak is upgraded
    /// (outside any borrow) for the encode/decode.
    async fn resume_worker(&self, input: Value) -> Result<Option<Value>, Control> {
        let (driver_cell, interp_weak, started, done) = match &self.inner {
            GenImpl::Worker {
                driver,
                interp,
                started,
                done,
            } => (driver, interp, started, done),
            _ => unreachable!("resume_worker on a non-worker generator"),
        };
        if done.get() {
            return Ok(None);
        }
        // The consumer's interp drives encode/decode. It outlives any live generator
        // (the handle is reachable only while a program runs), so a failed upgrade is a
        // wiring bug.
        let interp = interp_weak
            .upgrade()
            .expect("interp dropped while a streaming generator is still live (wiring bug)");

        // SP9 determinism (Spec B Task 12) — REPLAY: if a determinism context is active
        // in Replay mode AND it has a recorded `GeneratorYield` at the cursor, return the
        // recorded yield/done/panic WITHOUT re-driving the producer isolate (the producer
        // ran exactly once, at Record). The borrow is a SHORT sync borrow, never held
        // across the `.await` below. The `None`/default + Record paths fall through to the
        // real resume — byte-identical when inert.
        let replayed: Option<crate::det::BoundaryOutcome> = interp
            .with_determinism_mut(|ctx| {
                if ctx.mode == crate::det::Mode::Replay {
                    ctx.replay_generator_yield()
                } else {
                    None
                }
            })
            .flatten();
        if let Some(outcome) = replayed {
            // Mark started so subsequent injected values are honored (matches the real
            // path's first-resume bookkeeping).
            started.set(true);
            return match outcome {
                crate::det::BoundaryOutcome::Bytes(bytes) => {
                    let v = crate::worker::serialize::decode(&bytes, &interp)
                        .map_err(|e| Control::Panic(crate::error::AsError::new(e.message())))?;
                    Ok(Some(v))
                }
                crate::det::BoundaryOutcome::Done => {
                    done.set(true);
                    Ok(None)
                }
                crate::det::BoundaryOutcome::Panic(msg) => {
                    done.set(true);
                    Err(Control::Panic(crate::error::AsError::new(msg)))
                }
            };
        }

        // Encode the injected value on FIRST resume too, but the producer ignores it
        // (first-`next` semantics) — we pass nil's encoding then to keep the protocol
        // uniform. Encode against the consumer's serializer (sendability gate applies).
        let to_inject = if started.get() { input } else { Value::Nil };
        if !started.get() {
            started.set(true);
        }
        let encoded_input = crate::worker::serialize::encode(&to_inject)
            .map_err(|e| Control::Panic(crate::error::AsError::new(e.message())))?;

        // Take the driver OUT (no borrow held across the await). `None` here means a
        // re-entrant resume of a generator already running — surface as done.
        let driver = match driver_cell.borrow_mut().take() {
            Some(d) => d,
            None => return Ok(None),
        };

        let outcome = driver.resume(encoded_input).await;

        // SP9 determinism (Task 12) — RECORD: event-source the boundary outcome (the
        // yielded bytes / done / panic) so a later Replay reproduces it without re-driving
        // the producer. SHORT sync borrow AFTER the `.await`, never across it.
        interp.with_determinism_mut(|ctx| {
            if ctx.mode == crate::det::Mode::Record {
                let ev = match &outcome {
                    Ok(Some(bytes)) => crate::det::BoundaryOutcome::Bytes(bytes.clone()),
                    Ok(None) => crate::det::BoundaryOutcome::Done,
                    Err(msg) => crate::det::BoundaryOutcome::Panic(msg.clone()),
                };
                ctx.record_generator_yield(&ev);
            }
        });

        match outcome {
            Ok(Some(bytes)) => {
                // Still producing: put the driver back for the next demand credit.
                *driver_cell.borrow_mut() = Some(driver);
                let v = crate::worker::serialize::decode(&bytes, &interp)
                    .map_err(|e| Control::Panic(crate::error::AsError::new(e.message())))?;
                Ok(Some(v))
            }
            Ok(None) => {
                // The producer finished: iteration ends. Drop the driver (= isolate
                // teardown) by leaving it taken out, and mark done.
                done.set(true);
                Ok(None)
            }
            Err(msg) => {
                // A producer panic / sendability failure: surface to the consumer. The
                // generator is done (driver stays taken out = torn down).
                done.set(true);
                Err(Control::Panic(crate::error::AsError::new(msg)))
            }
        }
    }

    /// Close the generator: no more values are produced; a subsequent `resume`
    /// returns `Ok(None)`. Used by `gen.close()`. For the tree-walker that drops
    /// the body future; for the VM that drops the Fiber and marks it done; for a
    /// streaming worker that drops the driver (closing the channel + joining the
    /// isolate thread — clean teardown, no zombie).
    pub fn close(&self) {
        match &self.inner {
            GenImpl::Body { body, .. } => *body.borrow_mut() = None,
            GenImpl::Vm { fiber, done, .. } => {
                done.set(true);
                *fiber.borrow_mut() = None;
            }
            GenImpl::Worker { driver, done, .. } => {
                done.set(true);
                *driver.borrow_mut() = None; // drop = IsolateHandle Drop = teardown
            }
        }
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
                g.yield_(Value::Float(v)).await;
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
                Some(Value::Float(1.0))
            );
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Float(2.0))
            );
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Float(3.0))
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
                let a = g.yield_(Value::Float(10.0)).await;
                seen2.borrow_mut().push(a);
                let g = current_generator().unwrap();
                let b = g.yield_(Value::Float(20.0)).await;
                seen2.borrow_mut().push(b);
                Ok(Value::Nil)
            });
            let g = Rc::new(GeneratorHandle::new(body));
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Float(10.0))
            );
            assert_eq!(
                g.resume(Value::Str("first".into())).await.unwrap(),
                Some(Value::Float(20.0))
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
                Some(Value::Float(1.0))
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
                g.yield_(Value::Float(1.0)).await;
                Err(Control::Panic(AsError::new("boom")))
            });
            let g = Rc::new(GeneratorHandle::new(body));
            assert_eq!(
                g.resume(Value::Nil).await.unwrap(),
                Some(Value::Float(1.0))
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
                Some(Value::Float(1.0))
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
                while let Some(Value::Float(n)) = inner_for_body.resume(Value::Nil).await? {
                    let g = current_generator().unwrap();
                    g.yield_(Value::Float(n * 2.0)).await;
                }
                Ok(Value::Nil)
            });
            let outer = Rc::new(GeneratorHandle::new(outer_body));
            assert_eq!(
                outer.resume(Value::Nil).await.unwrap(),
                Some(Value::Float(2.0))
            );
            assert_eq!(
                outer.resume(Value::Nil).await.unwrap(),
                Some(Value::Float(4.0))
            );
            assert_eq!(outer.resume(Value::Nil).await.unwrap(), None);
            // The stack is balanced after all polling.
            assert!(current_generator().is_none());
        })
        .await;
    }
}
