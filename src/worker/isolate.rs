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

/// A live isolate: the `Send` request channel into it plus its OS thread handle.
pub struct Isolate {
    /// Sends [`WorkerRequest`]s into the isolate's run loop.
    pub tx: mpsc::UnboundedSender<WorkerRequest>,
    /// The OS thread handle for this isolate.  Kept as `Option` so it can be taken
    /// if a future teardown path wants to join it, but no explicit `Drop`/join is
    /// currently implemented: idle isolates block on `recv()` and exit naturally with
    /// the process.  The pool is a capped, reused set of threads; they are not joined
    /// explicitly — address-space and thread-count overhead is bounded by
    /// `ASCRIPT_WORKERS`.
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
    ///
    /// Built on the shared [`bootstrap`] used by the dedicated [`spawn_isolate`]: the
    /// pool's stateless request loop is just one particular run-loop body over the same
    /// thread + runtime + `LocalSet` + fresh `Interp`/`Vm` substrate.
    pub fn spawn() -> std::io::Result<Isolate> {
        let (tx, rx) = mpsc::unbounded_channel::<WorkerRequest>();
        let thread = bootstrap(move |vm| isolate_loop(vm, rx))?;
        Ok(Isolate {
            tx,
            thread: Some(thread),
        })
    }
}

/// Spawn a fresh isolate thread (own `current_thread` runtime + `LocalSet` + a fresh
/// shared-NOTHING [`Vm`]/[`Interp`] with `global_env`, on the [`ISOLATE_STACK_SIZE`]
/// stack) and run a CALLER-SUPPLIED async run-loop on it, driven by an inbound `Send`
/// byte channel. The handle is the `Send` byte sender plus the thread; dropping the
/// handle closes the channel (so the run-loop's `recv().await` returns `None` and the
/// body ends) and joins the thread — cancel-on-drop / clean teardown, no zombie thread.
///
/// This is the DEDICATED (non-pooled) isolate substrate. Unlike the pool's reused
/// stateless isolates, a dedicated isolate is spawned per actor (Task 5) / streaming
/// generator (Task 6), lives for that handle's lifetime holding its own state, and is
/// torn down on `close`/last-drop. The shared [`bootstrap`] guarantees the SAME
/// shared-nothing `!Send` integrity (the isolate builds its own runtime types; only
/// `Send` bytes cross the `mpsc`).
///
/// `make_loop` receives the freshly-built `Rc<Vm>` (its `Interp` is reachable via
/// `vm.interp()`) plus the inbound `mpsc::UnboundedReceiver<Vec<u8>>`, and returns the
/// run-loop future. Returning the receiver lets the actor/stream driver own the
/// protocol; Task 4 only provides the spawn/hold/teardown mechanism.
///
/// FALLIBLE for the same reason as [`Isolate::spawn`] (thread-limit / memory pressure
/// → `Err`); callers map that to a recoverable panic rather than aborting.
// Task 4 ships this dedicated-isolate substrate; the actor (Task 5) and streaming
// generator (Task 6) drivers are its only non-test callers, so it reads as dead code
// until those land. The inline unit test below exercises the full spawn/use/teardown.
#[allow(dead_code)]
pub(crate) fn spawn_isolate<F, Fut>(make_loop: F) -> std::io::Result<IsolateHandle>
where
    F: FnOnce(Rc<Vm>, mpsc::UnboundedReceiver<Vec<u8>>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let thread = bootstrap(move |vm| make_loop(vm, rx))?;
    Ok(IsolateHandle {
        tx,
        thread: Some(thread),
    })
}

