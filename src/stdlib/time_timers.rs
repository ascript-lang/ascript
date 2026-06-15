//! `std/time` timers — `interval`, `debounce`, `throttle`.
//!
//! These three primitives extend `std/time` with timer-based control-flow
//! utilities. They are NOT feature-gated (tokio timers are always present).
//!
//! ## interval(ms) → interval-handle
//! Returns a native resource handle.  The script calls `await iv.tick()` to
//! park until the next period fires.  Uses `tokio::time::interval` internally.
//! The first tick fires immediately (tokio's default `MissedTickBehavior::Burst`
//! is left as-is; callers that want to skip the initial immediate tick can just
//! discard the first `tick()` call).
//!
//! ## debounce(fn, ms) → callable-wrapper
//! Returns a `Value::NativeMethod` whose receiver is a `DebounceWrapper` resource.
//! Each call to the wrapper resets a timer: any previously-scheduled delayed call
//! is cancelled (cancel-on-drop via `AbortHandle::abort()`), and a new task is
//! spawned that sleeps for `ms` then calls `fn(args)`.  The return value of the
//! deferred call is discarded (fire-and-forget trailing edge).
//!
//! ## throttle(fn, ms) → callable-wrapper
//! Returns a `Value::NativeMethod` whose receiver is a `ThrottleWrapper` resource.
//! Each call checks the monotonic time since the last fire.  If `>= ms` has
//! elapsed (or it is the first call), `fn(args)` is called synchronously and the
//! last-fire timestamp is updated (leading-edge).  Otherwise the call is a no-op.
//!
//! ## Borrow discipline
//! All state mutation goes through `take_resource` / `return_resource` (the
//! take-out-across-await pattern) so no `RefCell` guard is held across an `.await`.

use super::{arg, want_number};
use crate::error::AsError;
use crate::interp::{Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, OwnedKind, Value, ValueKind};
use std::rc::Rc;
use tokio::task::AbortHandle;

// ── DebounceState ────────────────────────────────────────────────────────────

/// Mutable state behind a debounce wrapper handle.
///
/// Stored in `ResourceState::DebounceWrapper`.  Mutation uses the
/// take-out-across-await pattern: the resource is `take_resource`'d before any
/// `.await` and `return_resource`'d after, so no borrow is held across an await.
pub struct DebounceState {
    /// The script function to invoke on the trailing edge.
    pub func: Value,
    /// Debounce window in milliseconds.
    pub ms: u64,
    /// `AbortHandle` of the currently-pending delayed call, if any.
    ///
    /// IMPORTANT: a `tokio::task::AbortHandle` does NOT abort on `Drop` — its
    /// `Drop` is only a refcount decrement, never `remote_abort()`. To actually
    /// cancel the pending task we must call `.abort()` explicitly: each new call
    /// does so before scheduling the next one, and the [`Drop`] impl below does
    /// so when the wrapper itself is dropped — preserving cancel-on-drop.
    pub pending: Option<AbortHandle>,
}

impl Drop for DebounceState {
    fn drop(&mut self) {
        // Cancel-on-drop: explicitly abort the pending delayed call so a
        // dropped/replaced debounce wrapper does not leave an orphaned fire.
        // (AbortHandle's own Drop would NOT do this.)
        if let Some(h) = self.pending.take() {
            h.abort();
        }
    }
}

// ── ThrottleState ────────────────────────────────────────────────────────────

/// Mutable state behind a throttle wrapper handle.
///
/// Stored in `ResourceState::ThrottleWrapper`.
pub struct ThrottleState {
    /// The script function to invoke on the leading edge.
    pub func: Value,
    /// Throttle window in milliseconds.
    pub ms: u64,
    /// Monotonic instant of the last successful fire, or `None` if never fired.
    pub last_fire: Option<std::time::Instant>,
}

// ── factory functions (called from mod.rs call_time) ─────────────────────────

/// `time.interval(ms)` — create and register a tokio `Interval` resource.
/// Returns a `Value::Native(Interval)` handle; script accesses `.tick()` on it.
pub fn create_interval(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let ms = want_number(&arg(args, 0), span, "time.interval")?;
    // Reject anything that would truncate to a zero Duration: tokio's
    // `interval` panics ("period must be non-zero") on a 0 period, and that
    // would surface as a raw Rust panic rather than a catchable Tier-2 AsError.
    // A fractional `ms >= 1` truncates toward zero (e.g. 1.7 → 1ms), which is fine.
    if ms <= 0.0 || (ms as u64) == 0 {
        return Err(AsError::at("time.interval: ms must be >= 1", span).into());
    }
    let period = std::time::Duration::from_millis(ms as u64);
    let iv = tokio::time::interval(period);
    let handle = interp.register_resource(
        NativeKind::Interval,
        indexmap::IndexMap::new(),
        ResourceState::Interval(Box::new(iv)),
    );
    Ok(handle)
}

