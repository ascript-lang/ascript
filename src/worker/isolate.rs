//! One worker isolate: a dedicated OS thread hosting a fresh, shared-NOTHING
//! `Interp`/`Vm` on its own current-thread tokio runtime + `LocalSet` (mirroring
//! [`crate::run_on_worker_stack`]). The isolate owns NO `Value` and NO `Interp`
//! reference from the caller — the runtime is `!Send`, so each isolate constructs
//! its own. Only `Send` BYTES cross the boundary: a [`WorkerRequest`] carries the
//! structured-clone-encoded args + (on first use) the `.aso` code slice; a
//! [`WorkerReply`] carries the encoded result or a panic message.
//!
//! Per-isolate code cache: a `worker fn`'s slice (`fn_id` → loaded globals) is
//! materialized into the isolate's `Vm` exactly once; subsequent requests for the
//! same `fn_id` reuse the already-defined globals.

use crate::value::Value;
use crate::vm::chunk::{Chunk, FnProto};
use crate::vm::value_ext::{Closure, RunOutcome};
use crate::vm::Vm;
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use tokio::sync::{mpsc, oneshot};

thread_local! {
    /// Set TRUE for the lifetime of an isolate's run loop. `dispatch_worker` reads it
    /// (via [`crate::worker::pool::in_isolate`]) to run a NESTED `worker fn` inline in
    /// the current isolate rather than re-dispatching to the pool — deadlock-free
    /// (Spec A §7), since an isolate must never block waiting on the pool it lives in.
    static IN_ISOLATE: Cell<bool> = const { Cell::new(false) };
}

/// Whether the current thread is executing inside a worker isolate.
pub(crate) fn in_isolate() -> bool {
    IN_ISOLATE.with(|c| c.get())
}

/// One unit of work shipped to an isolate. Every field is `Send` (bytes / plain
/// scalars / channels) — no `Value`, no `Interp`, no `Rc<Chunk>` crosses.
pub struct WorkerRequest {
    /// Stable identity of the worker entry (keys the per-isolate code cache).
    pub fn_id: u64,
    /// The `.aso` code-slice bytes. `Some` when the isolate has not yet cached
    /// `fn_id`; `None` once it has (the caller-side pool tracks this per isolate).
    pub slice_bytes: Option<Vec<u8>>,
    /// `Some(class)` for a `static worker fn` (currently advisory — the entry is a
    /// free top-level fn in the slice; kept for diagnostics + future class binding).
    #[allow(dead_code)]
    pub class_name: Option<String>,
    /// The worker entry's global name, fetched from the isolate's globals to call.
    pub entry_name: String,
    /// Structured-clone-encoded positional args (see `serialize::encode`).
    pub args: Vec<u8>,
    /// Where the isolate sends the reply (result bytes or a panic message).
    pub reply: oneshot::Sender<WorkerReply>,
    /// Cancel signal: the caller drops the paired sender on `Value::Future` drop;
    /// the isolate `select!`s on this to abort the in-flight run (cancel-on-drop).
    pub abort: oneshot::Receiver<()>,
}

/// The isolate's response. `Send` bytes / a message string only.
pub enum WorkerReply {
    /// The structured-clone-encoded result `Value`.
    Ok(Vec<u8>),
    /// An uncaught Tier-2 panic message raised inside the worker body.
    Panic(String),
    /// The run was cancelled (the caller dropped the future before it resolved).
    Cancelled,
}

/// A live isolate: the `Send` request channel into it plus its joinable thread.
pub struct Isolate {
    /// Sends [`WorkerRequest`]s into the isolate's run loop.
    pub tx: mpsc::UnboundedSender<WorkerRequest>,
    /// The OS thread handle. Joined on pool teardown (the channel closing ends the
    /// loop). `Option` so `Drop`/teardown can take it.
    pub thread: Option<std::thread::JoinHandle<()>>,
}

