//! The synchronous DAP server loop + the event-pump thread.
//!
//! See the module doc ([`super`]) for the threading shape. This file owns:
//! - [`AdapterState`] (behind an `Arc<Mutex>`, shared with the pump thread),
//! - [`pump_events`] (the event-pump thread body),
//! - [`run_server`] (the public entry — the synchronous request loop).

use super::launch;
use super::proto::{event, read_message, response, write_message};
use crate::vm::instrument::{DebugCommand, DebugEvent, FrameSnapshot};
use serde_json::{json, Value as Json};
use std::io::{BufReader, Stdout, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// The SESSION-scoped slice of the adapter state: every field tied to a specific
/// debuggee/pump GENERATION (one `launch` → run → terminate). Grouped into its own
/// `Default`-able struct so a re-`launch` resets the whole slice in one move
/// (`self.session = SessionState::default()`) — and, crucially, so adding a future
/// session-scoped field here resets it AUTOMATICALLY rather than depending on someone
/// remembering to extend a hand-maintained reset allowlist (the exact stale-state bug
/// class this grouping guards against). The connection-scoped counters (`seq`, `bp_id`)
/// deliberately live on [`AdapterState`] directly, OUTSIDE this struct, so they survive
/// a re-launch (the client keeps one monotonic view of them across the connection).
#[derive(Default, PartialEq, Eq)]
struct SessionState {
    /// The cached frames from the LAST `Stopped` event — stackTrace/scopes/variables
    /// are answered from these (already crossed the airlock as plain data), never a
    /// round-trip to the VM thread.
    frames: Vec<FrameSnapshot>,
    /// Whether the FIRST stop (break-on-entry) has been reported yet — the first
    /// `stopped` uses `reason: "entry"`, subsequent ones `reason: "breakpoint"`.
    entry_reported: bool,
    /// Whether the VM is CURRENTLY parked at a stop (set on every `stopped`, cleared on
    /// any resume command). Distinct from `entry_reported` (which latches true forever):
    /// `evaluate` needs a frame to exist RIGHT NOW, so it gates on this — an `evaluate`
    /// sent between a `continue` and the next breakpoint is rejected (success:false)
    /// rather than sent to a non-parked VM whose reply would never arrive (review F2).
    is_stopped: bool,
    /// Breakpoints buffered from a `setBreakpoints` that arrived BEFORE the VM parked
    /// at entry (so they could not be applied yet). `(source, lines, ids)` — the ids
    /// are the stable per-breakpoint ids already handed to the editor, carried so the
    /// `BreakpointsVerified` reply can be correlated back to them. Applied at the entry
    /// stop, in arrival order.
    pending_breakpoints: Vec<(String, Vec<u32>, Vec<i64>)>,
    /// FIFO of breakpoint-id lists awaiting a `BreakpointsVerified` reply, pushed at the
    /// moment a `SetBreakpoints` command is SENT to the VM (parked-apply or entry-apply).
    /// The VM processes commands in order, so the pump pops the front to correlate the
    /// next verified reply to the ids it must update via a `breakpoint` event (F1).
    pending_verify: std::collections::VecDeque<Vec<i64>>,
    /// FIFO of `evaluate` request seqs awaiting an `EvaluateResult` reply, pushed when an
    /// `Evaluate` command is SENT to the VM. The VM processes commands in order and ships
    /// exactly one `EvaluateResult` per `Evaluate`, so the pump pops the front to correlate
    /// the reply back to the request seq and send the `evaluate` RESPONSE itself (the
    /// reply crosses the pump-owned `evt_rx`, so the synchronous request handler cannot
    /// read it — same shape as `pending_verify`).
    pending_evaluate: std::collections::VecDeque<i64>,
}

/// Shared adapter state, mutated by BOTH the main (request) thread and the event-pump
/// thread, so it lives behind an `Arc<Mutex>`. A std `Mutex` (no async here).
struct AdapterState {
    /// The server's outgoing sequence counter (every response/event gets a fresh seq).
    /// CONNECTION-scoped — survives a re-launch.
    seq: i64,
    /// Monotonic breakpoint-id allocator (stable ids the editor uses to update markers).
    /// CONNECTION-scoped — survives a re-launch.
    bp_id: i64,
    /// The per-debuggee-generation state, reset wholesale on a re-`launch`.
    session: SessionState,
}

impl AdapterState {
    fn new() -> Self {
        AdapterState {
            seq: 0,
            bp_id: 0,
            session: SessionState::default(),
        }
    }

    /// Allocate the next outgoing sequence number.
    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    /// Allocate the next stable breakpoint id.
    fn next_bp_id(&mut self) -> i64 {
        self.bp_id += 1;
        self.bp_id
    }

    /// Reset the SESSION-scoped state to its fresh-session default, in preparation for a
    /// re-`launch` while a prior session is being torn down. The whole [`SessionState`]
    /// is replaced in one move, so a stale event from the OLD pump thread (already
    /// detached/joined by the caller) cannot leak `frames`, `pending_*`, or
    /// `entry_reported`/`is_stopped` into the NEW session — AND any future session field
    /// added to `SessionState` is reset automatically (compiler-enforced, no allowlist to
    /// forget). The connection-scoped counters (`seq`, `bp_id`) are left untouched.
    fn reset_session(&mut self) {
        self.session = SessionState::default();
    }
}

/// Write a fully-formed DAP message to the shared stdout (locking both the adapter
/// state for the seq counter — already provided by the caller — and the stdout mutex).
fn send(stdout: &Mutex<Stdout>, msg: &Json) {
    let mut out = stdout.lock().expect("stdout mutex");
    let _ = write_message(&mut *out, msg);
}

/// The event-pump thread body: translate VM `DebugEvent`s into DAP events on stdout.
/// Runs until the event channel closes (the debuggee dropped its hook after
/// `Terminated`). Shares `state` + `stdout` with the main thread.
fn pump_events(
    evt_rx: Receiver<DebugEvent>,
    state: Arc<Mutex<AdapterState>>,
    stdout: Arc<Mutex<Stdout>>,
    cmd_tx: Sender<DebugCommand>,
) {
    while let Ok(evt) = evt_rx.recv() {
        match evt {
            DebugEvent::Stopped { frames, .. } => {
                // Cache the frames + decide the stop reason, then emit `stopped`.
                let (seq, reason) = {
                    let mut st = state.lock().expect("state mutex");
                    st.session.frames = frames;
                    st.session.is_stopped = true; // the VM is now parked (review F2).
                    let reason = if st.session.entry_reported {
                        "breakpoint"
                    } else {
                        st.session.entry_reported = true;
                        "entry"
                    };
                    // If the client buffered breakpoints before we parked, apply them now
                    // (the entry stop is the first chance). Each was answered with a
                    // pending (`verified:false`) response carrying stable ids; push the
                    // command AND the id list onto `pending_verify` so the authoritative
                    // `BreakpointsVerified` reply correlates back and emits a `breakpoint`
                    // event with the real verdict (F1).
                    let pending = std::mem::take(&mut st.session.pending_breakpoints);
                    for (source, lines, ids) in pending {
                        let _ = cmd_tx.send(DebugCommand::SetBreakpoints { source, lines });
                        st.session.pending_verify.push_back(ids);
                    }
                    (st.next_seq(), reason)
                };
                send(
                    &stdout,
                    &event(
                        seq,
                        "stopped",
                        json!({
                            "reason": reason,
                            "threadId": 1,
                            "allThreadsStopped": true,
                        }),
                    ),
                );
            }
            DebugEvent::BreakpointsVerified { results } => {
                // The VM's AUTHORITATIVE per-line verdict. `evt_rx` is owned by THIS pump
                // thread, so the synchronous `setBreakpoints` handler could not read it;
                // instead it responded with `verified:false` (pending) + assigned a stable
                // id per requested breakpoint, pushing the id list onto `pending_verify`
                // (FIFO — the VM applies `SetBreakpoints` commands in order). We pop the
                // matching id list and emit a DAP `breakpoint` event per line carrying the
                // REAL verdict, so the editor flips the marker to verified (or leaves an
                // unbindable line unverified). This is the DAP-sanctioned late-verification
                // path (no fabricated verdicts).
                let ids = {
                    let mut st = state.lock().expect("state mutex");
                    st.session.pending_verify.pop_front()
                };
                let Some(ids) = ids else { continue };
                for (i, binding) in results.iter().enumerate() {
                    let Some(&id) = ids.get(i) else { break };
                    let seq = state.lock().expect("state mutex").next_seq();
                    send(
                        &stdout,
                        &event(
                            seq,
                            "breakpoint",
                            json!({
                                "reason": "changed",
                                "breakpoint": {
                                    "id": id,
                                    "verified": binding.verified,
                                    "line": binding.line,
                                },
                            }),
                        ),
                    );
                }
            }
            DebugEvent::Output { text, stderr } => {
                let seq = state.lock().expect("state mutex").next_seq();
                let category = if stderr { "stderr" } else { "stdout" };
                send(
                    &stdout,
                    &event(
                        seq,
                        "output",
                        json!({ "category": category, "output": text }),
                    ),
                );
            }
            DebugEvent::EvaluateResult { ok, display } => {
                // The VM's reply to a `DebugCommand::Evaluate`. `evt_rx` is owned by THIS
                // pump thread, so the synchronous `evaluate` request handler could not read
                // the result; it pushed its request seq onto `pending_evaluate` (FIFO — the
                // VM ships one reply per `Evaluate`, in order). Pop the matching request seq
                // and send the `evaluate` RESPONSE carrying the rendered result. A failed
                // eval (parse error / thrown panic) responds with `success:false` + the
                // error text so the editor surfaces it without hanging.
                let req_seq = {
                    let mut st = state.lock().expect("state mutex");
                    st.session.pending_evaluate.pop_front()
                };
                let Some(req_seq) = req_seq else { continue };
                let rseq = state.lock().expect("state mutex").next_seq();
                let body = if ok {
                    json!({ "result": display, "variablesReference": 0 })
                } else {
                    json!({ "error": display })
                };
                send(&stdout, &response(rseq, req_seq, "evaluate", ok, body));
            }
            DebugEvent::Terminated { exit_code } => {
                let (seq1, seq2) = {
                    let mut st = state.lock().expect("state mutex");
                    (st.next_seq(), st.next_seq())
                };
                // `exited` carries the code; `terminated` ends the debug session.
                send(&stdout, &event(seq1, "exited", json!({ "exitCode": exit_code })));
                send(&stdout, &event(seq2, "terminated", json!({})));
                // The channel will close right after; loop will end on the next recv.
            }
        }
    }
}

/// Tear down a live debuggee+pump generation: resume the (possibly parked) VM, drop the
/// command sender so the VM unblocks and the pump sees the event channel close, then JOIN
/// both threads so no zombie thread outlives the generation. Used by BOTH the end-of-loop
/// shutdown AND a re-`launch` (where the OLD session must be fully reaped before the new
/// one starts — otherwise the detached old pump thread keeps writing stale `Stopped`/
/// `BreakpointsVerified`/`EvaluateResult` events into the SHARED state, corrupting the new
/// session's `frames`/`pending_*`). Takes the handles BY VALUE (the caller's `Option`s are
/// `.take()`n at the call site), so a torn-down generation is unreachable afterward.
fn teardown_session(
    cmd_tx: Option<Sender<DebugCommand>>,
    debuggee_join: Option<std::thread::JoinHandle<()>>,
    pump: Option<std::thread::JoinHandle<()>>,
) {
    // A final Continue in case the VM is still parked (so it can finish + close its hook).
    if let Some(tx) = &cmd_tx {
        let _ = tx.send(DebugCommand::Continue);
    }
    // Drop the sender: the parked VM's blocking `recv` returns `Err` → it resumes/unblocks,
    // runs to completion, drops its event `Sender`, and the pump's `recv` then sees EOF.
    drop(cmd_tx);
    if let Some(join) = debuggee_join {
        let _ = join.join();
    }
    if let Some(p) = pump {
        let _ = p.join();
    }
}

/// Run the DAP server over stdio (synchronous). `program: Some(path)` is the
/// `run --inspect <file>` form (the program is pre-set; a `launch` request that omits
/// a path uses it). `program: None` is `ascript dap` (the program comes from the
/// `launch` request's `program` argument). Returns the process exit code.
pub fn run_server(
    program: Option<PathBuf>,
    script_args: Vec<String>,
    caps: Option<crate::stdlib::caps::CapSet>,
) -> std::io::Result<i32> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let state = Arc::new(Mutex::new(AdapterState::new()));

    // The debuggee + pump threads are created lazily at `launch`. Until then there is
    // no VM thread. We keep the join handles + the command sender.
    let mut debuggee_join: Option<std::thread::JoinHandle<()>> = None;
    let mut pump: Option<std::thread::JoinHandle<()>> = None;
    let mut cmd_tx: Option<Sender<DebugCommand>> = None;
    // The ADAPTER process always exits 0 on a clean teardown. The DEBUGGEE's real exit
    // code is NOT propagated to this process's exit status — by design, it is reported to
    // the DAP client via the `exited` event (`{"exitCode": …}`) the pump emits on
    // `DebugEvent::Terminated`. The editor consumes that event; the `ascript dap` /
    // `run --inspect` process's own exit status reflects only whether the adapter itself
    // shut down cleanly. (Not a bug: a debugger front-end reads the debuggee's code off
    // the protocol, not off the adapter's process exit.)
    let exit_code = 0i32;

    loop {
        let req = match read_message(&mut reader) {
            Ok(Some(r)) => r,
            Ok(None) => break, // stdin EOF → client closed the session.
            // A malformed frame (bad Content-Length / non-JSON body) must NOT bypass
            // teardown via `?` — fall through to the clean shutdown so the parked
            // debuggee is resumed and both threads are joined (F4).
            Err(e) => {
                eprintln!("dap: malformed message ({e}); shutting down");
                break;
            }
        };

        match req.command.as_str() {
            "initialize" => {
                let (rseq, eseq) = {
                    let mut st = state.lock().expect("state");
                    (st.next_seq(), st.next_seq())
                };
                send(
                    &stdout,
                    &response(
                        rseq,
                        req.seq,
                        "initialize",
                        true,
                        json!({
                            "supportsConfigurationDoneRequest": true,
                        }),
                    ),
                );
                // The DAP spec: emit `initialized` so the client sends its config
                // (setBreakpoints, then configurationDone).
                send(&stdout, &event(eseq, "initialized", json!({})));
            }

            // `attach` is an alias for `launch` in v1 (same behavior).
            "launch" | "attach" => {
                // The program path: prefer the request's `program`, else the CLI
                // pre-set one (`run --inspect`).
                let path = req
                    .arguments
                    .get("program")
                    .and_then(|p| p.as_str())
                    .map(PathBuf::from)
                    .or_else(|| program.clone());
                let args = req
                    .arguments
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|| script_args.clone());

                let Some(path) = path else {
                    let rseq = state.lock().expect("state").next_seq();
                    send(
                        &stdout,
                        &response(
                            rseq,
                            req.seq,
                            &req.command,
                            false,
                            json!({ "error": "no program path: pass `program` in the launch request or use `run --inspect <file>`" }),
                        ),
                    );
                    continue;
                };

                // Re-launch hygiene: if a prior session is still live, REAP it before
                // starting the new one. Otherwise the old debuggee/pump threads are merely
                // detached (their join handles overwritten below) — the old pump keeps
                // running and writes stale `Stopped`/`BreakpointsVerified`/`EvaluateResult`
                // events into the SHARED `AdapterState`, corrupting the new session's
                // `frames`/`pending_*`/`entry_reported`. Tear the old generation down
                // (resume + drop sender + JOIN both threads), THEN reset the session-scoped
                // state so the new launch starts byte-clean. The connection seq/bp_id
                // counters are preserved (`reset_session` leaves them).
                if cmd_tx.is_some() || debuggee_join.is_some() || pump.is_some() {
                    teardown_session(cmd_tx.take(), debuggee_join.take(), pump.take());
                    state.lock().expect("state").reset_session();
                }

                // Send the launch RESPONSE before spawning, so it is guaranteed to
                // precede the entry `stopped` event the pump will emit once the debuggee
                // parks (the debuggee must compile + build a runtime first, but ordering
                // should not rely on that latency) (F5).
                let rseq = state.lock().expect("state").next_seq();
                send(&stdout, &response(rseq, req.seq, &req.command, true, json!({})));

                let handle = launch::spawn_debuggee(path, args, caps.clone());
                cmd_tx = Some(handle.cmd_tx.clone());
                debuggee_join = Some(handle.join);
                let evt_rx = handle.evt_rx;
                let pump_state = state.clone();
                let pump_stdout = stdout.clone();
                let pump_cmd = handle.cmd_tx;
                pump = Some(
                    std::thread::Builder::new()
                        .name("ascript-dap-pump".to_string())
                        .spawn(move || pump_events(evt_rx, pump_state, pump_stdout, pump_cmd))
                        .expect("spawn pump thread"),
                );
            }

            "setBreakpoints" => {
                let source = req
                    .arguments
                    .get("source")
                    .and_then(|s| s.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                let lines: Vec<u32> = req
                    .arguments
                    .get("breakpoints")
                    .and_then(|b| b.as_array())
                    .map(|bps| {
                        bps.iter()
                            .filter_map(|b| b.get("line").and_then(|l| l.as_u64()).map(|l| l as u32))
                            .collect()
                    })
                    .unwrap_or_default();

                // DAP setBreakpoints is replace-all per source. We CANNOT verify
                // synchronously (the pump thread owns `evt_rx`), so we respond with the
                // DAP-sanctioned PENDING state — each breakpoint gets a stable `id` and
                // `verified:false` — then the VM's authoritative `BreakpointsVerified`
                // reply is correlated by id (FIFO via `pending_verify`) and surfaced as a
                // `breakpoint` event that flips the marker to its REAL verdict (F1). No
                // fabricated verdicts: an unbindable line stays unverified.
                let (ids, rseq) = {
                    let mut st = state.lock().expect("state");
                    let ids: Vec<i64> = lines.iter().map(|_| st.next_bp_id()).collect();
                    if st.session.entry_reported {
                        // Parked: apply immediately + queue the ids for the reply.
                        if let Some(tx) = &cmd_tx {
                            let _ = tx.send(DebugCommand::SetBreakpoints {
                                source: source.clone(),
                                lines: lines.clone(),
                            });
                            st.session.pending_verify.push_back(ids.clone());
                        }
                    } else {
                        // Not parked yet — buffer for application at the entry stop
                        // (carrying the ids so the eventual reply correlates).
                        st.session
                            .pending_breakpoints
                            .push((source.clone(), lines.clone(), ids.clone()));
                    }
                    (ids, st.next_seq())
                };

                let breakpoints: Vec<Json> = lines
                    .iter()
                    .zip(ids.iter())
                    .map(|(&line, &id)| json!({ "id": id, "verified": false, "line": line }))
                    .collect();
                send(
                    &stdout,
                    &response(
                        rseq,
                        req.seq,
                        "setBreakpoints",
                        true,
                        json!({ "breakpoints": breakpoints }),
                    ),
                );
            }

            "configurationDone" => {
                // Resume from the entry stop.
                if let Some(tx) = &cmd_tx {
                    let _ = tx.send(DebugCommand::Continue);
                }
                let rseq = {
                    let mut st = state.lock().expect("state");
                    st.session.is_stopped = false; // resumed (review F2).
                    st.next_seq()
                };
                send(
                    &stdout,
                    &response(rseq, req.seq, "configurationDone", true, json!({})),
                );
            }

            "threads" => {
                let rseq = state.lock().expect("state").next_seq();
                send(
                    &stdout,
                    &response(
                        rseq,
                        req.seq,
                        "threads",
                        true,
                        json!({ "threads": [ { "id": 1, "name": "main" } ] }),
                    ),
                );
            }

            "stackTrace" => {
                let st = state.lock().expect("state");
                let stack_frames: Vec<Json> = st
                    .session
                    .frames
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        json!({
                            "id": i,
                            "name": f.function,
                            // The snapshot is 0-based; DAP is 1-based.
                            "line": f.line + 1,
                            "column": f.column + 1,
                        })
                    })
                    .collect();
                let total = stack_frames.len();
                drop(st);
                let rseq = state.lock().expect("state").next_seq();
                send(
                    &stdout,
                    &response(
                        rseq,
                        req.seq,
                        "stackTrace",
                        true,
                        json!({ "stackFrames": stack_frames, "totalFrames": total }),
                    ),
                );
            }

            "scopes" => {
                // One Locals scope per frame. `variablesReference` encodes the frame id
                // as `frameId + 1` (0 is reserved for "no children" in DAP).
                //
                // `frameId` is client-supplied: clamp a negative/absurd value to 0 (no
                // real frame → the downstream `variables` lookup returns empty, never a
                // panic) and use `saturating_add` so `i64::MAX` does not overflow (a
                // debug-build panic / release wrap-to-`i64::MIN`). The legitimate
                // round-trip frameId→var_ref→frameId is `id → id+1 → id` for every
                // in-range frame id, which this preserves.
                let frame_id = req
                    .arguments
                    .get("frameId")
                    .and_then(|f| f.as_i64())
                    .unwrap_or(0)
                    .max(0);
                let var_ref = frame_id.saturating_add(1);
                let rseq = state.lock().expect("state").next_seq();
                send(
                    &stdout,
                    &response(
                        rseq,
                        req.seq,
                        "scopes",
                        true,
                        json!({
                            "scopes": [ {
                                "name": "Locals",
                                "variablesReference": var_ref,
                                "expensive": false,
                            } ]
                        }),
                    ),
                );
            }

            "variables" => {
                // Decode the frame id from the variablesReference (frameId + 1).
                // `variablesReference` is client-supplied: use `saturating_sub` so
                // `i64::MIN` does not underflow (a debug-build panic), then clamp the
                // negative range to 0. An out-of-range id simply misses
                // `frames.get(..)` below → an empty `variables` list, never a panic.
                let var_ref = req
                    .arguments
                    .get("variablesReference")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let frame_id = var_ref.saturating_sub(1).max(0) as usize;
                let st = state.lock().expect("state");
                let vars: Vec<Json> = st
                    .session
                    .frames
                    .get(frame_id)
                    .map(|f| {
                        f.locals
                            .iter()
                            .map(|(name, value)| {
                                json!({
                                    "name": name,
                                    "value": value,
                                    "variablesReference": 0,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                drop(st);
                let rseq = state.lock().expect("state").next_seq();
                send(
                    &stdout,
                    &response(
                        rseq,
                        req.seq,
                        "variables",
                        true,
                        json!({ "variables": vars }),
                    ),
                );
            }

            "continue" => {
                if let Some(tx) = &cmd_tx {
                    let _ = tx.send(DebugCommand::Continue);
                }
                let rseq = {
                    let mut st = state.lock().expect("state");
                    st.session.is_stopped = false; // resumed (review F2).
                    st.next_seq()
                };
                send(
                    &stdout,
                    &response(
                        rseq,
                        req.seq,
                        "continue",
                        true,
                        json!({ "allThreadsContinued": true }),
                    ),
                );
            }

            // v1 stepping: the VM records the step mode but does NOT yet synthesize the
            // transient single-line breakpoints, so a `next`/`stepIn`/`stepOut`
            // currently RESUMES TO THE NEXT BREAKPOINT (real single-line stepping is a
            // later task). We send the matching command honestly; do not claim true
            // stepping.
            "next" | "stepIn" | "stepOut" => {
                if let Some(tx) = &cmd_tx {
                    let cmd = match req.command.as_str() {
                        "next" => DebugCommand::Next,
                        "stepIn" => DebugCommand::StepIn,
                        _ => DebugCommand::StepOut,
                    };
                    let _ = tx.send(cmd);
                }
                let rseq = {
                    let mut st = state.lock().expect("state");
                    st.session.is_stopped = false; // resumed (review F2).
                    st.next_seq()
                };
                send(&stdout, &response(rseq, req.seq, &req.command, true, json!({})));
            }

            "evaluate" => {
                // DAP `evaluate` — the Watch panel / Debug Console / hover. Evaluate the
                // `expression` in the paused frame `frameId`, returning the rendered value.
                // The result crosses the pump-owned `evt_rx` as an `EvaluateResult`, so we
                // CANNOT respond synchronously: send the command, push our request seq onto
                // `pending_evaluate`, and let the pump correlate the reply and respond (same
                // shape as `setBreakpoints` ↔ `BreakpointsVerified`).
                let expr = req
                    .arguments
                    .get("expression")
                    .and_then(|e| e.as_str())
                    .unwrap_or("")
                    .to_string();
                // `frameId` is optional in DAP (a global eval omits it); default to the
                // innermost frame (0). The VM bounds-checks the id.
                let frame_id = req
                    .arguments
                    .get("frameId")
                    .and_then(|f| f.as_i64())
                    .unwrap_or(0)
                    .max(0) as usize;

                // Only meaningful while the VM is CURRENTLY parked (a frame must exist).
                // `is_stopped` (set on `stopped`, cleared on every resume) — NOT the
                // latching `entry_reported` — so an `evaluate` sent between a `continue`
                // and the next breakpoint is rejected here rather than dispatched to a
                // non-parked VM whose reply would never arrive, dangling the request
                // (review F2). Respond unsuccessfully instead (deadlock-avoidance).
                let parked = {
                    let st = state.lock().expect("state");
                    st.session.is_stopped && cmd_tx.is_some()
                };
                if parked {
                    if let Some(tx) = &cmd_tx {
                        let _ = tx.send(DebugCommand::Evaluate { expr, frame_id });
                        state
                            .lock()
                            .expect("state")
                            .session
                            .pending_evaluate
                            .push_back(req.seq);
                        // The pump sends the response when the `EvaluateResult` arrives.
                    }
                } else {
                    let rseq = state.lock().expect("state").next_seq();
                    send(
                        &stdout,
                        &response(
                            rseq,
                            req.seq,
                            "evaluate",
                            false,
                            json!({ "error": "not paused: evaluate requires a stopped frame" }),
                        ),
                    );
                }
            }

            "disconnect" | "terminate" => {
                // Resume the debuggee so it can finish, respond, then exit the loop.
                if let Some(tx) = &cmd_tx {
                    let _ = tx.send(DebugCommand::Continue);
                }
                let rseq = state.lock().expect("state").next_seq();
                send(&stdout, &response(rseq, req.seq, &req.command, true, json!({})));
                break;
            }

            // Unknown / unimplemented request — respond unsuccessfully so the client
            // is not left waiting (a deadlock-avoidance posture).
            other => {
                let rseq = state.lock().expect("state").next_seq();
                send(
                    &stdout,
                    &response(
                        rseq,
                        req.seq,
                        other,
                        false,
                        json!({ "error": format!("unsupported request '{other}'") }),
                    ),
                );
            }
        }
    }

    // Teardown: resume the (possibly parked) VM, drop the command sender so the VM
    // unblocks and the pump sees EOF, then join both threads so no zombie thread outlives
    // the server (the same reaping a re-`launch` performs on the OLD generation).
    teardown_session(cmd_tx.take(), debuggee_join.take(), pump.take());
    let _ = std::io::stdout().flush();
    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact `frameId → variablesReference` encoding the `scopes` handler uses,
    /// hoisted so the overflow/clamp behavior is unit-testable without a live server.
    fn scopes_var_ref(frame_id_arg: i64) -> i64 {
        frame_id_arg.max(0).saturating_add(1)
    }

    /// The exact `variablesReference → frameId` decode the `variables` handler uses.
    fn variables_frame_id(var_ref_arg: i64) -> usize {
        var_ref_arg.saturating_sub(1).max(0) as usize
    }

    /// (1) `scopes` with `frameId: i64::MAX` must NOT overflow — `frame_id + 1` would
    /// panic in a debug build (or wrap to `i64::MIN` in release). `saturating_add`
    /// pins it at `i64::MAX` instead, a valid (if absurd) variablesReference.
    #[test]
    fn scopes_frame_id_max_does_not_overflow() {
        // Pre-fix this expression was `i64::MAX + 1` → arithmetic overflow panic.
        let var_ref = scopes_var_ref(i64::MAX);
        assert_eq!(var_ref, i64::MAX, "saturates instead of overflowing");
        // And the downstream variables decode of that var_ref also does not panic.
        // We only care that it didn't panic and decodes to a positive out-of-range
        // index (`frames.get(huge)` → None → empty variables); the exact value is
        // platform-`usize`-width-dependent, so assert the property, not the number.
        let frame_id = variables_frame_id(var_ref);
        assert!(
            frame_id > 0,
            "absurd var_ref decodes to a positive out-of-range index, no panic"
        );
    }

    /// A negative/absurd client `frameId` clamps to a valid, non-panicking var_ref
    /// (no real frame → empty scopes/variables downstream, never a panic).
    #[test]
    fn scopes_frame_id_negative_clamps() {
        assert_eq!(scopes_var_ref(-1), 1);
        assert_eq!(scopes_var_ref(i64::MIN), 1);
        // `variables` with `variablesReference: i64::MIN` must not underflow either.
        assert_eq!(variables_frame_id(i64::MIN), 0);
        assert_eq!(variables_frame_id(0), 0);
    }

    /// The legitimate round-trip `frameId → var_ref → frameId` is the identity for
    /// every in-range frame id (the encoding the happy-path test relies on).
    #[test]
    fn scopes_variables_round_trip_identity() {
        for frame_id in [0i64, 1, 2, 7, 100, 1_000_000] {
            let var_ref = scopes_var_ref(frame_id);
            assert_eq!(var_ref, frame_id + 1, "frameId {frame_id} → var_ref");
            assert_eq!(
                variables_frame_id(var_ref),
                frame_id as usize,
                "var_ref → frameId {frame_id} round-trips"
            );
        }
    }

    /// (2) `reset_session` clears every SESSION-scoped field (so a re-launch starts
    /// clean — no stale frames/pending entries from the old, torn-down generation),
    /// while PRESERVING the connection-scoped `seq`/`bp_id` counters (the client keeps
    /// one monotonic view of those across the whole connection). The wholesale-reset
    /// assertion (`session == SessionState::default()`) is the compiler-enforced guard:
    /// a future session-scoped field is covered automatically, never silently forgotten.
    #[test]
    fn reset_session_clears_session_state_preserves_counters() {
        let mut st = AdapterState::new();
        // Simulate a live session that accumulated state.
        st.seq = 42;
        st.bp_id = 7;
        st.session.frames = vec![FrameSnapshot {
            function: "stale".into(),
            line: 9,
            column: 1,
            locals: vec![("x".into(), "1".into())],
        }];
        st.session.entry_reported = true;
        st.session.is_stopped = true;
        st.session
            .pending_breakpoints
            .push(("f.as".into(), vec![2], vec![1]));
        st.session.pending_verify.push_back(vec![1]);
        st.session.pending_evaluate.push_back(99);

        st.reset_session();

        // The whole session slice is back to its fresh-session default — this single
        // assertion covers every current AND future session-scoped field.
        assert!(
            st.session == SessionState::default(),
            "the entire session slice resets to default (compiler-enforced)"
        );
        // Spot-check the individual fields too (clearer failure messages).
        assert!(st.session.frames.is_empty(), "stale frames cleared");
        assert!(!st.session.entry_reported, "entry latch reset");
        assert!(!st.session.is_stopped, "stopped flag reset");
        assert!(st.session.pending_breakpoints.is_empty(), "pending breakpoints cleared");
        assert!(st.session.pending_verify.is_empty(), "pending verify cleared");
        assert!(st.session.pending_evaluate.is_empty(), "pending evaluate cleared");

        // Connection-scoped counters survive (no rewind across a re-launch).
        assert_eq!(st.seq, 42, "outgoing seq counter preserved");
        assert_eq!(st.bp_id, 7, "breakpoint-id allocator preserved");
    }
}
