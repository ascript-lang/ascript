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
use std::rc::Rc;

pub use dispatch::{build_code_slice, build_code_slice_from_source};
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
    let inflight = pool::dispatch(req);

    // --- Caller-thread bridge: await the reply, decode, resolve the future. ---
    let interp_rc = interp.rc();
    let handle = tokio::task::spawn_local(async move {
        // `abort_tx` lives in this task; if the task is aborted (future dropped), it
        // is dropped, signalling the isolate to cancel.
        let _abort_tx = abort_tx;
        let reply = reply_rx.await;
        inflight.set(inflight.get().saturating_sub(1));
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