/// The base native stack (bytes) each isolate thread reserves. Deliberately MODEST
/// (8 MiB) rather than the main thread's 512 MiB `WORKER_STACK_SIZE`: an isolate's VM
/// goes through the SAME `stacker::maybe_grow` re-entry funnels (`src/vm/stack.rs`
/// `grow_future`, invoked by `call_value`/`invoke_compiled_method`/generator resume),
/// so deep recursion grows fresh heap segments on demand and still reaches the SP3
/// `MAX_CALL_DEPTH` cap cleanly — without each isolate reserving half a gigabyte of
/// virtual address space. Reserving 512 MiB × `num_cpus` per subprocess is what made
/// `thread::Builder::spawn` fail intermittently under parallel test/production load
/// (address-space / thread-limit pressure); 8 MiB × `num_cpus` does not.
pub const ISOLATE_STACK_SIZE: usize = 8 * 1024 * 1024;

impl Isolate {
    /// Spawn a fresh isolate thread (own runtime + `LocalSet` + `Interp`/`Vm`) and
    /// return the channel handle to it.
    ///
    /// FALLIBLE by design: under memory / thread-limit pressure `thread::Builder::spawn`
    /// can return `Err`. The pool treats that as "cannot grow right now" and the
    /// dispatcher gracefully degrades to running the worker INLINE on the caller thread
    /// (correct result, just not parallel) — never a panic. So this returns the spawn
    /// error instead of `.expect()`ing.
    pub fn spawn() -> std::io::Result<Isolate> {
        let (tx, rx) = mpsc::unbounded_channel::<WorkerRequest>();
        let thread = std::thread::Builder::new()
            .name("ascript-isolate".to_string())
            .stack_size(ISOLATE_STACK_SIZE)
            .spawn(move || run_isolate(rx))?;
        Ok(Isolate {
            tx,
            thread: Some(thread),
        })
    }
}

/// The isolate thread entry: build the runtime + a fresh `Interp`/`Vm`, then drive
/// the request loop on the `LocalSet`. Returns when the request channel closes.
///
/// If the tokio runtime cannot be built (resource pressure), the thread simply ends
/// WITHOUT panicking — the request channel's senders see no receiver, so every
/// dispatch to this isolate fails its reply `oneshot` and surfaces as a recoverable
/// "isolate terminated" panic at the caller's `await`, rather than aborting the
/// process. (In practice the pool prefers other isolates / inline execution.)
fn run_isolate(rx: mpsc::UnboundedReceiver<WorkerRequest>) {
    IN_ISOLATE.with(|c| c.set(true));
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return, // graceful: no panic; the channel closing signals callers
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, isolate_loop(rx));
}

