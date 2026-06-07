//! Workers Spec A: shared-nothing isolates. `serialize` is the value airlock;
//! `dispatch` builds the shippable code slice (entry fn + its transitive top-level
//! dependency closure, materialized as a `.aso` module fragment); `pool`/`isolate`
//! (later tasks) host the isolate pool + the `Send` byte-channel transport.

pub mod dispatch;
pub mod isolate;
pub mod pool;
pub mod serialize;

use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use std::cell::Cell;
use std::rc::Rc;

/// RAII guard that decrements an isolate's in-flight counter on drop. This ensures
/// the counter is decremented even if the caller-thread bridge task is aborted
/// (cancel-on-drop: the last `Value::Future` clone drops → `SharedFuture::Drop` fires
/// the abort handle → the bridge `spawn_local` task is cancelled → this guard drops →
/// the counter is decremented). Without the guard the counter leaks on cancel, making
/// the isolate appear busier than it is and starving future jobs.
struct InflightGuard(Rc<Cell<usize>>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

pub use dispatch::{
    build_code_slice, build_code_slice_for_static_method,
    build_code_slice_for_static_method_from_source, build_code_slice_from_source,
};
pub use pool::pool_is_initialized;

/// The shippable bytecode payload for one worker fn: its compiled chunk plus its
/// transitive top-level dependency closure (other top-level `fn`s and literal
/// `const`s it references), serialized via the `.aso` writer as a self-contained
/// "module fragment", keyed by a stable function identity for per-isolate caching.
///
/// Running `entry_aso` on a FRESH isolate's `Vm` defines exactly the closure's
/// globals (and the entry) and nothing else from the original module — so the
/// isolate can fetch and call the entry with zero access to the original heap.
pub struct WorkerCodeSlice {
    /// Identity for the per-isolate code cache (a stable hash of the entry's
    /// `class_name` + name). A repeatedly-dispatched worker ships its bytecode at
    /// most once per isolate, keyed by this id (Task 8).
    pub fn_id: u64,
    /// The `.aso` bytes: the module fragment carrying the transitive deps + the
    /// entry fn define.
    pub entry_aso: Rc<[u8]>,
    /// `Some(class)` for a `static worker fn` on a class; `None` for a free
    /// `worker fn`. Task 8 binds the class on the far isolate.
    pub class_name: Option<Rc<str>>,
    /// The worker entry's global name (fetched on the far isolate to call it).
    pub entry_name: Rc<str>,
}

/// Dispatch a `worker fn` call: ship the entry's code slice + the structured-clone
/// args to a pooled isolate (another OS thread) and return a `Value::Future` that
/// resolves with the worker's result. Only BYTES cross the thread boundary — the
/// `!Send` `Interp`/`Value`s never leave the caller thread; the isolate builds its
/// own. The awaiting bridge lives on the caller thread.
///
/// Three paths:
///   - INLINE NESTING: when already inside an isolate (`pool::in_isolate()`), the
///     worker body runs inline on the current isolate's VM (its globals already
///     define the entry, shipped in the enclosing slice) and resolves immediately —
///     deadlock-free (an isolate must never block on the pool it lives in).
///   - SENDABILITY: each arg is gated by `check_sendable`; a violation is a
///     recoverable Tier-2 panic carrying the offending field path (anchored at the
///     call `span`).
///   - DISPATCH: encode the args (as one array, preserving cross-arg sharing), build
///     a `SharedFuture` + reply/abort `oneshot`s, hand the request to the pool, and
///     `spawn_local` a small caller-thread bridge that awaits the reply, decodes the
///     result against `interp`, and resolves the future. `set_abort` wires
///     cancel-on-drop: dropping the last `Value::Future` aborts the bridge, which
///     drops the abort sender, which the isolate `select!`s on to cancel its run.
pub fn dispatch_worker(
    interp: &Interp,
    slice: WorkerCodeSlice,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, Control> {
    // --- Inline nesting: run on the current isolate, no re-dispatch. ---
    // (The engine hooks call `dispatch_worker_inline` directly when in an isolate so
    // they can skip the cross-thread slice build entirely; this guard keeps
    // `dispatch_worker` correct if called inline anyway.)
    if pool::in_isolate() {
        return dispatch_worker_inline(interp, &slice.entry_name, args, span);
    }

    // --- Sendability gate + encode (args wrapped as one array for one decode). ---
    for arg in &args {
        serialize::check_sendable(arg).map_err(|e| {
            Control::Panic(crate::error::AsError::at(e.message(), span))
        })?;
    }
    let args_array = Value::Array(crate::value::ArrayCell::new(args));
    let encoded = serialize::encode(&args_array).map_err(|e| {
        Control::Panic(crate::error::AsError::at(e.message(), span))
    })?;

    // --- Build the future + transport channels. ---
    let fut = crate::task::SharedFuture::new();
    let cell = fut.cell();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel::<isolate::WorkerReply>();
    let (abort_tx, abort_rx) = tokio::sync::oneshot::channel::<()>();

    let req = isolate::WorkerRequest {
        fn_id: slice.fn_id,
        // Always ship the slice; the isolate caches by fn_id and ignores re-sends.
        slice_bytes: Some(slice.entry_aso.to_vec()),
        class_name: slice.class_name.as_deref().map(|s| s.to_string()),
        entry_name: slice.entry_name.to_string(),
        args: encoded,
        reply: reply_tx,
        abort: abort_rx,
    };
    let inflight = match pool::dispatch(req) {
        Ok(inflight) => inflight,
        // GRACEFUL DEGRADATION: no isolate available and none spawnable (memory /
        // thread-limit pressure). Run the worker INLINE on the caller thread — correct
        // result, just not parallel. We load the SAME code slice into a fresh
        // in-process `Vm` and run the entry exactly as an isolate would (just without a
        // thread). Engine-independent (works on the tree-walker caller, which has no
        // VM of its own); preserves shared-nothing semantics (a fresh slice run).
        Err(req) => {
            return run_slice_inline(
                req.slice_bytes.as_deref(),
                &req.entry_name,
                &req.args,
                span,
            );
        }
    };

    // --- Caller-thread bridge: await the reply, decode, resolve the future. ---
    let interp_rc = interp.rc();
    let handle = tokio::task::spawn_local(async move {
        // `abort_tx` lives in this task; if the task is aborted (future dropped), it
        // is dropped, signalling the isolate to cancel.
        let _abort_tx = abort_tx;
        // RAII guard: decrements the inflight counter when dropped, even on task
        // abort (cancel-on-drop). Prevents inflight-counter drift when the caller
        // drops the future before the reply arrives.
        let _inflight_guard = InflightGuard(inflight);
        let reply = reply_rx.await;
        let result = match reply {
            Ok(isolate::WorkerReply::Ok(bytes)) => {
                serialize::decode(&bytes, &interp_rc).map_err(|e| Control::Panic(e.into()))
            }
            Ok(isolate::WorkerReply::Panic(msg)) => {
                Err(Control::Panic(crate::error::AsError::at(msg, span)))
            }
            Ok(isolate::WorkerReply::Cancelled) => {
                Err(Control::Panic(crate::error::AsError::at(
                    "worker was cancelled".to_string(),
                    span,
                )))
            }
            // The isolate dropped the reply sender without replying (it panicked).
            Err(_) => Err(Control::Panic(crate::error::AsError::at(
                "worker isolate terminated unexpectedly".to_string(),
                span,
            ))),
        };
        cell.resolve(result);
    });
    fut.set_abort(handle.abort_handle());

    Ok(Value::Future(fut))
}

/// Inline-nesting path: a `worker fn` called from inside an isolate runs on the
/// current isolate's VM (no pool round-trip → deadlock-free). The entry global is
/// already defined (the enclosing slice shipped it transitively); we look it up and
/// call it, eagerly scheduled like any async call so the result is a `Value::Future`.
///
/// Engine hooks call this DIRECTLY when `pool::in_isolate()`, so they avoid building
/// a cross-thread code slice (the isolate's `Interp` has no `worker_source` and needs
/// none — the entry is already a global on its VM).
pub fn dispatch_worker_inline(
    interp: &Interp,
    entry_name: &str,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, Control> {
    let vm = interp.vm().ok_or_else(|| {
        Control::Panic(crate::error::AsError::at(
            "inline worker dispatch requires a VM (internal invariant)".to_string(),
            span,
        ))
    })?;
    let entry = vm.user_global(entry_name).ok_or_else(|| {
        Control::Panic(crate::error::AsError::at(
            format!(
                "nested worker '{entry_name}' is not available in the enclosing worker's code slice"
            ),
            span,
        ))
    })?;

    // Eagerly schedule the inline body so it behaves like a normal async call
    // (returns a `Value::Future`; `await` drives it). Runs on the current isolate's
    // LocalSet — no cross-thread transport.
    let fut = crate::task::SharedFuture::new();
    let cell = fut.cell();
    let handle = tokio::task::spawn_local(async move {
        let r = vm.call_value(entry, args, span).await;
        cell.resolve(r);
    });
    fut.set_abort(handle.abort_handle());
    Ok(Value::Future(fut))
}

/// Caller-thread INLINE FALLBACK (graceful degradation, #1): when the pool cannot
/// place a job on any isolate and cannot spawn one (memory / thread-limit pressure),
/// run the worker on the caller thread by loading the SAME code slice into a FRESH
/// in-process `Vm` and calling the entry — exactly what an isolate does, minus the
/// thread. Engine-independent: builds its own `Interp`/`Vm`, so it works even on the
/// tree-walker caller (which has no VM). Preserves shared-nothing semantics (a fresh
/// slice run, no access to the caller's heap), so the result is byte-identical to the
/// parallel path; only the parallelism is lost.
///
/// Returns a `Value::Future` that resolves with the worker's result (scheduled on the
/// caller's `LocalSet`, like any async call).
fn run_slice_inline(
    slice_bytes: Option<&[u8]>,
    entry_name: &str,
    encoded_args: &[u8],
    span: Span,
) -> Result<Value, Control> {
    // Build a fresh, shared-nothing Interp/Vm on THIS thread.
    let iso_interp = Rc::new(Interp::new());
    iso_interp.install_self();
    let vm = crate::vm::Vm::new(iso_interp.clone());

    // Decode the args against the fresh interp (cycles / class reconstruction resolve
    // against its own globals once the slice is loaded — same as the isolate).
    let args = isolate::decode_args(encoded_args, &iso_interp)
        .map_err(|msg| Control::Panic(crate::error::AsError::at(msg, span)))?;

    let slice_owned: Option<Vec<u8>> = slice_bytes.map(|b| b.to_vec());
    let entry_owned = entry_name.to_string();

    let fut = crate::task::SharedFuture::new();
    let cell = fut.cell();
    let handle = tokio::task::spawn_local(async move {
        // Load the slice's globals into the fresh Vm.
        if let Err(msg) = isolate::load_slice(&vm, slice_owned.as_deref()).await {
            cell.resolve(Err(Control::Panic(crate::error::AsError::at(msg, span))));
            return;
        }
        let entry = match vm.user_global(&entry_owned) {
            Some(v) => v,
            None => {
                cell.resolve(Err(Control::Panic(crate::error::AsError::at(
                    format!("worker entry '{entry_owned}' is not defined in the code slice"),
                    span,
                ))));
                return;
            }
        };
        let r = match vm.call_value(entry, args, span).await {
            Ok(v) => Ok(v),
            // A top-level `?` propagation inside the worker body ends with nil (matches
            // the isolate's WorkerReply handling).
            Err(Control::Propagate(_)) => Ok(Value::Nil),
            Err(Control::Exit(_)) => Err(Control::Panic(crate::error::AsError::at(
                "exit() is not allowed inside a worker".to_string(),
                span,
            ))),
            Err(other) => Err(other),
        };
        cell.resolve(r);
    });
    fut.set_abort(handle.abort_handle());
    Ok(Value::Future(fut))
}

#[cfg(test)]
mod tests {
    use super::InflightGuard;
    use std::cell::Cell;
    use std::rc::Rc;

    /// RAII guard decrements even when dropped early (simulates cancel-on-drop:
    /// the bridge task is aborted before `reply_rx.await` resolves, so only the
    /// guard's `Drop` decrements the counter — never a manual `.set()` call).
    #[test]
    fn inflight_guard_decrements_on_drop() {
        let counter = Rc::new(Cell::new(3usize));
        let guard = InflightGuard(counter.clone());
        assert_eq!(counter.get(), 3);
        drop(guard);
        assert_eq!(counter.get(), 2, "guard must decrement on drop");
    }

    /// Guard saturates at zero — never wraps around.
    #[test]
    fn inflight_guard_saturates_at_zero() {
        let counter = Rc::new(Cell::new(0usize));
        let guard = InflightGuard(counter.clone());
        drop(guard);
        assert_eq!(counter.get(), 0, "saturating_sub must not underflow");
    }
}