/// A live DEDICATED isolate: the inbound `Send` byte channel into its run-loop plus the
/// OS thread handle. Held for the lifetime of the actor / streaming generator it backs.
///
/// **Teardown (cancel-on-drop):** `Drop` drops the `tx` sender — the run-loop's
/// `rx.recv().await` then resolves to `None`, ending the body — and joins the thread so
/// no zombie thread leaks. (Plan A's pooled `Isolate` deliberately does NOT join idle
/// isolates because they are reused for the pool's lifetime; a dedicated isolate is the
/// opposite — bound to one handle, reclaimed when it drops.) This extends Plan A's
/// cancel-on-drop to the held-for-lifetime case.
pub struct IsolateHandle {
    /// Sends serialized (`Send`) protocol messages into the dedicated isolate's
    /// run-loop. The actor wraps this with a mailbox; the stream wraps it with a
    /// demand driver (Tasks 5/6 define the on-the-wire message framing).
    pub tx: mpsc::UnboundedSender<Vec<u8>>,
    /// The OS thread handle, taken + joined on `Drop`.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for IsolateHandle {
    fn drop(&mut self) {
        // Replace the sender with a fresh, immediately-dropped one so the live `tx` is
        // dropped HERE: that closes the channel, so the isolate's run-loop sees
        // `recv().await == None` and returns, letting `block_on` finish and the thread
        // exit. We then join to reclaim it (no zombie thread).
        let (dead_tx, _dead_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        self.tx = dead_tx; // drops the real sender → closes the inbound channel
        if let Some(thread) = self.thread.take() {
            // Join cannot deadlock: the channel is closed above, so the run-loop's
            // `recv().await` resolves to `None` and the body returns. A panicked
            // isolate thread surfaces here as a join `Err`, which we ignore (the
            // caller-side reply channels already mapped any failure to a recoverable
            // panic) rather than re-panicking on the dropping thread.
            let _ = thread.join();
        }
    }
}

/// Shared isolate THREAD BOOTSTRAP: spawn an OS thread with the [`ISOLATE_STACK_SIZE`]
/// stack that builds a `current_thread` tokio runtime + `LocalSet` + a fresh
/// shared-NOTHING `Interp`/`Vm` (with `global_env` via `install_self`/`Vm::new`), then
/// `block_on`s the run-loop produced by `make_loop(vm)`. Used by BOTH the pooled
/// [`Isolate::spawn`] (its loop is [`isolate_loop`]) and the dedicated
/// [`spawn_isolate`]. The `!Send` runtime types are constructed INSIDE the thread; only
/// the `Send` `make_loop` closure crosses.
fn bootstrap<F, Fut>(make_loop: F) -> std::io::Result<std::thread::JoinHandle<()>>
where
    F: FnOnce(Rc<Vm>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    std::thread::Builder::new()
        .name("ascript-isolate".to_string())
        .stack_size(ISOLATE_STACK_SIZE)
        .spawn(move || run_isolate_thread(make_loop))
}

/// The isolate thread entry: build the runtime + `LocalSet` + a fresh `Interp`/`Vm`,
/// then drive the run-loop produced by `make_loop` on the `LocalSet`. Returns when the
/// run-loop future completes (its inbound channel closed).
///
/// If the tokio runtime cannot be built (resource pressure), the thread simply ends
/// WITHOUT panicking — the inbound channel's senders see no receiver, so every send /
/// reply fails and surfaces as a recoverable "isolate terminated" panic at the caller's
/// `await`, rather than aborting the process.
fn run_isolate_thread<F, Fut>(make_loop: F)
where
    F: FnOnce(Rc<Vm>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    IN_ISOLATE.with(|c| c.set(true));
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return, // graceful: no panic; the channel closing signals callers
    };
    let local = tokio::task::LocalSet::new();
    let interp = Rc::new(crate::interp::Interp::new());
    interp.install_self();
    let vm = Vm::new(interp);
    local.block_on(&rt, make_loop(vm));
}

/// The per-isolate request loop. A fresh `Interp`/`Vm` is created ONCE and reused for
/// every request on this isolate (shared-nothing across isolates, but stateful within
/// one — Spec A workers are stateless, so reuse is observably a fresh-globals run per
/// distinct slice). `loaded` tracks which slices' globals are already defined.
async fn isolate_loop(vm: Rc<Vm>, mut rx: mpsc::UnboundedReceiver<WorkerRequest>) {
    let interp = vm.interp().clone();
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
        owning_class: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc as std_mpsc;
    use std::sync::Arc;

    /// The dedicated (non-pooled) isolate substrate: `spawn_isolate` births a thread
    /// with its own runtime + fresh `Interp`/`Vm`, runs a caller-supplied run-loop that
    /// receives `Send` bytes over time, decodes each against ITS OWN `Interp` (proving
    /// the shared-nothing bootstrap), replies with `Send` bytes over a `std::mpsc`
    /// back-channel, and — when the `IsolateHandle` is dropped — its inbound channel
    /// closes, the loop ends, and the thread is joined cleanly (no zombie thread).
    #[test]
    fn spawn_isolate_runs_loop_and_drops_cleanly() {
        // `Send` back-channel for results + a flag the run-loop sets when its body ends.
        let (result_tx, result_rx) = std_mpsc::channel::<f64>();
        let loop_ended = Arc::new(AtomicBool::new(false));
        let loop_ended_in = loop_ended.clone();

        let handle = spawn_isolate(move |vm, mut rx| async move {
            // The fresh, isolate-owned interp (built by `bootstrap`, shared with no one).
            let interp = vm.interp().clone();
            while let Some(msg) = rx.recv().await {
                // Decode the shipped bytes against THIS isolate's own interp, double the
                // number, and report it back over the `Send` back-channel.
                if let Ok(Value::Number(n)) =
                    crate::worker::serialize::decode(&msg, &interp)
                {
                    let _ = result_tx.send(n * 2.0);
                }
            }
            // Inbound channel closed (handle dropped) → body returns → thread can exit.
            loop_ended_in.store(true, Ordering::SeqCst);
        })
        .expect("dedicated isolate should spawn");

        // Ship a trivial value as bytes; expect the doubled result back.
        let payload = crate::worker::serialize::encode(&Value::Number(21.0))
            .expect("encode sendable number");
        handle.tx.send(payload).expect("isolate inbound channel open");
        let got = result_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("isolate should reply within 5s");
        assert_eq!(got, 42.0, "isolate ran the shipped work on its own Vm");

        // Sanity: the loop has NOT ended while the handle is still alive.
        assert!(
            !loop_ended.load(Ordering::SeqCst),
            "run-loop must stay alive while the handle is held"
        );

        // Dropping the handle closes the inbound channel and JOINS the thread (clean
        // teardown). After `drop` returns, the joined loop must have ended — proving no
        // zombie thread is left behind.
        drop(handle);
        assert!(
            loop_ended.load(Ordering::SeqCst),
            "dropping the handle must end the run-loop and join the thread (no zombie)"
        );
    }
}