/// The per-isolate request loop. A fresh `Interp`/`Vm` is created ONCE and reused for
/// every request on this isolate (shared-nothing across isolates, but stateful within
/// one — Spec A workers are stateless, so reuse is observably a fresh-globals run per
/// distinct slice). `loaded` tracks which slices' globals are already defined.
async fn isolate_loop(mut rx: mpsc::UnboundedReceiver<WorkerRequest>) {
    let interp = Rc::new(crate::interp::Interp::new());
    interp.install_self();
    let vm = Vm::new(interp.clone());
    let mut loaded: HashSet<u64> = HashSet::new();

    while let Some(req) = rx.recv().await {
        let WorkerRequest {
            fn_id,
            slice_bytes,
            class_name: _,
            entry_name,
            args,
            reply,
            abort,
        } = req;

        // Ensure the slice's globals are defined on this isolate's Vm (once per fn_id).
        if !loaded.contains(&fn_id) {
            match load_slice(&vm, slice_bytes.as_deref()).await {
                Ok(()) => {
                    loaded.insert(fn_id);
                }
                Err(msg) => {
                    let _ = reply.send(WorkerReply::Panic(msg));
                    continue;
                }
            }
        }

        // Decode the args against THIS isolate's interp (cycles + class reconstruction
        // resolve against the isolate's own globals).
        let arg_values = match decode_args(&args, &interp) {
            Ok(vs) => vs,
            Err(msg) => {
                let _ = reply.send(WorkerReply::Panic(msg));
                continue;
            }
        };

        // Fetch + run the entry, racing the run against the abort signal.
        let entry = match vm.user_global(&entry_name) {
            Some(v) => v,
            None => {
                let _ = reply.send(WorkerReply::Panic(format!(
                    "worker entry '{entry_name}' is not defined in the shipped code slice"
                )));
                continue;
            }
        };

        let run = vm.call_value(entry, arg_values, crate::span::Span::new(0, 0));
        tokio::pin!(run);
        let reply_msg = tokio::select! {
            biased;
            _ = abort => WorkerReply::Cancelled,
            result = &mut run => match result {
                Ok(v) => match crate::worker::serialize::encode(&v) {
                    Ok(bytes) => WorkerReply::Ok(bytes),
                    Err(e) => WorkerReply::Panic(e.message()),
                },
                Err(crate::interp::Control::Panic(e)) => WorkerReply::Panic(e.message),
                // A top-level `?` propagation inside the worker body ends with nil.
                Err(crate::interp::Control::Propagate(_)) => {
                    match crate::worker::serialize::encode(&Value::Nil) {
                        Ok(bytes) => WorkerReply::Ok(bytes),
                        Err(e) => WorkerReply::Panic(e.message()),
                    }
                }
                Err(crate::interp::Control::Exit(_)) => {
                    WorkerReply::Panic("exit() is not allowed inside a worker".to_string())
                }
            },
        };
        let _ = reply.send(reply_msg);
    }
}

/// Load a code slice's fragment `.aso` into a `Vm`, defining its globals (the worker
/// entry + its transitive top-level deps). Returns the rendered error message on any
/// failure (decode / run). Shared by the isolate loop AND the caller-thread inline
/// fallback (`crate::worker::run_slice_inline`).
pub(crate) async fn load_slice(vm: &Rc<Vm>, slice_bytes: Option<&[u8]>) -> Result<(), String> {
    let bytes = slice_bytes
        .ok_or_else(|| "worker code slice missing for an uncached entry".to_string())?;
    let chunk = Chunk::from_bytes(bytes)
        .map_err(|e| format!("worker code slice could not be loaded: {e:?}"))?;
    let proto = Rc::new(FnProto {
        chunk,
        arity: 0,
        has_rest: false,
        is_async: false,
        is_generator: false,
        is_worker: false,
        params: Vec::new(),
        ret: None,
    });
    let closure = Closure::new(proto);
    let mut fiber = crate::vm::fiber::Fiber::new(closure);
    match vm.run(&mut fiber).await {
        Ok(RunOutcome::Done(_)) => Ok(()),
        Ok(RunOutcome::Yielded(_)) => {
            Err("worker code slice top-level unexpectedly yielded".to_string())
        }
        Err(crate::interp::Control::Panic(e)) => Err(e.message),
        Err(_) => Err("worker code slice failed to load".to_string()),
    }
}

/// Decode the structured-clone arg payload into the isolate's `Value`s. The payload is
/// an encoded ARRAY of the positional args (the caller wraps them so one decode call
/// reconstructs the whole arg list, preserving cross-arg shared references / cycles).
pub(crate) fn decode_args(
    bytes: &[u8],
    interp: &crate::interp::Interp,
) -> Result<Vec<Value>, String> {
    let decoded = crate::worker::serialize::decode(bytes, interp).map_err(|e| e.message())?;
    match decoded {
        Value::Array(a) => Ok(a.borrow().clone()),
        other => Err(format!(
            "worker args payload did not decode to an array (got {})",
            crate::interp::type_name(&other)
        )),
    }
}
