//! `worker fn*` streaming-generator runtime (Spec B, Task 6) — the dedicated-isolate
//! driver behind `GenImpl::Worker`.
//!
//! A streaming generator runs its producer body in its OWN dedicated isolate (via
//! [`crate::worker::isolate::spawn_isolate`]); the caller side is a demand-driven
//! driver: each `resume`/`.next(v)` sends one demand credit over the [`IsolateHandle`]'s
//! inbound `Send` channel and awaits the next serialized yield (or done/error). A
//! bounded prefetch buffer (default 1 = strict pull) gives backpressure.
//!
//! ## The demand-driven pull protocol (GenStage / gRPC pull model)
//!
//! The producer isolate, on [`StreamMsg::Init`], loads the `worker fn*` code slice,
//! calls the entry (which returns a LOCAL `Value::Generator` on the isolate's own Vm),
//! and stores it. Then it services [`StreamMsg::Resume`] credits one at a time:
//!
//!   - One consumer `.next(v)` == one [`StreamMsg::Resume`] credit carrying `encode(v)`.
//!   - The isolate `decode`s `v`, calls `gen.resume(v)` on its LOCAL generator (the
//!     `worker fn*` body runs exactly ONE step to its next `yield`), and replies with
//!     the encoded yielded value / `Done` / a panic message.
//!   - Because the producer only advances per credit, it runs at most ONE step ahead of
//!     demand — strict pull = backpressure. (A larger prefetch window is a future knob;
//!     `prefetch = 1` is the default and the only currently-implemented depth.)
//!
//! `v` is injected back as the result of the producer's `yield` expression — bidirectional
//! `next(v)`, exactly the local-generator semantics, just across the isolate boundary.
//!
//! ## `!Send` integrity
//!
//! The producer isolate builds its OWN `Interp`/`Vm` and runs its OWN generator; the
//! caller-side [`StreamDriver`] holds only `Send` channels + the [`IsolateHandle`]. Only
//! `Send` bytes (encoded yields / injected values) cross. No `unsafe impl Send`.
//!
//! ## Teardown
//!
//! Dropping the [`StreamDriver`] drops the [`IsolateHandle`] (channel close + thread
//! join) — `gen.close()` / dropping the `Value::Generator` reclaims the isolate, no
//! zombie thread (mirrors the local generator's clean-drop guarantee).

use crate::value::{Value, ValueKind};
use std::rc::Rc;
use tokio::sync::{mpsc, oneshot};

pub(crate) use crate::worker::isolate::{spawn_isolate, IsolateHandle};

/// The default prefetch window: 1 = strict pull (the producer runs exactly one step
/// ahead of demand). Backpressure is automatic — the producer does nothing between
/// demand credits.
pub const DEFAULT_PREFETCH: usize = 1;

/// One demand credit / control message in a streaming generator's pull protocol.
/// Every field is `Send` (bytes + a `Send` reply channel).
pub enum StreamMsg {
    /// Construct the producer: decode `args`, load the slice, look up the `worker fn*`
    /// entry, CALL it (yielding a local `Value::Generator` on the isolate Vm), and store
    /// it. `reply` acks success (`Ack`) or a build/call panic (`Panic`).
    Init {
        entry_name: String,
        /// The `worker fn*` code slice `.aso` bytes (entry + its transitive top-level
        /// dependency closure). Loaded once to define the entry on the isolate.
        slice_bytes: Vec<u8>,
        /// Structured-clone-encoded ARRAY of the positional generator-call args.
        args: Vec<u8>,
        /// SRV §3.7(b) frozen-`Arc` side-vector for `Value::Shared` args.
        shared: Vec<std::sync::Arc<crate::value::SharedNode>>,
        reply: oneshot::Sender<StreamReply>,
    },
    /// One demand credit. `input` is the structured-clone-encoded value `.next(v)`
    /// passed in (the result of the producer's suspended `yield` expression); on the
    /// FIRST resume the producer ignores it (local-generator first-`next` semantics).
    Resume {
        input: Vec<u8>,
        /// SRV §3.7(b) frozen-`Arc` side-vector for a `Value::Shared` injected value.
        shared: Vec<std::sync::Arc<crate::value::SharedNode>>,
        reply: oneshot::Sender<StreamReply>,
    },
}

