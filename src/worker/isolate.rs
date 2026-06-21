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

use crate::value::{Value, ValueKind};
use crate::vm::chunk::{Chunk, FnProto};
use crate::vm::value_ext::{Closure, RunOutcome};
use crate::vm::Vm;
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use tokio::sync::{mpsc, oneshot};

// ── PAR §3.3.2 ──────────────────────────────────────────────────────────────

/// PAR (spec §3.3.2): which kind of per-chunk data-parallel job to run.
/// Plain `Copy`/`Send` scalars — no heap allocation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChunkKind {
    /// Map: apply the entry fn to each element in `start..end`; collect results in order.
    Map,
    /// Reduce: fold the entry fn (binary) over `start..end`, seeded by the FIRST element
    /// (`element[start]`). `init` never enters a chunk — that is the final-combine stage
    /// (spec §3.3.3).
    Reduce,
}

/// PAR (spec §3.3.2): a per-chunk data-parallel job. Plain `Copy`/`Send` scalars
/// riding beside `caps` on [`WorkerRequest`] — NOT part of the structured-clone byte
/// stream (no new wire tag; pinned by `tests/par_negative_space.rs`).
#[derive(Clone, Copy, Debug)]
pub struct ChunkJob {
    pub kind: ChunkKind,
    /// Inclusive start index into the data array (chunk's first element).
    pub start: u32,
    /// Exclusive end index into the data array (one past the chunk's last element).
    pub end: u32,
}

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

/// Run `f` (producing a future) with the `IN_ISOLATE` flag forced to `value` for the
/// duration of the future, restoring the prior value afterward (even if the future is
/// dropped mid-flight). The flag is thread-local and the runtime is single-threaded, so
/// set-run-restore is sound.
///
/// Used to override the per-thread isolate flag for a scoped sub-run:
///   - SRV forces it TRUE (a `setup` `worker fn` must run INLINE on the current VM).
///   - DX D2's test-file isolate forces it FALSE so the file it hosts behaves like a
///     normal top-level program — its own `worker fn`s dispatch to a (per-isolate-thread)
///     pool with the full code-slice path, instead of the inline-nesting path that
///     assumes the entry is already a VM global from an enclosing slice.
pub(crate) async fn with_isolate_flag<F, Fut, T>(value: bool, f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            IN_ISOLATE.with(|c| c.set(self.0));
        }
    }
    let prev = IN_ISOLATE.with(|c| c.replace(value));
    let _restore = Restore(prev);
    f().await
}

/// Run `f` with the `IN_ISOLATE` flag forced TRUE (SRV single-isolate `serve` fallback).
/// Thin wrapper over [`with_isolate_flag`].
#[cfg(feature = "net")]
pub(crate) async fn with_inline_dispatch<F, Fut, T>(f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    with_isolate_flag(true, f).await
}

