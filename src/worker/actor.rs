//! `worker class` actor runtime (Spec B, Task 5).
//!
//! An actor is a class instance born in its OWN dedicated isolate (one per actor, via
//! [`crate::worker::isolate::spawn_isolate`]), holding state across calls, talked to
//! over time through a FIFO mailbox: method calls become serialized [`ActorMsg`]s sent
//! over a `Send` channel; the isolate processes each to completion before the next
//! (the GenServer one-at-a-time guarantee — single consumer = serialized = no locks).
//! Resources opened in `init` stay in the isolate and never cross — only `Send` bytes
//! do.
//!
//! ## Two halves
//!
//! - **Isolate side** ([`actor_loop`]): runs on the dedicated isolate thread. Loads
//!   the class code slice once, constructs the instance via `init` IN the isolate on
//!   `Init`, then services `Call` messages against that instance, replying with the
//!   encoded result / panic over a `oneshot`.
//! - **Caller side** ([`WorkerActorHandle`] + the host hooks in `src/interp.rs` /
//!   `src/vm/run.rs`): the `Value::Native(WorkerActor)` proxy. `ClassName.spawn(args)`
//!   builds the slice, spawns the isolate, sends `Init`, and registers the handle;
//!   `await handle.method(args)` sends a `Call` and awaits the reply.
//!
//! `!Send` integrity: the isolate builds its OWN `Interp`/`Vm`; only `Send` bytes (the
//! encoded args / results) and `Send` channels cross. The proxy stays on the caller
//! thread. Take-out-across-await: the host clones the `Send` sender out of the
//! `resources` table BEFORE awaiting, never holding a `RefCell` borrow across `.await`.

use crate::value::Value;
use std::rc::Rc;
use tokio::sync::{mpsc, oneshot};

pub(crate) use crate::worker::isolate::{spawn_isolate, IsolateHandle};

/// The reply an actor sends back for one `Call` (over a `oneshot`). `Send` bytes only.
pub enum ActorReply {
    /// The structured-clone-encoded result `Value`.
    Ok(Vec<u8>),
    /// An uncaught Tier-2 panic message raised inside the method (or a sendability
    /// failure encoding the result). Re-raised as a recoverable panic on the caller.
    Panic(String),
}

/// One message in an actor's FIFO mailbox. Every field is `Send`.
pub enum ActorMsg {
    /// Construct the instance: decode `args`, look up the class global, run `init`
    /// IN the isolate, store the instance. `reply` acks success (`Ok`) or the
    /// construction panic (`Panic`).
    Init {
        class_name: String,
        /// The class code slice `.aso` bytes (superclass chain + method table +
        /// field defaults). Loaded once to define the class global on the isolate.
        slice_bytes: Vec<u8>,
        /// Structured-clone-encoded ARRAY of the positional `init` args.
        args: Vec<u8>,
        reply: oneshot::Sender<ActorReply>,
    },
    /// Invoke `method` on the in-isolate instance with the decoded `args`.
    Call {
        method: String,
        /// Structured-clone-encoded ARRAY of the positional method args.
        args: Vec<u8>,
        reply: oneshot::Sender<ActorReply>,
    },
}

/// The caller-side proxy payload behind a `Value::Native(WorkerActor)`. Stored in
/// `Interp.resources` as `ResourceState::WorkerActor(Box<WorkerActorHandle>)`.
///
/// **Teardown:** dropping this handle drops the [`IsolateHandle`], whose `Drop` closes
/// the inbound channel (ending the mailbox loop) and joins the isolate thread — no
/// zombie. `close()` (a host method) takes the resource out of the table, dropping it
/// here; the resources table is also dropped when the `Interp` drops (process exit),
/// so an un-`close`d actor is reclaimed then.
///
/// **GC invariant:** this holds `Send` channels + a thread handle, NOT script
/// `Value`s, so the GC must NEVER trace into it. `Value::Native` already traces as a
/// no-op (`gc.rs`'s `_ => {}` arm), so the invariant holds for free.
pub struct WorkerActorHandle {
    /// Outbound `Send` sender of mailbox messages into the dedicated isolate. Cloned
    /// OUT across `.await` by the host method dispatch (take-out-across-await).
    pub tx: mpsc::UnboundedSender<ActorMsg>,
    /// The dedicated isolate. `Drop` = teardown (close channel + join thread).
    pub isolate: IsolateHandle,
    /// The declared class name (a readable proxy field; future `instanceof`).
    pub class_name: Rc<str>,
}

impl WorkerActorHandle {
    pub fn new(
        tx: mpsc::UnboundedSender<ActorMsg>,
        isolate: IsolateHandle,
        class_name: Rc<str>,
    ) -> Self {
        WorkerActorHandle {
            tx,
            isolate,
            class_name,
        }
    }
}

/// Spawn a dedicated isolate running the actor mailbox loop, returning its inbound
/// `Send` message sender + the [`IsolateHandle`]. The loop owns the isolate's
/// `Interp`/`Vm` and the actor instance for the isolate's lifetime.
///
/// FALLIBLE (thread-limit / memory pressure → `Err`, mapped by the host to a
/// recoverable panic).
pub fn spawn_actor_isolate() -> std::io::Result<(mpsc::UnboundedSender<ActorMsg>, IsolateHandle)> {
    // A SECOND `Send` channel carries the typed `ActorMsg`s. The dedicated
    // `IsolateHandle`'s own byte channel (`Vec<u8>`) is unused for actors (the actor
    // protocol is typed, not raw bytes) — we just need the handle for teardown. So we
    // bridge: the isolate run-loop ignores its byte receiver and instead consumes the
    // `ActorMsg` receiver we move into it.
    let (msg_tx, msg_rx) = mpsc::unbounded_channel::<ActorMsg>();
    // `spawn_isolate`'s closure must be `Send + 'static` and capture only `Send`
    // values; `msg_rx` is `Send`. The byte receiver is unused.
    let isolate = spawn_isolate(move |vm, _byte_rx| actor_loop(vm, msg_rx))?;
    Ok((msg_tx, isolate))
}