/// The producer's reply to one [`StreamMsg`]. `Send` bytes / a message string only.
pub enum StreamReply {
    /// `Init` succeeded (the producer generator is built and ready).
    Ack,
    /// A `yield`ed value, structured-clone-encoded + the SRV §3.7(b) frozen-`Arc`
    /// side-vector (a producer may `yield` a `shared.freeze`d value).
    Yielded(Vec<u8>, Vec<std::sync::Arc<crate::value::SharedNode>>),
    /// The generator finished (its body returned / closed). No more values.
    Done,
    /// An uncaught Tier-2 panic raised inside the producer body (or a sendability
    /// failure encoding a yielded value). Re-raised as a recoverable panic on the
    /// consumer.
    Panic(String),
}

/// The caller-side, demand-driven driver behind [`crate::coro::GenImpl::Worker`]. Holds
/// the outbound demand channel into the producer isolate + the [`IsolateHandle`] (whose
/// `Drop` tears the isolate down). `!Send` like the rest of the runtime — it lives on
/// the consumer thread and is never moved across threads (only the `Send` bytes it sends
/// cross).
///
/// **GC invariant:** this holds `Send` channels + a thread handle, NOT script `Value`s,
/// so the GC must NEVER trace into it. It sits behind `Value::Generator`, whose
/// `Value::trace` arm is a no-op (acyclic handle), so the invariant holds for free.
pub struct StreamDriver {
    /// Outbound `Send` sender of demand credits into the producer isolate.
    tx: mpsc::UnboundedSender<StreamMsg>,
    /// The dedicated isolate. `Drop` = teardown (close channel + join thread).
    #[allow(dead_code)] // held purely for its teardown-on-drop side effect.
    isolate: IsolateHandle,
    /// The prefetch depth (currently always [`DEFAULT_PREFETCH`] = 1 = strict pull).
    #[allow(dead_code)] // reserved for a future >1 prefetch window.
    prefetch: usize,
}

impl StreamDriver {
    /// Spawn a dedicated isolate running the `worker fn*` producer, ship the code slice
    /// and call args, and await the `Init` ack. Returns the ready driver, or a
    /// recoverable panic message if the isolate could not spawn / the producer build
    /// failed.
    ///
    /// `!Send`: the isolate builds its own `Interp`/`Vm`; only the `Send` slice bytes +
    /// encoded args cross.
    ///
    /// `archive_bytes` (Task 1.6): the bundled program's encoded `ModuleArchive`, `Some` for
    /// a bundled multi-module program. Captured into the `Send` `make_loop` closure and
    /// installed on the producer isolate's `Vm` before the slice loads, so a `worker fn*` that
    /// calls into an imported module resolves it from memory. `None` for an unbundled program.
    pub async fn spawn(
        entry_name: String,
        slice_bytes: Vec<u8>,
        encoded_args: Vec<u8>,
        encoded_shared: Vec<std::sync::Arc<crate::value::SharedNode>>,
        archive_bytes: Option<Vec<u8>>,
    ) -> Result<StreamDriver, String> {
        // A typed `Send` demand channel; the dedicated `IsolateHandle`'s own byte
        // channel is unused (the stream protocol is typed, not raw bytes) — we keep the
        // handle only for teardown, exactly like the actor.
        let (tx, rx) = mpsc::unbounded_channel::<StreamMsg>();
        let isolate = spawn_isolate(move |vm, _byte_rx| stream_loop(vm, rx, archive_bytes))
            .map_err(|e| format!("could not spawn streaming-generator isolate: {e}"))?;

        // Send Init and await the ack.
        let (ack_tx, ack_rx) = oneshot::channel::<StreamReply>();
        let init = StreamMsg::Init {
            entry_name,
            slice_bytes,
            args: encoded_args,
            shared: encoded_shared,
            reply: ack_tx,
        };
        if tx.send(init).is_err() {
            return Err("streaming-generator isolate terminated before initialization".to_string());
        }
        match ack_rx.await {
            Ok(StreamReply::Ack) => Ok(StreamDriver {
                tx,
                isolate,
                prefetch: DEFAULT_PREFETCH,
            }),
            Ok(StreamReply::Panic(msg)) => Err(msg),
            Ok(_) => Err("streaming-generator isolate gave an unexpected init reply".to_string()),
            Err(_) => {
                Err("streaming-generator isolate terminated during initialization".to_string())
            }
        }
    }

