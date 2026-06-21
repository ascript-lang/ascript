//! Workers Spec A: shared-nothing isolates. `serialize` is the value airlock;
//! `dispatch` builds the shippable code slice (entry fn + its transitive top-level
//! dependency closure, materialized as a `.aso` module fragment); `pool`/`isolate`
//! (later tasks) host the isolate pool + the `Send` byte-channel transport.

pub mod actor;
pub mod dispatch;
pub mod isolate;
pub mod pool;
pub mod serialize;
pub mod stream;
pub mod testrun;

use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use crate::worker::isolate::ChunkJob;
use std::cell::Cell;
use std::rc::Rc;

/// RAII guard that decrements an isolate's in-flight counter on drop. This ensures
/// the counter is decremented even if the caller-thread bridge task is aborted
/// (cancel-on-drop: the last `Value::future` clone drops → `SharedFuture::Drop` fires
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
    build_class_slice, build_class_slice_for_interp, build_code_slice,
    build_code_slice_for_interp, build_code_slice_for_static_method,
    build_code_slice_for_static_method_from_source, build_code_slice_from_source,
    build_stream_slice_for_interp,
};
pub use pool::pool_is_initialized;
#[cfg(feature = "net")]
pub(crate) use isolate::with_inline_dispatch;

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

impl WorkerCodeSlice {
    /// PAR (spec §3.2): a cheap clone of the slice so ONE build can serve every chunk
    /// of a `task.pmap`/`task.preduce` call. All fields are `Copy`/`Rc`-backed
    /// (`entry_aso: Rc<[u8]>`, `class_name: Option<Rc<str>>`, `entry_name: Rc<str>`),
    /// so this is a handful of refcount bumps — never a byte copy of the slice.
    pub(crate) fn clone_for_dispatch(&self) -> WorkerCodeSlice {
        WorkerCodeSlice {
            fn_id: self.fn_id,
            entry_aso: self.entry_aso.clone(),
            class_name: self.class_name.clone(),
            entry_name: self.entry_name.clone(),
        }
    }
}