/// The actor's FIFO mailbox loop, on the dedicated isolate thread. Processes each
/// message to completion before the next (one-at-a-time). Ends when the inbound
/// channel closes (the `WorkerActorHandle` dropped → `close()`/last-drop/teardown).
async fn actor_loop(vm: Rc<crate::vm::Vm>, mut rx: mpsc::UnboundedReceiver<ActorMsg>) {
    let interp = vm.interp().clone();
    // The actor's instance, built on `Init` and reused for every `Call`. Method
    // dispatch resolves through the instance's class (the VM side table), so no
    // separate class handle is kept.
    let mut instance: Option<Value> = None;

    while let Some(msg) = rx.recv().await {
        match msg {
            ActorMsg::Init {
                class_name,
                slice_bytes,
                args,
                reply,
            } => {
                let result = init_instance(&vm, &interp, &class_name, &slice_bytes, &args).await;
                match result {
                    Ok(inst) => {
                        instance = Some(inst);
                        // Ack with an encoded nil (the host ignores the value, only
                        // distinguishing Ok vs Panic).
                        let _ = reply.send(ack_ok());
                    }
                    Err(msg) => {
                        let _ = reply.send(ActorReply::Panic(msg));
                    }
                }
            }
            ActorMsg::Call {
                method,
                args,
                reply,
            } => {
                let Some(inst) = instance.clone() else {
                    let _ = reply.send(ActorReply::Panic(
                        "actor received a method call before it was initialized".to_string(),
                    ));
                    continue;
                };
                let r = call_method(&vm, &interp, &inst, &method, &args).await;
                let _ = reply.send(r);
            }
        }
    }
}

/// An `Ok` ack carrying an encoded nil.
fn ack_ok() -> ActorReply {
    match crate::worker::serialize::encode(&Value::Nil) {
        Ok(b) => ActorReply::Ok(b),
        Err(e) => ActorReply::Panic(e.message()),
    }
}

/// Load the class slice (defining the class global + deps) and construct the instance
/// by calling the class value with the decoded args (runs `init` IN the isolate).
async fn init_instance(
    vm: &Rc<crate::vm::Vm>,
    interp: &crate::interp::Interp,
    class_name: &str,
    slice_bytes: &[u8],
    args: &[u8],
) -> Result<Value, String> {
    // Define the class (and its deps) on this isolate's Vm.
    crate::worker::isolate::load_slice(vm, Some(slice_bytes)).await?;
    let class = vm
        .user_global(class_name)
        .ok_or_else(|| format!("worker class '{class_name}' was not defined by its code slice"))?;
    // Decode the init args against THIS isolate's interp.
    let arg_values = crate::worker::isolate::decode_args(args, interp)?;
    // Constructing the class value runs `init` synchronously (a class call returns an
    // instance, not a future). Any construction panic → its message.
    match vm.call_value(class, arg_values, crate::span::Span::new(0, 0)).await {
        Ok(v) => Ok(v),
        Err(crate::interp::Control::Panic(e)) => Err(e.message),
        Err(crate::interp::Control::Propagate(_)) => {
            Err("worker class init propagated an error".to_string())
        }
        Err(crate::interp::Control::Exit(_)) => {
            Err("exit() is not allowed inside a worker class init".to_string())
        }
    }
}

/// Run `method` on the actor's instance with the decoded args; encode the result (or
/// the panic message) for the reply. The method body may `await` its own I/O — the
/// mailbox stays parked on this one call until it completes (one-at-a-time).
async fn call_method(
    vm: &Rc<crate::vm::Vm>,
    interp: &crate::interp::Interp,
    instance: &Value,
    method: &str,
    args: &[u8],
) -> ActorReply {
    let arg_values = match crate::worker::isolate::decode_args(args, interp) {
        Ok(vs) => vs,
        Err(msg) => return ActorReply::Panic(msg),
    };
    // Resolve + run the method via the VM's per-class method side table (a VM-built
    // class keeps its methods there, not in `Class.methods`). `call_method_named`
    // drives an `async` method's `Value::Future` to its value.
    let value = vm
        .call_method_named(
            instance.clone(),
            method,
            arg_values,
            crate::span::Span::new(0, 0),
        )
        .await;
    match value {
        Ok(v) => match crate::worker::serialize::encode(&v) {
            Ok(bytes) => ActorReply::Ok(bytes),
            // A non-sendable return (e.g. a raw native resource) is a sendability
            // panic with a field path ("resource lives in the actor").
            Err(e) => ActorReply::Panic(e.message()),
        },
        Err(crate::interp::Control::Panic(e)) => ActorReply::Panic(e.message),
        Err(crate::interp::Control::Propagate(_)) => {
            // A top-level `?` propagation inside the method ends with nil.
            match crate::worker::serialize::encode(&Value::Nil) {
                Ok(bytes) => ActorReply::Ok(bytes),
                Err(e) => ActorReply::Panic(e.message()),
            }
        }
        Err(crate::interp::Control::Exit(_)) => {
            ActorReply::Panic("exit() is not allowed inside a worker class method".to_string())
        }
    }
}