    /// One demand credit: send `encoded_input` (the encoded `.next(v)` value) and await
    /// the next yield. Returns `Ok(Some(bytes))` for a yielded value (caller decodes),
    /// `Ok(None)` when the producer is done, or `Err(msg)` for a producer panic /
    /// isolate teardown.
    ///
    /// AWAIT DISCIPLINE: no `RefCell` borrow is held — the `Send` sender is owned by the
    /// driver and the reply rides a `oneshot`. The caller ([`crate::coro`]) takes the
    /// driver OUT of its `RefCell<Option<..>>` before calling this (take-out-across-await).
    #[allow(clippy::type_complexity)]
    pub async fn resume(
        &self,
        encoded_input: Vec<u8>,
        encoded_shared: Vec<std::sync::Arc<crate::value::SharedNode>>,
    ) -> Result<Option<(Vec<u8>, Vec<std::sync::Arc<crate::value::SharedNode>>)>, String> {
        let (reply_tx, reply_rx) = oneshot::channel::<StreamReply>();
        let msg = StreamMsg::Resume {
            input: encoded_input,
            shared: encoded_shared,
            reply: reply_tx,
        };
        if self.tx.send(msg).is_err() {
            // The producer isolate is gone (torn down) — treat as done rather than a
            // hard error so a post-close resume is the clean done sentinel.
            return Ok(None);
        }
        match reply_rx.await {
            Ok(StreamReply::Yielded(bytes, shared)) => Ok(Some((bytes, shared))),
            Ok(StreamReply::Done) => Ok(None),
            Ok(StreamReply::Panic(msg)) => Err(msg),
            Ok(StreamReply::Ack) => {
                Err("streaming-generator isolate gave an unexpected resume reply".to_string())
            }
            // The reply sender dropped without replying → the producer was torn down.
            Err(_) => Ok(None),
        }
    }
}

/// The producer isolate's run-loop. Builds the producer generator on `Init`, then
/// services `Resume` credits one at a time (strict pull). Ends when the demand channel
/// closes (the [`StreamDriver`] dropped → `close()`/last-drop/teardown).
async fn stream_loop(
    vm: Rc<crate::vm::Vm>,
    mut rx: mpsc::UnboundedReceiver<StreamMsg>,
    archive_bytes: Option<Vec<u8>>,
) {
    let interp = vm.interp().clone();
    // Task 1.6: install the bundled program's archive (if any) BEFORE any producer slice
    // loads (the slice re-runs the program's top-level imports). The stream isolate is
    // single-tenant + long-lived, so install once at boot; a decode failure surfaces as the
    // `Init` panic (the producer never builds).
    let archive_err: Option<String> = match archive_bytes {
        Some(bytes) => crate::worker::isolate::install_module_archive(&vm, &bytes).err(),
        None => None,
    };
    // The producer generator, built on `Init`, driven one step per `Resume`. Held as a
    // `Value::Generator` (an `Rc<GeneratorHandle>`) on the isolate's own heap — never
    // crosses the boundary.
    let mut producer: Option<Value> = None;

    while let Some(msg) = rx.recv().await {
        match msg {
            StreamMsg::Init {
                entry_name,
                slice_bytes,
                args,
                shared,
                reply,
            } => {
                if let Some(msg) = archive_err.clone() {
                    let _ = reply.send(StreamReply::Panic(msg));
                    continue;
                }
                match build_producer(&vm, &interp, &entry_name, &slice_bytes, &args, &shared).await {
                    Ok(gen) => {
                        producer = Some(gen);
                        let _ = reply.send(StreamReply::Ack);
                    }
                    Err(msg) => {
                        let _ = reply.send(StreamReply::Panic(msg));
                    }
                }
            }
            StreamMsg::Resume {
                input,
                shared,
                reply,
            } => {
                let Some(gen) = producer.clone() else {
                    let _ = reply.send(StreamReply::Panic(
                        "streaming generator received a demand credit before initialization"
                            .to_string(),
                    ));
                    continue;
                };
                let r = resume_producer(&interp, &gen, &input, &shared).await;
                let _ = reply.send(r);
            }
        }
    }
}