/// One unit of work shipped to an isolate. Every field is `Send` (bytes / plain
/// scalars / channels) — no `Value`, no `Interp`, no `Rc<Chunk>` crosses.
pub struct WorkerRequest {
    /// Stable identity of the worker entry (keys the per-isolate code cache).
    pub fn_id: u64,
    /// The `.aso` code-slice bytes. `dispatch_worker` ALWAYS ships `Some` (it cannot know
    /// what a given pooled isolate has already cached); the isolate dedups by `fn_id`
    /// (`isolate_loop`'s `loaded` set) and ignores a re-send. `Option` is kept because the
    /// inline graceful-degradation fallback may pass `None` to `load_slice` once cached in
    /// process. A per-isolate caller-side code cache would let the caller stop re-shipping —
    /// a documented future airlock optimization, NOT done here.
    pub slice_bytes: Option<Vec<u8>>,
    /// SELF-CONTAINED-BUNDLES Task 1.6 — the encoded `ModuleArchive` of a BUNDLED
    /// multi-module program. `dispatch_worker` ships these bytes on EVERY pooled request
    /// (no caller-side suppression); the isolate installs the archive AT MOST ONCE, guarded
    /// by `isolate_loop`'s `archive_installed` flag, BEFORE the slice loads (the slice
    /// re-runs the program's top-level imports). The dedicated/actor/stream isolates instead
    /// capture the bytes ONCE at spawn (a single-tenant isolate, installed at boot). This
    /// per-request re-ship mirrors the existing `slice_bytes` shipping model above — a
    /// per-isolate caller-side cache would optimize BOTH (the same documented future airlock
    /// optimization), an accepted characteristic, not a defect. `None` for an ordinary
    /// unbundled program → nothing installed → today's exact path.
    pub archive_bytes: Option<Vec<u8>>,
    /// `Some(class)` for a `static worker fn` (currently advisory — the entry is a
    /// free top-level fn in the slice; kept for diagnostics + future class binding).
    #[allow(dead_code)]
    pub class_name: Option<String>,
    /// The worker entry's global name, fetched from the isolate's globals to call.
    pub entry_name: String,
    /// Structured-clone-encoded positional args (see `serialize::encode`).
    pub args: Vec<u8>,
    /// SRV §3.7(b) — the frozen `Arc<SharedNode>` side-vector that travels alongside
    /// `args`. A `Value::shared` arg is encoded as a `TAG_SHARED` index into this
    /// vector (a `Send` field; the frozen graph crosses by `Arc` pointer, NOT a
    /// structured-clone copy). Empty when no arg is a `Shared`.
    pub shared: Vec<std::sync::Arc<crate::value::SharedNode>>,
    /// FFI §4.5a — the dispatching isolate's `CapSet` (a `Send` side-channel field, NOT
    /// a `Value`, so it never touches the structured-clone serializer). The pooled
    /// isolate installs this FRESH at the top of each request (so request B is
    /// unaffected by request A — no forward leak) and clears `caps_drop_allowed` (a
    /// pooled `caps.drop` is refused — a durable drop on a reused `Interp` would leak /
    /// re-grant, §4.5a). This carries the CALLER'S FLOOR — a pooled worker never drops,
    /// so writing it grants nothing it had-and-lost (the monotone argument). Boxed to
    /// keep `WorkerRequest` compact (the `CapSet`'s `Option<FsScope>`/`Option<NetScope>`
    /// carry heap `Vec`s; `dispatch` returns the request by value in its `Err` path).
    pub caps: Box<crate::stdlib::caps::CapSet>,
    /// EMBED §6.4 — the dispatching (main) isolate's host-module FACTORIES, carried
    /// EXACTLY the way `caps` rides: a plain `Send` side-channel field (the `Arc`
    /// factories are `Send + Sync`; the name is `String` — `Send`, not the `!Send`
    /// `Rc<str>`), NOT serialized, NO wire tag. The pooled isolate installs these FRESH
    /// at the top of EACH request (so request B is unaffected by request A — the no-leak
    /// / caps-floor discipline). Empty for an ordinary program (no embed factories) →
    /// today's exact path. The factory RUNS on the isolate thread, building the `!Send`
    /// `HostModuleDef` in-isolate (its host fns never cross the airlock).
    pub host_factories: Vec<(String, std::sync::Arc<crate::interp::HostModuleFactoryCore>)>,
    /// Where the isolate sends the reply (result bytes or a panic message).
    pub reply: oneshot::Sender<WorkerReply>,
    /// Cancel signal: the caller drops the paired sender on `Value::future` drop;
    /// the isolate `select!`s on this to abort the in-flight run (cancel-on-drop).
    pub abort: oneshot::Receiver<()>,
    /// PAR (spec §3.3.2): when `Some`, the isolate runs the native chunk DRIVER over
    /// the decoded data arg instead of a single entry call. Plain `Copy`/`Send` scalars —
    /// no new wire tag, no serializer change (pinned by `tests/par_negative_space.rs`).
    pub chunk: Option<ChunkJob>,
}

/// The isolate's response. `Send` bytes / a message string only — plus the SRV
/// §3.7(b) frozen-`Arc` side-vector for a `Value::shared` result (also `Send`).
pub enum WorkerReply {
    /// The structured-clone-encoded result `Value`, plus the frozen `Arc<SharedNode>`
    /// side-vector (a worker may RETURN a `shared.freeze`d value — it crosses back by
    /// `Arc` pointer, zero copy).
    Ok(Vec<u8>, Vec<std::sync::Arc<crate::value::SharedNode>>),
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
    // WASM §5.3.7: ONE chokepoint. Every worker form (`worker fn` pools via
    // `Isolate::spawn`, `worker class` actors / `worker fn*` streams / `run_in_worker`
    // via `spawn_isolate`) reaches a thread spawn here. `wasm32-unknown-unknown` has no
    // threads, so refuse with `Unsupported` — the dispatch funnels turn it into the
    // Tier-2 `workers are not available on this platform (wasm)` (never a hang, never a
    // silent inline-degradation: the pooled funnel suppresses its inline fallback on
    // wasm so this error propagates). `make_loop` is intentionally dropped unused.
    #[cfg(target_family = "wasm")]
    {
        let _ = make_loop;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "workers are not available on this platform (wasm)",
        ))
    }
    #[cfg(not(target_family = "wasm"))]
    {
        std::thread::Builder::new()
            .name("ascript-isolate".to_string())
            .stack_size(ISOLATE_STACK_SIZE)
            .spawn(move || run_isolate_thread(make_loop))
    }
}