/// `time.debounce(fn, ms)` — create a debounce wrapper.
/// Returns a `Value::NativeMethod { receiver: DebounceWrapper, method: "call" }`.
/// Invoking the returned value as `wrapper(args)` dispatches through
/// `call_native_method` → `call_debounce_method`.
pub fn create_debounce(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let func = arg(args, 0);
    let ms_f = want_number(&arg(args, 1), span, "time.debounce ms")?;
    if ms_f <= 0.0 {
        return Err(AsError::at("time.debounce: ms must be a positive number", span).into());
    }
    let state = DebounceState {
        func,
        ms: ms_f as u64,
        pending: None,
    };
    let native = interp.register_resource(
        NativeKind::DebounceWrapper,
        indexmap::IndexMap::new(),
        ResourceState::DebounceWrapper(state),
    );
    // Return a NativeMethod so `wrapper(args)` is valid in call position.
    let obj = match native.kind() {
        ValueKind::Native(o) => o.clone(),
        _ => unreachable!(),
    };
    Ok(Value::NativeMethod(Rc::new(NativeMethod {
        receiver: obj,
        method: "call".to_string(),
    })))
}

/// `time.throttle(fn, ms)` — create a throttle wrapper.
/// Returns a `Value::NativeMethod { receiver: ThrottleWrapper, method: "call" }`.
pub fn create_throttle(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let func = arg(args, 0);
    let ms_f = want_number(&arg(args, 1), span, "time.throttle ms")?;
    if ms_f <= 0.0 {
        return Err(AsError::at("time.throttle: ms must be a positive number", span).into());
    }
    let state = ThrottleState {
        func,
        ms: ms_f as u64,
        last_fire: None,
    };
    let native = interp.register_resource(
        NativeKind::ThrottleWrapper,
        indexmap::IndexMap::new(),
        ResourceState::ThrottleWrapper(state),
    );
    let obj = match native.kind() {
        ValueKind::Native(o) => o.clone(),
        _ => unreachable!(),
    };
    Ok(Value::NativeMethod(Rc::new(NativeMethod {
        receiver: obj,
        method: "call".to_string(),
    })))
}

// ── Interp method handlers ────────────────────────────────────────────────────

impl Interp {
    /// Handle `.tick()` on an `Interval` handle.
    ///
    /// Takes the interval out of the resource table, awaits one tick, then
    /// puts it back — never holding the RefCell borrow across `.await`.
    pub(crate) async fn call_interval_method(
        &self,
        m: &Rc<NativeMethod>,
        _args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        if m.method != "tick" {
            return Err(AsError::at(
                format!("interval has no method '{}' (try 'tick')", m.method),
                span,
            )
            .into());
        }
        let id = m.receiver.id;
        // Take the interval out so no RefCell borrow is held across the await.
        let mut iv = match self.take_resource(id) {
            Some(ResourceState::Interval(iv)) => iv,
            _ => {
                return Err(AsError::at("interval.tick: handle was already closed", span).into());
            }
        };
        // Await the next tick outside any borrow.
        iv.tick().await;
        // Put the interval back.
        self.return_resource(id, ResourceState::Interval(iv));
        Ok(Value::nil())
    }

    /// Handle a bare call on a `DebounceWrapper` handle (method == "call").
    ///
    /// Cancels any pending delayed call, then spawns a new fire-and-forget task
    /// that sleeps for `ms` and calls `fn(args)`.  Cancel-on-drop: aborting the
    /// old `AbortHandle` cancels the sleeping task before it fires.
    pub(crate) async fn call_debounce_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        if m.method != "call" {
            return Err(AsError::at(
                format!(
                    "debounce wrapper has no method '{}' (call it as a function)",
                    m.method
                ),
                span,
            )
            .into());
        }
        let id = m.receiver.id;

        // Take state out so we can mutate it and spawn without holding a borrow.
        let mut state = match self.take_resource(id) {
            Some(ResourceState::DebounceWrapper(s)) => s,
            _ => {
                return Err(AsError::at("debounce: handle is no longer valid", span).into());
            }
        };