/// Load the `worker fn*` code slice (entry + deps), look up + CALL the entry with the
/// decoded args, and return the LOCAL `Value::Generator` it produces. Runs entirely on
/// the isolate's own Vm — no original-heap access.
async fn build_producer(
    vm: &Rc<crate::vm::Vm>,
    interp: &crate::interp::Interp,
    entry_name: &str,
    slice_bytes: &[u8],
    args: &[u8],
    shared: &[std::sync::Arc<crate::value::SharedNode>],
) -> Result<Value, String> {
    crate::worker::isolate::load_slice(vm, Some(slice_bytes)).await?;
    let entry = vm.user_global(entry_name).ok_or_else(|| {
        format!("streaming generator entry '{entry_name}' was not defined by its code slice")
    })?;
    let arg_values = crate::worker::isolate::decode_args_with_shared(args, shared, interp)?;
    // Calling a `worker fn*` on the isolate's Vm builds a LOCAL `Value::Generator` (the
    // proto's is_worker flag is irrelevant on the isolate — the entry runs as a plain
    // generator there; the cross-thread streaming is the CALLER-side driver). It does
    // NOT run the body yet (consumer-driven).
    match vm
        .call_value(entry, arg_values, crate::span::Span::new(0, 0))
        .await
    {
        Ok(v) if matches!(v.kind(), ValueKind::Generator(_)) => Ok(v),
        Ok(other) => Err(format!(
            "streaming generator entry '{entry_name}' did not produce a generator (got {})",
            crate::interp::type_name(&other)
        )),
        Err(crate::interp::Control::Panic(e)) => Err(e.message),
        Err(crate::interp::Control::Propagate(_)) => {
            Err("streaming generator entry propagated an error".to_string())
        }
        Err(crate::interp::Control::Exit(_)) => {
            Err("exit() is not allowed inside a worker fn*".to_string())
        }
    }
}

/// Resume the LOCAL producer generator by one step with the decoded injected value, and
/// encode the yielded value (or the done/panic outcome) for the reply. The injected
/// value becomes the result of the producer's suspended `yield` expression — the
/// bidirectional `next(v)` round-trip across the boundary.
async fn resume_producer(
    interp: &crate::interp::Interp,
    gen: &Value,
    input: &[u8],
    shared: &[std::sync::Arc<crate::value::SharedNode>],
) -> StreamReply {
    let injected = match crate::worker::serialize::decode_with_shared(input, shared, interp) {
        Ok(v) => v,
        Err(e) => return StreamReply::Panic(e.message()),
    };
    let ValueKind::Generator(handle) = gen.kind() else {
        return StreamReply::Panic("streaming generator producer is not a generator".to_string());
    };
    match handle.resume(injected).await {
        Ok(Some(v)) => match crate::worker::serialize::encode(&v) {
            Ok((bytes, shared)) => StreamReply::Yielded(bytes, shared),
            // A non-sendable yielded value (e.g. a raw native resource) is a sendability
            // panic with a field path — it cannot cross the boundary.
            Err(e) => StreamReply::Panic(e.message()),
        },
        Ok(None) => StreamReply::Done,
        Err(crate::interp::Control::Panic(e)) => StreamReply::Panic(e.message),
        Err(crate::interp::Control::Propagate(_)) => {
            // A top-level `?` propagation inside the producer body ends iteration.
            StreamReply::Done
        }
        Err(crate::interp::Control::Exit(_)) => {
            StreamReply::Panic("exit() is not allowed inside a worker fn*".to_string())
        }
    }
}