/// Dispatch a `worker fn` call: ship the entry's code slice + the structured-clone
/// args to a pooled isolate (another OS thread) and return a `Value::future` that
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
///     cancel-on-drop: dropping the last `Value::future` aborts the bridge, which
///     drops the abort sender, which the isolate `select!`s on to cancel its run.
///
/// Public signature is UNCHANGED — delegates to `dispatch_worker_job` with `None`
/// (no chunk job) so all existing callers are byte-identical.
pub fn dispatch_worker(
    interp: &Interp,
    slice: WorkerCodeSlice,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, Control> {
    dispatch_worker_job(interp, slice, args, None, span)
}

/// PAR (spec §3.3): the core dispatch path, parameterised over an optional chunk job.
///
/// When `chunk` is `None` this is the exact pre-PAR path (byte-for-byte: same
/// sendability gate, same encode-as-one-array, same caps floor, same bridge — the
/// `None` branch in `isolate_loop` and `run_slice_inline` restores today's behaviour).
///
/// When `chunk` is `Some`, the isolate runs the native chunk DRIVER (`run_chunk_job`)
/// over the decoded data arg instead of a single entry call, and `run_slice_inline`
/// mirrors the same decomposition (venue-invariance §5.1).
///
/// Separated from `dispatch_worker` so Phase-2 `task.pmap`/`task.preduce` can call
/// it directly without touching `dispatch_worker`'s public signature.
pub(crate) fn dispatch_worker_job(
    interp: &Interp,
    slice: WorkerCodeSlice,
    args: Vec<Value>,
    chunk: Option<ChunkJob>,
    span: Span,
) -> Result<Value, Control> {
    // REPLAY §6 backstop: refuse pooled-isolate dispatch under a trace context. The
    // high-level call sites (worker fn / static worker fn / pmap / preduce) carry the
    // descriptive `what`; this is the by-construction net so no pooled path can record a
    // non-replayable trace. INERT inside an isolate (a fresh `Interp` is never traced) and
    // on the default path (`trace_active() == false`).
    interp.refuse_worker_under_trace("dispatching a worker fn", span)?;

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
    let args_array = Value::array_cell(crate::value::ArrayCell::new(args));
    let (encoded, encoded_shared) = serialize::encode(&args_array).map_err(|e| {
        Control::Panic(crate::error::AsError::at(e.message(), span))
    })?;

    // --- Build the future + transport channels. ---
    let fut = crate::task::SharedFuture::new();
    let cell = fut.cell();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel::<isolate::WorkerReply>();
    let (abort_tx, abort_rx) = tokio::sync::oneshot::channel::<()>();

    // Task 1.6: ship the bundled program's archive (if any) so the isolate installs it
    // before its re-run imports — read the `Rc` out here (no borrow across the dispatch).
    let archive_bytes = interp.worker_archive_bytes().map(|b| b.to_vec());
    let req = isolate::WorkerRequest {
        fn_id: slice.fn_id,
        // Always BUILD the request with the slice bytes; the pool's `send_to` suppresses
        // the re-send per isolate (it mirrors the isolate's `fn_id` cache and clears
        // `slice_bytes` once an isolate has been shipped them), and the isolate itself
        // dedups by `fn_id` and ignores any re-send as a belt-and-braces backstop.
        slice_bytes: Some(slice.entry_aso.to_vec()),
        archive_bytes,
        class_name: slice.class_name.as_deref().map(|s| s.to_string()),
        entry_name: slice.entry_name.to_string(),
        args: encoded,
        // SRV §3.7(b): the frozen `Arc<SharedNode>` side-vector travels alongside the
        // bytes (a `Send` field, NOT structured-clone). A `Value::shared` among the
        // args is shipped by `Arc` pointer — zero copy of the frozen graph.
        shared: encoded_shared,
        // FFI §4.5a: ship the dispatching isolate's caps as the pooled worker's
        // read-only floor (a `Send` side-channel field, not a `Value`). The isolate
        // installs it fresh per request + refuses a drop there.
        caps: Box::new(interp.caps()),
        // EMBED §6.4: ship the dispatching isolate's host-module FACTORIES the SAME way
        // `caps` rides (a `Send` side-channel, not a `Value`). The pooled isolate installs
        // them FRESH per request (clear-then-install) so a `worker fn` importing a
        // factory-registered `host:` module resolves it. Empty for a non-embed program.
        host_factories: interp
            .host_factories()
            .into_iter()
            .map(|(name, f)| (name.to_string(), f))
            .collect(),
        reply: reply_tx,
        abort: abort_rx,
        // PAR §3.3.2: thread the chunk job through to the isolate (None = pre-PAR path).
        chunk,
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
            // WASM §5.3.7: on wasm a pooled isolate can NEVER spawn (no threads — the
            // `bootstrap` chokepoint refuses), so `pool::dispatch` always returns `Err`.
            // The native inline-degradation would silently RUN the worker in-process,
            // masking the platform gap; on wasm we instead surface the clean Tier-2
            // platform error (never silent). `worker fn` / `task.pmap` / `task.preduce`
            // (the pooled forms) all funnel here.
            #[cfg(target_family = "wasm")]
            {
                let _ = req;
                return Err(Control::Panic(crate::error::AsError::at(
                    "workers are not available on this platform (wasm)",
                    span,
                )));
            }
            #[cfg(not(target_family = "wasm"))]
            return run_slice_inline(
                req.slice_bytes.as_deref(),
                req.archive_bytes.as_deref(),
                &req.entry_name,
                &req.args,
                &req.shared,
                *req.caps,
                // EMBED §6.4: forward the host-module factories so the inline-degraded
                // run installs them too (authority + host-module parity with the pooled
                // path — a `worker fn` importing a factory module works either way).
                &req.host_factories,
                // PAR §3.5: forward the chunk job to the inline path (venue-invariance §5.1).
                req.chunk,
                span,
            );
        }
    };

    // --- Caller-thread bridge: await the reply, decode, resolve the future. ---
    let interp_rc = interp.rc();
    let handle = crate::exec::spawn_local(async move {
        // `abort_tx` lives in this task; if the task is aborted (future dropped), it
        // is dropped, signalling the isolate to cancel.
        let _abort_tx = abort_tx;
        // RAII guard: decrements the inflight counter when dropped, even on task
        // abort (cancel-on-drop). Prevents inflight-counter drift when the caller
        // drops the future before the reply arrives.
        let _inflight_guard = InflightGuard(inflight);
        let reply = reply_rx.await;
        let result = match reply {
            Ok(isolate::WorkerReply::Ok(bytes, shared)) => {
                serialize::decode_with_shared(&bytes, &shared, &interp_rc)
                    .map_err(|e| Control::Panic(e.into()))
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

    Ok(Value::future(fut))
}

/// Inline-nesting path: a `worker fn` called from inside an isolate runs on the
/// current isolate's VM (no pool round-trip → deadlock-free). The entry global is
/// already defined (the enclosing slice shipped it transitively); we look it up and
/// call it, eagerly scheduled like any async call so the result is a `Value::future`.
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
    // (returns a `Value::future`; `await` drives it). Runs on the current isolate's
    // LocalSet — no cross-thread transport.
    let fut = crate::task::SharedFuture::new();
    let cell = fut.cell();
    let handle = crate::exec::spawn_local(async move {
        let r = vm.call_value(entry, args, span).await;
        cell.resolve(r);
    });
    fut.set_abort(handle.abort_handle());
    Ok(Value::future(fut))
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
/// PAR (spec §5.1, venue-invariance): when `chunk` is `Some`, the spawned task routes
/// through the SAME `run_chunk_job` decomposition as the pooled path, so the result is
/// byte-identical regardless of whether the job ran on a pool isolate or was degraded to
/// inline execution. The `None` path is today's exact pre-PAR call.
///
/// Returns a `Value::future` that resolves with the worker's result (scheduled on the
/// caller's `LocalSet`, like any async call).
// The inline executor mirrors the wire fields of a `WorkerRequest` (slice/archive bytes,
// entry, encoded args + shared side-vec, caps floor, the PAR chunk job, span) — a cohesive
// payload, not an accidental pile; folding them behind a struct would only rename the same
// fields. PAR added the 8th (`chunk`).
#[allow(clippy::too_many_arguments)]
// WASM §5.3.7: the pooled inline-degradation fallback is unreachable on wasm (the
// pooled funnel refuses with a Tier-2 platform error instead), so this is dead there.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
fn run_slice_inline(
    slice_bytes: Option<&[u8]>,
    archive_bytes: Option<&[u8]>,
    entry_name: &str,
    encoded_args: &[u8],
    encoded_shared: &[std::sync::Arc<crate::value::SharedNode>],
    caps: crate::stdlib::caps::CapSet,
    host_factories: &[(String, std::sync::Arc<crate::interp::HostModuleFactoryCore>)],
    chunk: Option<ChunkJob>,
    span: Span,
) -> Result<Value, Control> {
    // Build a fresh, shared-nothing Interp/Vm on THIS thread.
    let iso_interp = Rc::new(Interp::new());
    iso_interp.install_self();
    // FFI §4.5a: this caller-thread fallback is the pooled-worker equivalent — install
    // the caller's caps floor and refuse a drop, so a `worker fn` is gated identically
    // whether it runs on a pool isolate or degrades to inline (byte-identical authority).
    iso_interp.set_caps(caps);
    iso_interp.set_caps_drop_allowed(false);
    // EMBED §6.4: install the host-module factories so a `worker fn` importing a
    // factory-registered `host:` module resolves it on the inline-degraded path too.
    for (name, factory) in host_factories {
        let name_rc: Rc<str> = Rc::from(name.as_str());
        iso_interp.install_host_factory(&name_rc, factory);
    }
    let vm = crate::vm::Vm::new(iso_interp.clone());

    // Decode the args against the fresh interp (cycles / class reconstruction resolve
    // against its own globals once the slice is loaded — same as the isolate).
    let args = isolate::decode_args_with_shared(encoded_args, encoded_shared, &iso_interp)
        .map_err(|msg| Control::Panic(crate::error::AsError::at(msg, span)))?;

    let slice_owned: Option<Vec<u8>> = slice_bytes.map(|b| b.to_vec());
    let archive_owned: Option<Vec<u8>> = archive_bytes.map(|b| b.to_vec());
    let entry_owned = entry_name.to_string();

    let fut = crate::task::SharedFuture::new();
    let cell = fut.cell();
    let handle = crate::exec::spawn_local(async move {
        // Task 1.6: install the bundled program's archive (if any) BEFORE the slice loads,
        // so this inline fallback re-runs imports from memory exactly like a pool isolate.
        if let Some(bytes) = archive_owned.as_deref() {
            if let Err(msg) = isolate::install_module_archive(&vm, bytes) {
                cell.resolve(Err(Control::Panic(crate::error::AsError::at(msg, span))));
                return;
            }
        }
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
        // PAR §5.1 (venue-invariance): when a chunk job was requested, run the SAME
        // `run_chunk_job` decomposition as the pooled path (data is the first decoded arg).
        // The None path restores the pre-PAR single-call behaviour byte-for-byte.
        let r = match chunk {
            Some(ref job) => {
                let data = args.into_iter().next().unwrap_or_else(Value::nil);
                match isolate::run_chunk_job(&vm, entry, data, job, span).await {
                    Ok(v) => Ok(v),
                    Err(other) => Err(other),
                }
            }
            None => match vm.call_value(entry, args, span).await {
                Ok(v) => Ok(v),
                // A top-level `?` propagation inside the worker body ends with nil (matches
                // the isolate's WorkerReply handling).
                Err(Control::Propagate(_)) => Ok(Value::nil()),
                Err(Control::Exit(_)) => Err(Control::Panic(crate::error::AsError::at(
                    "exit() is not allowed inside a worker".to_string(),
                    span,
                ))),
                Err(other) => Err(other),
            },
        };
        cell.resolve(r);
    });
    fut.set_abort(handle.abort_handle());
    Ok(Value::future(fut))
}

/// FFI §4.5a — THE KEYSTONE: dispatch a worker onto a DEDICATED (single-tenant)
/// isolate carrying a REDUCED `CapSet`. This is the `run_in_worker({caps})` path.
///
/// Unlike the pooled path (`dispatch_worker`), this spawns a FRESH isolate for this
/// one job via [`isolate::spawn_isolate`]: the `Send` `CapSet` is captured DIRECTLY in
/// the `Send + 'static` `make_loop` closure (it never rides the byte channel and never
/// touches the structured-clone value serializer — it is not a `Value`). The closure
/// installs `caps` into the brand-new `Interp` **before** running the entry, runs the
/// one job, and the isolate is torn down on `IsolateHandle` drop. Because the `Interp`
/// is NEVER reused, an in-plugin `caps.drop` is durable AND cannot leak — there is no
/// "next request" on that `Interp`. The plugin keeps `caps_drop_allowed = true` (the
/// `Interp::new` default), so it CAN drop further (one-way), but never re-grant.
///
/// The reply crosses back as `Send` bytes over a `std::mpsc` back-channel; the caller
/// bridges it onto a `Value::future` decoded against the CALLER's interp (so the
/// returned plain data — `int`/`Bytes`/`Object` — reconstructs in the caller's heap).
pub fn dispatch_worker_dedicated(
    interp: &Interp,
    slice: WorkerCodeSlice,
    args: Vec<Value>,
    caps: crate::stdlib::caps::CapSet,
    span: Span,
) -> Result<Value, Control> {
    // REPLAY §6 backstop: refuse dedicated-isolate dispatch under a trace context. The
    // `run_in_worker` call site carries the descriptive `what`; this nets any other path.
    interp.refuse_worker_under_trace("dispatching a worker fn", span)?;

    // Sendability gate + encode (args wrapped as one array for one decode), exactly as
    // the pooled path: an FFI handle / closure / future arg is rejected with a field
    // path here, before any isolate spawn.
    for arg in &args {
        serialize::check_sendable(arg)
            .map_err(|e| Control::Panic(crate::error::AsError::at(e.message(), span)))?;
    }
    let args_array = Value::array_cell(crate::value::ArrayCell::new(args));
    let (encoded, encoded_shared) = serialize::encode(&args_array)
        .map_err(|e| Control::Panic(crate::error::AsError::at(e.message(), span)))?;

    let slice_bytes: Vec<u8> = slice.entry_aso.to_vec();
    let entry_name: String = slice.entry_name.to_string();
    // Task 1.6: capture the bundled program's archive bytes (if any) into the `Send`
    // `make_loop` closure so the dedicated isolate installs it before its re-run imports.
    let archive_bytes: Option<Vec<u8>> = interp.worker_archive_bytes().map(|b| b.to_vec());
    // EMBED §6.4: capture the host-module FACTORIES (a `Send + Sync` list, like the
    // `CapSet`) directly into the `Send` `make_loop` closure (path-a — it never rides the
    // byte channel). The dedicated isolate installs them ONCE at boot (single-tenant).
    let host_factories: Vec<(String, std::sync::Arc<crate::interp::HostModuleFactoryCore>)> = interp
        .host_factories()
        .into_iter()
        .map(|(name, f)| (name.to_string(), f))
        .collect();

    // `Send` back-channel for the one reply (the dedicated isolate is single-shot here).
    let (reply_tx, reply_rx) = std::sync::mpsc::channel::<isolate::WorkerReply>();

    // Spawn the dedicated isolate. The CapSet + slice + entry + reply sender are all
    // `Send` and captured directly in the `Send + 'static` closure.
    let handle = isolate::spawn_isolate(move |vm, mut rx| async move {
        let iso_interp = vm.interp().clone();
        // KEYSTONE: install the reduced caps into the fresh, single-tenant Interp
        // BEFORE running any plugin code. `caps_drop_allowed` stays true (the default):
        // a dedicated isolate is single-tenant, so an in-plugin drop is terminal.
        iso_interp.set_caps(caps);

        // EMBED §6.4: install the captured host-module factories on the fresh, single-
        // tenant isolate (once, at boot) so a `worker fn` importing a factory-registered
        // `host:` module resolves it. Empty for a non-embed program.
        for (name, factory) in &host_factories {
            let name_rc: Rc<str> = Rc::from(name.as_str());
            iso_interp.install_host_factory(&name_rc, factory);
        }

        // Task 1.6: install the bundled program's archive BEFORE the slice loads (the slice
        // re-runs the program's top-level imports). `None` (unbundled) installs nothing.
        if let Some(bytes) = archive_bytes.as_deref() {
            if let Err(msg) = isolate::install_module_archive(&vm, bytes) {
                let _ = reply_tx.send(isolate::WorkerReply::Panic(msg));
                return;
            }
        }

        // Load the slice's globals once.
        if let Err(msg) = isolate::load_slice(&vm, Some(&slice_bytes)).await {
            let _ = reply_tx.send(isolate::WorkerReply::Panic(msg));
            return;
        }
        // Wait for the single args message.
        let Some(args_bytes) = rx.recv().await else {
            return; // handle dropped before we got args (cancelled).
        };
        // SRV §3.7: the dedicated isolate's inbound channel is `Vec<u8>` only, so the
        // frozen-`Arc` side-vector is CAPTURED directly in this `Send` closure (path-a
        // style) rather than riding the byte channel — a `Value::shared` arg crosses by
        // `Arc` pointer (zero copy), reconstructed here against this isolate.
        let arg_values =
            match isolate::decode_args_with_shared(&args_bytes, &encoded_shared, &iso_interp) {
                Ok(vs) => vs,
                Err(msg) => {
                    let _ = reply_tx.send(isolate::WorkerReply::Panic(msg));
                    return;
                }
            };
        let entry = match vm.user_global(&entry_name) {
            Some(v) => v,
            None => {
                let _ = reply_tx.send(isolate::WorkerReply::Panic(format!(
                    "worker entry '{entry_name}' is not defined in the shipped code slice"
                )));
                return;
            }
        };
        let reply = match vm.call_value(entry, arg_values, crate::span::Span::new(0, 0)).await {
            Ok(v) => match serialize::encode(&v) {
                Ok((bytes, shared)) => isolate::WorkerReply::Ok(bytes, shared),
                Err(e) => isolate::WorkerReply::Panic(e.message()),
            },
            Err(Control::Panic(e)) => isolate::WorkerReply::Panic(e.message),
            Err(Control::Propagate(_)) => match serialize::encode(&Value::nil()) {
                Ok((bytes, shared)) => isolate::WorkerReply::Ok(bytes, shared),
                Err(e) => isolate::WorkerReply::Panic(e.message()),
            },
            Err(Control::Exit(_)) => {
                isolate::WorkerReply::Panic("exit() is not allowed inside a worker".to_string())
            }
        };
        let _ = reply_tx.send(reply);
        // The isolate loop ends when `rx` closes (handle dropped); we've sent our reply.
    })
    .map_err(|e| {
        Control::Panic(crate::error::AsError::at(
            format!("could not spawn a dedicated worker isolate: {e}"),
            span,
        ))
    })?;

    // Ship the args, then bridge the reply onto a Value::future. The isolate handle is
    // moved into the bridge task so it stays alive until the reply arrives (and is then
    // dropped → the isolate's thread joins → no zombie).
    if handle.tx.send(encoded).is_err() {
        return Err(Control::Panic(crate::error::AsError::at(
            "dedicated worker isolate terminated before receiving its input".to_string(),
            span,
        )));
    }

    let interp_rc = interp.rc();
    let fut = crate::task::SharedFuture::new();
    let cell = fut.cell();
    let bridge = crate::exec::spawn_local(async move {
        // Hold the handle alive across the blocking reply wait. The recv runs on a
        // blocking helper so the current-thread runtime is not stalled; on success the
        // result is decoded against the caller's interp.
        let _handle = handle;
        let reply = tokio::task::spawn_blocking(move || {
            reply_rx
                .recv_timeout(std::time::Duration::from_secs(300))
                .ok()
        })
        .await
        .ok()
        .flatten();
        let result = match reply {
            Some(isolate::WorkerReply::Ok(bytes, shared)) => {
                serialize::decode_with_shared(&bytes, &shared, &interp_rc)
                    .map_err(|e| Control::Panic(e.into()))
            }
            Some(isolate::WorkerReply::Panic(msg)) => {
                Err(Control::Panic(crate::error::AsError::at(msg, span)))
            }
            Some(isolate::WorkerReply::Cancelled) | None => {
                Err(Control::Panic(crate::error::AsError::at(
                    "dedicated worker isolate terminated unexpectedly".to_string(),
                    span,
                )))
            }
        };
        cell.resolve(result);
    });
    fut.set_abort(bridge.abort_handle());
    Ok(Value::future(fut))
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

    // ── PAR Task 1.2: end-to-end pooled chunk dispatch ───────────────────────

    /// PAR Task 1.2 Step 1: end-to-end POOLED chunk dispatch.
    /// Builds a slice for `worker fn double`, dispatches it with a `ChunkJob{Map,0,3}`
    /// and plain data `[1,2,3]`, awaits the future on a `LocalSet`, asserts `[2,4,6]`.
    ///
    /// Exercises `dispatch_worker_job(... Some(ChunkJob{Map,0,3}) ...)` → pool isolate
    /// → `run_chunk_job` → reply decode.
    #[test]
    fn dispatch_worker_job_pooled_map_chunk() {
        use super::{
            build_code_slice, dispatch_worker_job,
            isolate::{ChunkJob, ChunkKind},
        };
        use crate::value::Value;

        let src = "worker fn double(x) { return x * 2 }";
        let top = crate::compile::compile_source(src).expect("source compiles");
        let slice = build_code_slice(&top, "double", None).expect("slice builds");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");
        let local = tokio::task::LocalSet::new();

        local.block_on(&rt, async {
            // Build a fresh interp for the caller-side bridge (pool dispatches from here).
            let interp = std::rc::Rc::new(crate::interp::Interp::new());
            interp.install_self();

            // data = [1, 2, 3] — shipped as the single arg; chunk driver indexes into it.
            let data = Value::array_cell(crate::value::ArrayCell::new(vec![
                Value::int(1),
                Value::int(2),
                Value::int(3),
            ]));

            let job = ChunkJob { kind: ChunkKind::Map, start: 0, end: 3 };
            let span = crate::span::Span::new(0, 0);

            let fut_val = dispatch_worker_job(&interp, slice, vec![data], Some(job), span)
                .expect("dispatch_worker_job succeeds");

            // Drive the returned future to completion on the LocalSet.
            let result = match fut_val.kind() {
                crate::value::ValueKind::Future(f) => f.get().await,
                other => panic!("expected Future, got {other:?}"),
            };
            let out = result.expect("chunk job must succeed");
            assert_eq!(format!("{out}"), "[2, 4, 6]", "chunk map result must be [2, 4, 6]");
        });
    }
}
