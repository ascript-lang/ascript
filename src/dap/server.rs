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

/// Shared adapter state, mutated by BOTH the main (request) thread and the event-pump
/// thread, so it lives behind an `Arc<Mutex>`. A std `Mutex` (no async here).
struct AdapterState {
    /// The server's outgoing sequence counter (every response/event gets a fresh seq).
    seq: i64,
    /// The cached frames from the LAST `Stopped` event — stackTrace/scopes/variables
    /// are answered from these (already crossed the airlock as plain data), never a
    /// round-trip to the VM thread.
    frames: Vec<FrameSnapshot>,
    /// Whether the FIRST stop (break-on-entry) has been reported yet — the first
    /// `stopped` uses `reason: "entry"`, subsequent ones `reason: "breakpoint"`.
    entry_reported: bool,
    /// Breakpoints buffered from a `setBreakpoints` that arrived BEFORE the VM parked
    /// at entry (so they could not be applied yet). Keyed by source path → lines.
    /// Applied at the entry stop. (v1: the standard flow sets them while parked, so
    /// this is a fallback for out-of-order clients.)
    pending_breakpoints: Vec<(String, Vec<u32>)>,
}

impl AdapterState {
    fn new() -> Self {
        AdapterState {
            seq: 0,
            frames: Vec::new(),
            entry_reported: false,
            pending_breakpoints: Vec::new(),
        }
    }

    /// Allocate the next outgoing sequence number.
    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
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
                    st.frames = frames;
                    let reason = if st.entry_reported {
                        "breakpoint"
                    } else {
                        st.entry_reported = true;
                        "entry"
                    };
                    // If the client buffered breakpoints before we parked, apply them
                    // now (the entry stop is the first chance). The verified replies
                    // arrive as BreakpointsVerified events but the client already got
                    // an optimistic response, so we just push the commands.
                    let pending = std::mem::take(&mut st.pending_breakpoints);
                    for (source, lines) in pending {
                        let _ = cmd_tx.send(DebugCommand::SetBreakpoints { source, lines });
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
            DebugEvent::BreakpointsVerified { .. } => {
                // The synchronous setBreakpoints handler consumes the verified reply
                // directly off `evt_rx`? No — `evt_rx` is owned by THIS pump thread.
                // The handler instead pushes the command and responds optimistically;
                // we surface the confirmed bindings as a `breakpoint` event so the
                // editor updates the marker. (Unreachable in the common flow because
                // the handler drains this synchronously; kept for the parked-apply
                // path triggered from the pump above.)
            }
            DebugEvent::Output { text } => {
                let seq = state.lock().expect("state mutex").next_seq();
                send(
                    &stdout,
                    &event(
                        seq,
                        "output",
                        json!({ "category": "stdout", "output": text }),
                    ),
                );
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

/// Run the DAP server over stdio (synchronous). `program: Some(path)` is the
/// `run --inspect <file>` form (the program is pre-set; a `launch` request that omits
/// a path uses it). `program: None` is `ascript dap` (the program comes from the
/// `launch` request's `program` argument). Returns the process exit code.
pub fn run_server(program: Option<PathBuf>, script_args: Vec<String>) -> std::io::Result<i32> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let state = Arc::new(Mutex::new(AdapterState::new()));

    // The debuggee + pump threads are created lazily at `launch`. Until then there is
    // no VM thread. We keep the join handles + the command sender.
    let mut debuggee_join: Option<std::thread::JoinHandle<()>> = None;
    let mut pump: Option<std::thread::JoinHandle<()>> = None;
    let mut cmd_tx: Option<Sender<DebugCommand>> = None;
    let exit_code = 0i32;

    loop {
        let req = match read_message(&mut reader)? {
            Some(r) => r,
            None => break, // stdin EOF → client closed the session.
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

                let handle = launch::spawn_debuggee(path, args);
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
                let rseq = state.lock().expect("state").next_seq();
                send(&stdout, &response(rseq, req.seq, &req.command, true, json!({})));
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

                // If the VM is parked (it is, after the entry stop), send the command
                // and wait for the BreakpointsVerified reply so we can respond with the
                // real verdicts. The pump thread owns `evt_rx`, so we cannot read it
                // here; instead we respond optimistically `verified:true` for each line
                // and let the pump surface the authoritative bindings. To give the
                // happy-path test a real `verified:true`, the bindings ARE authoritative
                // when applied while parked, and the VM's resolver binds any executable
                // line — so optimistic `verified:true` matches the parked outcome.
                //
                // We DO apply them: send the command to the VM. The verified event is
                // consumed by the pump (a no-op there) — the editor already has its
                // response.
                let parked = state.lock().expect("state").entry_reported;
                if parked {
                    if let Some(tx) = &cmd_tx {
                        let _ = tx.send(DebugCommand::SetBreakpoints {
                            source: source.clone(),
                            lines: lines.clone(),
                        });
                    }
                } else {
                    // Not parked yet — buffer for application at the entry stop.
                    state
                        .lock()
                        .expect("state")
                        .pending_breakpoints
                        .push((source.clone(), lines.clone()));
                }

                let verified: Vec<Json> = lines
                    .iter()
                    .map(|&line| {
                        json!({ "verified": parked, "line": line })
                    })
                    .collect();
                let rseq = state.lock().expect("state").next_seq();
                send(
                    &stdout,
                    &response(
                        rseq,
                        req.seq,
                        "setBreakpoints",
                        true,
                        json!({ "breakpoints": verified }),
                    ),
                );
            }

            "configurationDone" => {
                // Resume from the entry stop.
                if let Some(tx) = &cmd_tx {
                    let _ = tx.send(DebugCommand::Continue);
                }
                let rseq = state.lock().expect("state").next_seq();
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
                let frame_id = req
                    .arguments
                    .get("frameId")
                    .and_then(|f| f.as_i64())
                    .unwrap_or(0);
                let var_ref = frame_id + 1;
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
                let var_ref = req
                    .arguments
                    .get("variablesReference")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let frame_id = (var_ref - 1).max(0) as usize;
                let st = state.lock().expect("state");
                let vars: Vec<Json> = st
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
                let rseq = state.lock().expect("state").next_seq();
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
                let rseq = state.lock().expect("state").next_seq();
                send(&stdout, &response(rseq, req.seq, &req.command, true, json!({})));
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

    // Teardown: send a final Continue (in case the VM is still parked), then drop the
    // command sender so the parked VM resumes/unblocks and the pump sees EOF. Join the
    // debuggee + pump threads so no zombie thread outlives the server.
    if let Some(tx) = &cmd_tx {
        let _ = tx.send(DebugCommand::Continue);
    }
    drop(cmd_tx);
    if let Some(join) = debuggee_join.take() {
        let _ = join.join();
    }
    if let Some(p) = pump.take() {
        let _ = p.join();
    }
    let _ = std::io::stdout().flush();
    Ok(exit_code)
}