/// The isolate thread entry: build the runtime + `LocalSet` + a fresh `Interp`/`Vm`,
/// then drive the run-loop produced by `make_loop` on the `LocalSet`. Returns when the
/// run-loop future completes (its inbound channel closed).
///
/// If the tokio runtime cannot be built (resource pressure), the thread simply ends
/// WITHOUT panicking — the inbound channel's senders see no receiver, so every send /
/// reply fails and surfaces as a recoverable "isolate terminated" panic at the caller's
/// `await`, rather than aborting the process.
// WASM §5.3.7: never called on wasm (the `bootstrap` chokepoint refuses the thread
// spawn before it would run this), so it is dead there.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
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
    // Task 1.6: the bundled program's archive is installed once per isolate (the first
    // request that carries it), so a worker fn that calls into an imported module resolves
    // it from memory on the isolate's re-run imports.
    let mut archive_installed = false;

    while let Some(req) = rx.recv().await {
        let WorkerRequest {
            fn_id,
            slice_bytes,
            archive_bytes,
            class_name: _,
            entry_name,
            args,
            shared,
            caps,
            host_factories,
            reply,
            abort,
            chunk,
        } = req;

        // FFI §4.5a (the pooled-isolate soundness keystone). This `Interp` is REUSED
        // across many requests, so we install the caller's floor FRESH at the top of
        // each request (request B is unaffected by request A's state) and REFUSE a
        // `caps.drop` here (a durable drop on a shared `Interp` would leak forward / be
        // a re-grant). Writing the caller-supplied floor every time grants nothing the
        // pooled worker ever had-and-lost — it never drops — so the monotone invariant
        // holds. Durable, irreversible `caps.drop` is available only on the top-level
        // program isolate and a DEDICATED `run_in_worker({caps})` isolate.
        interp.set_caps(*caps);
        interp.set_caps_drop_allowed(false);

        // EMBED §6.4: install the host-module FACTORIES FRESH at the top of each request
        // (the caps-floor no-leak discipline — Isolate B sharing this pool thread does NOT
        // see Isolate A's factory-built modules). CLEAR first so a module from a prior
        // request's factory cannot leak forward, then install THIS request's set. Each
        // factory builds a fresh in-isolate `HostModuleDef`. Empty for an ordinary program.
        interp.clear_host_modules();
        for (name, factory) in &host_factories {
            let name_rc: Rc<str> = Rc::from(name.as_str());
            interp.install_host_factory(&name_rc, factory);
        }

        // Task 1.6: install the bundled program's archive BEFORE the slice loads (the slice
        // re-runs the program's top-level imports). Once per isolate; a decode failure is a
        // recoverable panic reply. `None` (unbundled) installs nothing → today's exact path.
        if let Some(bytes) = archive_bytes {
            if !archive_installed {
                match install_module_archive(&vm, &bytes) {
                    Ok(()) => archive_installed = true,
                    Err(msg) => {
                        let _ = reply.send(WorkerReply::Panic(msg));
                        continue;
                    }
                }
            }
        }

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
        let arg_values = match decode_args_with_shared(&args, &shared, &interp) {
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

        // PAR §3.3.2: branch on whether this request carries a chunk job.
        // The None arm is today's exact path (byte-for-byte).
        let span = crate::span::Span::new(0, 0);
        let run = async {
            match &chunk {
                Some(job) => {
                    // chunk driver: data is the first (and only) decoded arg
                    let data = arg_values.into_iter().next().unwrap_or(Value::nil());
                    run_chunk_job(&vm, entry, data, job, span).await
                }
                None => vm.call_value(entry, arg_values, span).await,
            }
        };
        tokio::pin!(run);
        let reply_msg = tokio::select! {
            biased;
            _ = abort => WorkerReply::Cancelled,
            result = &mut run => match result {
                Ok(v) => match crate::worker::serialize::encode(&v) {
                    Ok((bytes, shared)) => WorkerReply::Ok(bytes, shared),
                    Err(e) => WorkerReply::Panic(e.message()),
                },
                Err(crate::interp::Control::Panic(e)) => WorkerReply::Panic(e.message),
                // A top-level `?` propagation inside the worker body ends with nil.
                Err(crate::interp::Control::Propagate(_)) => {
                    match crate::worker::serialize::encode(&Value::nil()) {
                        Ok((bytes, shared)) => WorkerReply::Ok(bytes, shared),
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

// ── PAR chunk driver helpers ─────────────────────────────────────────────────

/// PAR §3.3.2: get the logical length of the data value for a chunk job.
/// Accepts `Value::Array` (borrow the vec length) and `Value::Shared` whose node is
/// `SharedNode::Array` (read the frozen array length). Any other kind is an internal
/// invariant violation — the stdlib layer is responsible for validating before dispatch.
fn chunk_data_len(
    data: &Value,
    span: crate::span::Span,
) -> Result<usize, crate::interp::Control> {
    use crate::value::SharedNode;
    match data.kind() {
        ValueKind::Array(a) => Ok(a.borrow().len()),
        ValueKind::Shared(node) => match &**node {
            SharedNode::Array(a) => Ok(a.len()),
            other => Err(crate::interp::Control::Panic(crate::error::AsError::at(
                format!(
                    "chunk driver: data is a frozen {} but must be a frozen array (internal invariant)",
                    other.kind_name()
                ),
                span,
            ))),
        },
        _ => Err(crate::interp::Control::Panic(crate::error::AsError::at(
            format!(
                "chunk driver: data must be an array or frozen array, got {} (internal invariant)",
                crate::interp::type_name(data)
            ),
            span,
        ))),
    }
}

/// PAR §3.3.2: read element `i` from the data value for a chunk job.
///
/// - `Value::Array`: clones the element OUT of the array before returning (never holds
///   the `RefCell` borrow across an `.await` — the caller awaits the entry call after).
/// - `Value::Shared` / `SharedNode::Array`: materializes the element via the shipped
///   SRV `shared_child_to_value` reader — byte-identical to `frozen[i]` in script.
fn chunk_data_element(
    data: &Value,
    i: usize,
    span: crate::span::Span,
) -> Result<Value, crate::interp::Control> {
    use crate::value::SharedNode;
    match data.kind() {
        ValueKind::Array(a) => {
            // Clone out of the borrow immediately so we never hold the borrow across
            // an await (clippy `await_holding_refcell_ref` deny).
            let elem = a.borrow().get(i).cloned().unwrap_or(Value::nil());
            Ok(elem)
        }
        ValueKind::Shared(node) => match &**node {
            SharedNode::Array(a) => {
                Ok(a.get(i)
                    .map(crate::interp::shared_child_to_value)
                    .unwrap_or(Value::nil()))
            }
            other => Err(crate::interp::Control::Panic(crate::error::AsError::at(
                format!(
                    "chunk driver: data is a frozen {} but must be a frozen array (internal invariant)",
                    other.kind_name()
                ),
                span,
            ))),
        },
        _ => Err(crate::interp::Control::Panic(crate::error::AsError::at(
            format!(
                "chunk driver: data must be an array or frozen array, got {} (internal invariant)",
                crate::interp::type_name(data)
            ),
            span,
        ))),
    }
}

/// PAR (spec §3.3.2): the native per-chunk element loop. `data` is either the WHOLE
/// frozen input array (`Value::Shared`, frozen path — `start..end` index into it) or
/// this chunk's own plain element slice (copy path — start=0). Per-element control
/// flow mirrors the worker top-level rules in `isolate_loop` EXACTLY:
/// - `Ok(v)` → element result (a returned future is driven to completion first)
/// - `Err(Propagate(_))` → `nil` element [DEAD: `run_body` converts body-level `?` to
///   `Ok([nil, err])` before `call_value` returns — kept to mirror `isolate_loop`]
/// - `Err(Exit(_))` → the shipped "exit() is not allowed inside a worker" panic
/// - `Err(Panic(e))` / other → the chunk fails with that error
///
/// Map replies an ordered results array; Reduce replies the partial fold seeded with
/// the chunk's FIRST element (`init` never enters a chunk — spec §3.3.3).
pub(crate) async fn run_chunk_job(
    vm: &Rc<Vm>,
    entry: Value,
    data: Value,
    job: &ChunkJob,
    span: crate::span::Span,
) -> Result<Value, crate::interp::Control> {
    use crate::interp::Control;

    let len = chunk_data_len(&data, span)?;
    let start = job.start as usize;
    let end = (job.end as usize).min(len);

    /// Apply the worker top-level control-flow rules to one element call.
    /// Mirrors the `isolate_loop` result match EXACTLY.
    async fn call_element(
        vm: &Rc<Vm>,
        entry: &Value,
        args: Vec<Value>,
        span: crate::span::Span,
    ) -> Result<Value, Control> {
        match vm.call_value(entry.clone(), args, span).await {
            Ok(v) => {
                // A future return value must be driven to completion first (the task.spawn rule).
                if let ValueKind::Future(f) = v.kind() {
                    f.get().await
                } else {
                    Ok(v)
                }
            }
            // DEAD arm: `run_body` converts a body-level `?` to `Ok([nil, err])` before
            // `call_value` returns, so this arm is never reached for a `worker fn`. Kept
            // to mirror `isolate_loop` byte-for-byte (spec §3.3.2 Phase-0 correction).
            Err(Control::Propagate(_)) => Ok(Value::nil()),
            // Exit refusal — the shipped diagnostic.
            Err(Control::Exit(_)) => Err(Control::Panic(crate::error::AsError::at(
                "exit() is not allowed inside a worker".to_string(),
                span,
            ))),
            // Any other error (Panic, etc.) propagates unchanged → chunk fails.
            Err(other) => Err(other),
        }
    }

    match job.kind {
        ChunkKind::Map => {
            let mut out = Vec::with_capacity(end.saturating_sub(start));
            for i in start..end {
                // Clone the element OUT before the await (never hold the borrow).
                let elem = chunk_data_element(&data, i, span)?;
                out.push(call_element(vm, &entry, vec![elem], span).await?);
            }
            Ok(Value::array_cell(crate::value::ArrayCell::new(out)))
        }
        ChunkKind::Reduce => {
            if start >= end {
                // Empty range for Reduce is an internal invariant violation: the planner
                // (Phase 2) must never emit an empty Reduce chunk. Return a recoverable
                // panic rather than unwrapping.
                return Err(Control::Panic(crate::error::AsError::at(
                    "chunk driver: Reduce job has empty range (start >= end) — internal invariant"
                        .to_string(),
                    span,
                )));
            }
            // Seed with the chunk's first element; init never enters a chunk (spec §3.3.3).
            let mut acc = chunk_data_element(&data, start, span)?;
            for i in start + 1..end {
                let elem = chunk_data_element(&data, i, span)?;
                acc = call_element(vm, &entry, vec![acc, elem], span).await?;
            }
            Ok(acc)
        }
    }
}

/// SELF-CONTAINED-BUNDLES Task 1.6: decode `bytes` into a [`crate::vm::archive::ModuleArchive`]
/// and install it on `vm` so the isolate's re-run top-level imports resolve from memory
/// (the bundled multi-module case). The SINGLE install seam — ALL five isolate sites
/// (pooled / inline-fallback / dedicated / actor / stream) call THIS, so the decode/install
/// step and its error string never drift. Returns a recoverable error message on a decode
/// failure (`.expect`-free); the caller maps it to a `WorkerReply::Panic`/`Control::Panic`.
/// `None` archive bytes never reach here (the caller skips the call) — today's exact path.
pub(crate) fn install_module_archive(vm: &Vm, bytes: &[u8]) -> Result<(), String> {
    let archive = crate::vm::archive::ModuleArchive::decode(bytes)
        .map_err(|e| format!("cannot install module archive on worker isolate: {e}"))?;
    vm.set_module_archive(Rc::new(archive));
    Ok(())
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
        local_names: Vec::new(),
        debug_name: None,
        name_span: None,
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
/// Resolves any `TAG_SHARED` index against the frozen-`Arc` side-vector (SRV §3.7b
/// — a `Value::shared` arg crosses by `Arc` clone, zero copy). Callers with no shared
/// values pass `&[]`.
pub(crate) fn decode_args_with_shared(
    bytes: &[u8],
    shared: &[std::sync::Arc<crate::value::SharedNode>],
    interp: &crate::interp::Interp,
) -> Result<Vec<Value>, String> {
    let decoded =
        crate::worker::serialize::decode_with_shared(bytes, shared, interp).map_err(|e| e.message())?;
    match decoded.kind() {
        ValueKind::Array(a) => Ok(a.borrow().clone()),
        _ => Err(format!(
            "worker args payload did not decode to an array (got {})",
            crate::interp::type_name(&decoded)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc as std_mpsc;
    use std::sync::Arc;

    // ── PAR Unit A: chunk driver tests (Step 5) ───────────────────────────────

    /// Load a code slice into a fresh Vm (the dispatch.rs run_slice_in_fresh_isolate
    /// pattern — lifted here so the chunk driver tests don't duplicate it). Returns
    /// the Vm with the slice's globals defined, ready for `run_chunk_job`.
    async fn fresh_vm_with_slice(src: &str, entry_name: &str) -> (Rc<Vm>, Value) {
        let top = crate::compile::compile_source(src).expect("compiles");
        let slice =
            crate::worker::build_code_slice(&top, entry_name, None).expect("slice builds");

        let chunk = Chunk::from_bytes(&slice.entry_aso).expect("slice .aso decodes");
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
            local_names: Vec::new(),
            debug_name: None,
            name_span: None,
        });
        let closure = Closure::new(proto);

        let interp = Rc::new(crate::interp::Interp::new());
        interp.install_self();
        let vm = Vm::new(interp);

        let mut fiber = crate::vm::fiber::Fiber::new(closure);
        match vm.run(&mut fiber).await.expect("slice runs") {
            RunOutcome::Done(_) => {}
            RunOutcome::Yielded(_) => unreachable!("fragment top-level cannot yield"),
        }
        let entry = vm
            .user_global(entry_name)
            .expect("entry global defined by the fragment");
        (vm, entry)
    }

    /// PAR Unit A: Map over a plain array in order — the primary happy-path test.
    /// `worker fn double(x) { return x * 2 }` applied to [10, 20, 30] must yield [20, 40, 60].
    #[tokio::test]
    async fn chunk_driver_map_plain_array_in_order() {
        let (vm, entry) = fresh_vm_with_slice(
            "worker fn double(x) { return x * 2 }",
            "double",
        )
        .await;

        let data = Value::array_cell(crate::value::ArrayCell::new(vec![
            Value::int(10),
            Value::int(20),
            Value::int(30),
        ]));
        let job = ChunkJob {
            kind: ChunkKind::Map,
            start: 0,
            end: 3,
        };
        let out = run_chunk_job(&vm, entry, data, &job, crate::span::Span::new(0, 0))
            .await
            .expect("map chunk succeeds");
        assert_eq!(format!("{out}"), "[20, 40, 60]");
    }

    /// PAR Unit A: Reduce seeds with the first element; `init` never enters a chunk
    /// (spec §3.3.3). `add(a, b) = a + b` over [1, 2, 3, 4] → 1+2+3+4 = 10.
    #[tokio::test]
    async fn chunk_driver_reduce_seeded_with_first_element() {
        let (vm, entry) = fresh_vm_with_slice(
            "worker fn add(a, b) { return a + b }",
            "add",
        )
        .await;

        let data = Value::array_cell(crate::value::ArrayCell::new(vec![
            Value::int(1),
            Value::int(2),
            Value::int(3),
            Value::int(4),
        ]));
        let job = ChunkJob {
            kind: ChunkKind::Reduce,
            start: 0,
            end: 4,
        };
        let out = run_chunk_job(&vm, entry, data, &job, crate::span::Span::new(0, 0))
            .await
            .expect("reduce chunk succeeds");
        // seed=1, then f(1,2)=3, f(3,3)=6, f(6,4)=10
        assert_eq!(out, Value::int(10));
    }

    /// PAR Unit A: Phase-0 correction — a `?`-propagating callback yields the [nil, err]
    /// PAIR (not nil), because `run_body` converts a body-level `?` to `Ok([nil, err])`
    /// before `call_value` returns. The Propagate arm in `call_element` is dead code.
    #[tokio::test]
    async fn chunk_driver_map_propagating_element_yields_pair() {
        // This worker fn uses `?` at top level; run_body converts it to Ok([nil, err])
        // before call_value returns — so the element result is the pair, not nil.
        let (vm, entry) = fresh_vm_with_slice(
            r#"worker fn maybe(x) {
    let [v, e] = [nil, {message: "oops"}]
    let r = [v, e]?
    return 1
}"#,
            "maybe",
        )
        .await;

        let data = Value::array_cell(crate::value::ArrayCell::new(vec![Value::int(0)]));
        let job = ChunkJob {
            kind: ChunkKind::Map,
            start: 0,
            end: 1,
        };
        let out = run_chunk_job(&vm, entry, data, &job, crate::span::Span::new(0, 0))
            .await
            .expect("map chunk returns ok (pair is a value)");
        // The result is a 1-element array whose element is [nil, err_obj]
        match out.kind() {
            ValueKind::Array(a) => {
                let elems = a.borrow().clone();
                assert_eq!(elems.len(), 1, "one element in the map result");
                // The element itself must be an array (the [nil, err] pair)
                match elems[0].kind() {
                    ValueKind::Array(pair) => {
                        let pair = pair.borrow().clone();
                        assert_eq!(pair.len(), 2, "pair has two elements");
                        assert_eq!(pair[0], Value::nil(), "first element of pair is nil");
                    }
                    _ => panic!("element must be a [nil, err] array pair, got {}", elems[0]),
                }
            }
            _ => panic!("expected Array output, got {}", out),
        }
    }

    /// PAR Unit A: a panicking element aborts the rest of the chunk.
    /// Uses division by zero to trigger a deterministic Tier-2 panic inside the worker fn.
    #[tokio::test]
    async fn chunk_driver_map_panic_aborts_chunk() {
        // Dividing by zero triggers a recoverable Tier-2 panic inside the worker.
        let (vm, entry) = fresh_vm_with_slice(
            r#"worker fn boom(x) { return 1 / x }"#,
            "boom",
        )
        .await;

        let data = Value::array_cell(crate::value::ArrayCell::new(vec![
            Value::int(0), // triggers divide-by-zero panic
            Value::int(2),
        ]));
        let job = ChunkJob {
            kind: ChunkKind::Map,
            start: 0,
            end: 2,
        };
        let err = run_chunk_job(&vm, entry, data, &job, crate::span::Span::new(0, 0))
            .await
            .unwrap_err();
        match err {
            crate::interp::Control::Panic(e) => {
                assert!(
                    e.message.contains("zero") || e.message.contains("division"),
                    "chunk error must propagate the element's panic message, got: {e:?}"
                );
            }
            other => panic!("expected Panic control, got {other:?}"),
        }
    }

    /// PAR Unit A: frozen `Shared` array data path — element materialization must be
    /// byte-identical to `frozen[i]` in script (via `shared_child_to_value`).
    #[tokio::test]
    async fn chunk_driver_map_frozen_shared_array() {
        let (vm, entry) = fresh_vm_with_slice(
            "worker fn triple(x) { return x * 3 }",
            "triple",
        )
        .await;

        // Build a frozen SharedNode::Array directly
        let shared_arr = std::sync::Arc::new(crate::value::SharedNode::Array(
            std::sync::Arc::from(vec![
                std::sync::Arc::new(crate::value::SharedNode::Int(5)),
                std::sync::Arc::new(crate::value::SharedNode::Int(10)),
                std::sync::Arc::new(crate::value::SharedNode::Int(15)),
            ]),
        ));
        let data = Value::shared(shared_arr);

        let job = ChunkJob {
            kind: ChunkKind::Map,
            start: 0,
            end: 3,
        };
        let out = run_chunk_job(&vm, entry, data, &job, crate::span::Span::new(0, 0))
            .await
            .expect("frozen shared map chunk succeeds");
        assert_eq!(format!("{out}"), "[15, 30, 45]");
    }

    /// PAR Unit A: `end > len` clamps to the actual array length — no out-of-bounds.
    #[tokio::test]
    async fn chunk_driver_map_end_clamps_to_len() {
        let (vm, entry) = fresh_vm_with_slice(
            "worker fn id(x) { return x }",
            "id",
        )
        .await;

        let data = Value::array_cell(crate::value::ArrayCell::new(vec![
            Value::int(1),
            Value::int(2),
        ]));
        // end=99 but len=2 — must clamp to 2 without panicking.
        let job = ChunkJob {
            kind: ChunkKind::Map,
            start: 0,
            end: 99,
        };
        let out = run_chunk_job(&vm, entry, data, &job, crate::span::Span::new(0, 0))
            .await
            .expect("clamped end succeeds");
        assert_eq!(format!("{out}"), "[1, 2]");
    }

    /// PAR Unit A: empty range (start == end) → Map returns `[]`.
    #[tokio::test]
    async fn chunk_driver_map_empty_range_returns_empty_array() {
        let (vm, entry) = fresh_vm_with_slice(
            "worker fn id(x) { return x }",
            "id",
        )
        .await;

        let data = Value::array_cell(crate::value::ArrayCell::new(vec![
            Value::int(1),
            Value::int(2),
        ]));
        let job = ChunkJob {
            kind: ChunkKind::Map,
            start: 1,
            end: 1,
        };
        let out = run_chunk_job(&vm, entry, data, &job, crate::span::Span::new(0, 0))
            .await
            .expect("empty range map succeeds");
        assert_eq!(format!("{out}"), "[]");
    }

    /// PAR Unit A: Reduce with a single element (start+1 == end) — returns just that element
    /// without calling the combiner (spec §3.3.3 — seed is the first element, loop body empty).
    #[tokio::test]
    async fn chunk_driver_reduce_single_element_returns_seed() {
        let (vm, entry) = fresh_vm_with_slice(
            "worker fn add(a, b) { return a + b }",
            "add",
        )
        .await;

        let data = Value::array_cell(crate::value::ArrayCell::new(vec![Value::int(42)]));
        let job = ChunkJob {
            kind: ChunkKind::Reduce,
            start: 0,
            end: 1,
        };
        let out = run_chunk_job(&vm, entry, data, &job, crate::span::Span::new(0, 0))
            .await
            .expect("single-element reduce succeeds");
        assert_eq!(out, Value::int(42));
    }

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
                if let Ok(v) = crate::worker::serialize::decode(&msg, &interp) {
                    if let ValueKind::Float(n) = v.kind() {
                        let _ = result_tx.send(n * 2.0);
                    }
                }
            }
            // Inbound channel closed (handle dropped) → body returns → thread can exit.
            loop_ended_in.store(true, Ordering::SeqCst);
        })
        .expect("dedicated isolate should spawn");

        // Ship a trivial value as bytes; expect the doubled result back.
        let (payload, _shared) = crate::worker::serialize::encode(&Value::float(21.0))
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

    /// FFI §4.5a keystone: a DEDICATED isolate installs a REDUCED `CapSet` (captured
    /// directly in the `Send` `make_loop` closure — never riding the value serializer)
    /// into its brand-new `Interp` BEFORE running the job, and `caps.drop` stays
    /// DURABLE there (single-tenant → terminal). We spawn an isolate, install a CapSet
    /// denying `ffi`, then in-isolate verify `ffi` is denied AND a further drop is
    /// allowed (and holds) — reporting the booleans back over a `Send` channel.
    #[test]
    fn dedicated_isolate_installs_reduced_caps_and_allows_durable_drop() {
        use crate::stdlib::caps::{Cap, CapSet};
        let (tx, rx) = std_mpsc::channel::<(bool, bool, bool)>();

        // The reduced CapSet (deny ffi) — a plain `Send` value captured in the closure.
        let mut reduced = CapSet::all_granted();
        reduced.deny(Cap::Ffi);

        let handle = spawn_isolate(move |vm, mut inbound| async move {
            let interp = vm.interp().clone();
            // Install the reduced caps BEFORE any job (the dedicated-spawn keystone).
            interp.set_caps(reduced);
            // ffi must be denied; net must still be granted; a drop must be ALLOWED
            // (dedicated isolates keep `caps_drop_allowed = true`).
            let ffi_granted = interp.caps().has(Cap::Ffi);
            let net_granted = interp.caps().has(Cap::Net);
            let drop_allowed = interp.caps_drop_allowed();
            // Prove the drop is durable: drop net, confirm it stays denied.
            interp.caps_deny(Cap::Net);
            let net_after_drop = interp.caps().has(Cap::Net);
            let _ = tx.send((ffi_granted, net_granted && !net_after_drop, drop_allowed));
            // Keep the loop alive until the handle drops.
            while inbound.recv().await.is_some() {}
        })
        .expect("dedicated isolate should spawn");

        let (ffi_granted, net_drop_durable, drop_allowed) = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("isolate should report within 5s");
        assert!(!ffi_granted, "the reduced CapSet must deny ffi in the dedicated isolate");
        assert!(net_drop_durable, "a net drop in a dedicated isolate must be durable");
        assert!(drop_allowed, "a dedicated isolate keeps caps.drop allowed (durable)");
        drop(handle);
    }

    /// FFI §4.5a soundness: a POOLED request installs the caller's caps FRESH and
    /// REFUSES a drop (the shared `Interp` is reused, so a durable drop would leak /
    /// re-grant). We simulate the `isolate_loop` per-request install on one reused
    /// `Interp`: request A drops nothing (refused), request B re-installs the floor and
    /// still has every cap — proving no forward leak across the reused isolate.
    #[tokio::test]
    async fn pooled_request_install_refuses_drop_and_no_leak() {
        use crate::stdlib::caps::{Cap, CapSet};
        let interp = crate::interp::Interp::new();

        // Request A: install the caller's floor (all granted) + refuse drops (the
        // `isolate_loop` per-request step). A `caps.drop` here must be refused.
        interp.set_caps(CapSet::all_granted());
        interp.set_caps_drop_allowed(false);
        assert!(!interp.caps_drop_allowed(), "pooled request refuses drops");
        let refused = interp
            .call_caps("drop", &[crate::value::Value::str("net")], crate::span::Span::new(0, 0))
            .await;
        assert!(refused.is_err(), "a pooled caps.drop must be refused");
        assert!(interp.caps().has(Cap::Net), "the refused drop must not mutate caps");

        // Request B: re-install the caller's floor FRESH — request B is unaffected by A
        // and still has every cap (no forward leak; the re-install is the caller's
        // authority, not a restoration of a dropped one).
        interp.set_caps(CapSet::all_granted());
        interp.set_caps_drop_allowed(false);
        for cap in Cap::ALL {
            assert!(interp.caps().has(cap), "request B has the full caller floor (no leak)");
        }
    }
}