        // Cancel any in-flight pending task.
        if let Some(handle) = state.pending.take() {
            handle.abort();
        }

        // Clone what we need for the spawned task.
        let func = state.func.clone();
        let ms = state.ms;
        let vm = self.rc();

        // Spawn the delayed call. The result is discarded (trailing-edge
        // fire-and-forget). Cancellation is explicit, never implicit: the NEXT
        // call aborts this task via the stored AbortHandle (below), and
        // `DebounceState::Drop` aborts it if the wrapper itself is dropped before
        // the window expires (an AbortHandle's own Drop does NOT abort).
        let jh = tokio::task::spawn_local(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            // Calling an `async fn` returns Ok(Value::Future(..)) WITHOUT running
            // the body — so we must drive that inner future to completion here,
            // otherwise an async callback would silently never run. (Mirror of
            // `task.retry`.) The driven future's result/panic is swallowed:
            // a debounced fire is a deferred side-effect, not an awaited value.
            if let Ok(v) = vm.call_value(func, args, Span::new(0, 0)).await {
                if let OwnedKind::Future(f) = v.into_kind() {
                    let _ = f.get().await;
                }
            }
        });

        // Store the new abort handle so the NEXT call (or Drop) can cancel it.
        state.pending = Some(jh.abort_handle());

        // Return state to the table before yielding.
        self.return_resource(id, ResourceState::DebounceWrapper(state));

        Ok(Value::nil())
    }

    /// Handle a bare call on a `ThrottleWrapper` handle (method == "call").
    ///
    /// If `ms` has elapsed since the last fire (or it is the first call),
    /// calls `fn(args)` immediately (leading-edge) and records the time.
    /// Otherwise the call is a no-op.
    pub(crate) async fn call_throttle_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        if m.method != "call" {
            return Err(AsError::at(
                format!(
                    "throttle wrapper has no method '{}' (call it as a function)",
                    m.method
                ),
                span,
            )
            .into());
        }
        let id = m.receiver.id;

        // Take state out — no await below while state is borrowed.
        let mut state = match self.take_resource(id) {
            Some(ResourceState::ThrottleWrapper(s)) => s,
            _ => {
                return Err(AsError::at("throttle: handle is no longer valid", span).into());
            }
        };

        let now = std::time::Instant::now();
        let should_fire = match state.last_fire {
            None => true,
            Some(last) => now.duration_since(last).as_millis() as u64 >= state.ms,
        };

        if should_fire {
            state.last_fire = Some(now);
        }

        let func = if should_fire {
            Some(state.func.clone())
        } else {
            None
        };

        // Return state before any await.
        self.return_resource(id, ResourceState::ThrottleWrapper(state));

        if let Some(f) = func {
            // Fire on the leading edge; the return value is not surfaced.
            // Calling an `async fn` returns Ok(Value::Future(..)) WITHOUT running
            // the body, so drive that inner future to completion here — otherwise
            // an async callback would silently never run. (Mirror of `task.retry`.)
            if let Ok(v) = self.call_value(f, args, span).await {
                if let OwnedKind::Future(fut) = v.into_kind() {
                    let _ = fut.get().await;
                }
            }
        }

        Ok(Value::nil())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// `DebounceState::Drop` must explicitly `.abort()` the pending task.
    ///
    /// This is the cancel-on-drop guarantee that a bare `AbortHandle` does NOT
    /// provide (its own `Drop` is a refcount decrement, never `remote_abort()`).
    /// We spawn a task that flips a flag after a delay, store its `AbortHandle`
    /// in a `DebounceState`, drop the state BEFORE the delay elapses, then wait
    /// past it and assert the flag never flipped — i.e. the task was aborted.
    #[tokio::test]
    async fn drop_aborts_pending_task() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // A flag the spawned task would set if it were allowed to run.
                let fired = Rc::new(Cell::new(false));
                let fired_task = fired.clone();

                let jh = tokio::task::spawn_local(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    fired_task.set(true);
                });

                // Build a DebounceState holding this task's AbortHandle, then
                // drop it immediately (before the 30ms fire).
                let state = DebounceState {
                    func: Value::nil(),
                    ms: 30,
                    pending: Some(jh.abort_handle()),
                };
                drop(state); // <- DebounceState::Drop must .abort() the task

                // Wait well past the task's fire time.
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;

                assert!(
                    !fired.get(),
                    "pending task fired despite the DebounceState being dropped — \
                     cancel-on-drop is broken (AbortHandle Drop alone does NOT abort)"
                );
            })
            .await;
    }
}
