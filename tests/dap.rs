//! End-to-end Debug Adapter Protocol (DAP) smoke tests.
//!
//! Spawns the real `ascript run --inspect <file>` (or `ascript dap`) binary and speaks
//! DAP (`Content-Length`-framed JSON) over its stdin/stdout, proving the adapter talks
//! the wire protocol and drives the VM debug core to a real stop/inspect/resume cycle.
//!
//! EVERY test is watchdog-guarded: a dedicated reader thread feeds framed messages onto
//! an `mpsc` channel and the test thread pulls with `recv_timeout` bounded by a wall
//! deadline, so a protocol DEADLOCK fails fast (with the child killed) instead of
//! hanging the host. macOS has no `timeout(1)`; this is the portable equivalent.
//!
//! Gated on the `dap` feature; under `--no-default-features` the whole file (and the
//! `--inspect`/`dap` surface) compiles out, so the file is empty there — which is also
//! the evidence that `dap` is cfg-gated (Gate test #3).

#![cfg(feature = "dap")]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// What the background reader thread yields for each frame it pulls off stdout.
enum ReadItem {
    Msg(Value),
    Eof,
}

/// A minimal DAP client driving the spawned adapter over its stdio. Reads run on a
/// dedicated background thread; the test thread pulls with a real deadline.
struct DapClient {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<ReadItem>,
    seq: i64,
}

impl DapClient {
    /// Spawn `ascript run --inspect <program>` (the pre-set-program form).
    fn spawn_inspect(program: &std::path::Path) -> Self {
        Self::spawn_inspect_with_flags(program, &[])
    }

    /// Spawn `ascript run [flags] --inspect <program>` — `flags` are extra `run` flags
    /// placed BEFORE `--inspect` (e.g. `--sandbox`, `--deny fs`) so capability handling
    /// is exercised under the debugger (review F2).
    fn spawn_inspect_with_flags(program: &std::path::Path, flags: &[&str]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ascript"));
        cmd.arg("run");
        for f in flags {
            cmd.arg(f);
        }
        let mut child = cmd
            .arg("--inspect")
            .arg(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn `ascript run --inspect`");
        Self::wrap(child.stdin.take().unwrap(), child.stdout.take().unwrap(), child)
    }

    fn wrap(stdin: ChildStdin, stdout: ChildStdout, child: Child) -> Self {
        let stdout = BufReader::new(stdout);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = stdout;
            loop {
                match read_framed_message(&mut stdout) {
                    Some(msg) => {
                        if tx.send(ReadItem::Msg(msg)).is_err() {
                            return;
                        }
                    }
                    None => {
                        let _ = tx.send(ReadItem::Eof);
                        return;
                    }
                }
            }
        });
        DapClient {
            child,
            stdin: Some(stdin),
            rx,
            seq: 0,
        }
    }

    fn send(&mut self, msg: &Value) {
        let body = serde_json::to_vec(msg).expect("serialize");
        let stdin = self.stdin.as_mut().expect("stdin closed");
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("header");
        stdin.write_all(&body).expect("body");
        stdin.flush().expect("flush");
    }

    /// Send a DAP request with the given command + arguments, returning its `seq`.
    fn request(&mut self, command: &str, arguments: Value) -> i64 {
        self.seq += 1;
        let seq = self.seq;
        self.send(&json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        }));
        seq
    }

    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// Pull the next framed message, honoring `deadline`.
    fn next_message(&mut self, deadline: Instant, waiting_for: &str) -> Option<Value> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = self.child.kill();
                panic!("timed out waiting for {waiting_for}{}", self.drain_stderr());
            }
            let step = remaining.min(Duration::from_millis(250));
            match self.rx.recv_timeout(step) {
                Ok(ReadItem::Msg(msg)) => return Some(msg),
                Ok(ReadItem::Eof) => return None,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Read until a `response` for the given request `seq` arrives.
    fn read_response(&mut self, request_seq: i64, deadline: Instant) -> Value {
        let waiting = format!("response request_seq={request_seq}");
        loop {
            let msg = self.next_message(deadline, &waiting).unwrap_or_else(|| {
                let _ = self.child.kill();
                panic!("stream closed before response request_seq={request_seq}{}", self.drain_stderr())
            });
            if msg.get("type").and_then(Value::as_str) == Some("response")
                && msg.get("request_seq").and_then(Value::as_i64) == Some(request_seq)
            {
                return msg;
            }
        }
    }

    /// Read until an `event` with the given name arrives, returning it.
    fn read_event(&mut self, name: &str, deadline: Instant) -> Value {
        let waiting = format!("`{name}` event");
        loop {
            let msg = self.next_message(deadline, &waiting).unwrap_or_else(|| {
                let _ = self.child.kill();
                panic!("stream closed before `{name}` event{}", self.drain_stderr())
            });
            if msg.get("type").and_then(Value::as_str) == Some("event")
                && msg.get("event").and_then(Value::as_str) == Some(name)
            {
                return msg;
            }
        }
    }

    /// Collect all `output`-event text seen up to (and including) the `terminated`
    /// event, returning the concatenation. Bounded by `deadline`.
    fn drain_output_until_terminated(&mut self, deadline: Instant) -> String {
        let mut out = String::new();
        loop {
            let msg = self
                .next_message(deadline, "terminated event (draining output)")
                .unwrap_or_else(|| {
                    let _ = self.child.kill();
                    panic!("stream closed before `terminated`{}", self.drain_stderr())
                });
            if msg.get("type").and_then(Value::as_str) == Some("event") {
                match msg.get("event").and_then(Value::as_str) {
                    Some("output") => {
                        if let Some(t) = msg["body"]["output"].as_str() {
                            out.push_str(t);
                        }
                    }
                    Some("terminated") => return out,
                    _ => {}
                }
            }
        }
    }

    fn drain_stderr(&mut self) -> String {
        if let Some(mut err) = self.child.stderr.take() {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            if !s.is_empty() {
                return format!("\n--- child stderr ---\n{s}");
            }
        }
        String::new()
    }
}

impl Drop for DapClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read exactly one Content-Length-framed message (background reader thread).
fn read_framed_message(stdout: &mut BufReader<ChildStdout>) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = Some(rest.trim().parse().expect("parse Content-Length"));
        }
    }
    let len = content_length.expect("no Content-Length header");
    let mut body = vec![0u8; len];
    stdout.read_exact(&mut body).ok()?;
    Some(serde_json::from_slice(&body).expect("parse JSON body"))
}

/// Write `src` to a uniquely-named temp `.as` file and return its path.
fn temp_program(name: &str, src: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let unique = format!(
        "ascript_dap_{}_{}_{}.as",
        name,
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    );
    path.push(unique);
    std::fs::write(&path, src).expect("write temp program");
    path
}

const PROGRAM: &str = "fn add(a, b) {\n  let s = a + b\n  return s\n}\nprint(add(2, 3))\n";

/// Test 1 — the happy path: initialize → launch (stop at entry) → setBreakpoints on the
/// `let s` line → configurationDone → stop at the breakpoint → stackTrace/scopes/
/// variables (a=2, b=3 in frame `add`) → continue → terminated; program output `5`
/// appears as an `output` event.
#[test]
fn dap_happy_path_breakpoint_inspect_continue() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let program = temp_program("happy", PROGRAM);
    let mut c = DapClient::spawn_inspect(&program);

    // initialize → response (capabilities) + `initialized` event.
    let init = c.request("initialize", json!({}));
    let resp = c.read_response(init, deadline);
    assert_eq!(resp["success"], json!(true), "initialize ok: {resp}");
    assert_eq!(
        resp["body"]["supportsConfigurationDoneRequest"],
        json!(true),
        "advertises configurationDone support"
    );
    c.read_event("initialized", deadline);

    // launch (program pre-set via --inspect) → stop at entry.
    let launch = c.request("launch", json!({}));
    c.read_response(launch, deadline);
    let stopped = c.read_event("stopped", deadline);
    assert_eq!(
        stopped["body"]["reason"], json!("entry"),
        "first stop is break-on-entry: {stopped}"
    );

    // setBreakpoints on the `let s` line (line 2, 1-based). The response is the
    // DAP-sanctioned PENDING state (`verified:false` + a stable `id`); the VM's REAL
    // verdict arrives next as a `breakpoint` event (F1 — no fabricated verdicts).
    let sb = c.request(
        "setBreakpoints",
        json!({
            "source": { "path": program.to_string_lossy() },
            "breakpoints": [ { "line": 2 } ],
        }),
    );
    let sb_resp = c.read_response(sb, deadline);
    let bps = sb_resp["body"]["breakpoints"].as_array().expect("breakpoints array");
    assert_eq!(bps.len(), 1, "one requested → one binding: {sb_resp}");
    assert_eq!(bps[0]["verified"], json!(false), "pending until the VM verifies: {sb_resp}");
    assert_eq!(bps[0]["line"], json!(2));
    let bp_id = bps[0]["id"].as_i64().expect("breakpoint id");
    // The authoritative verdict: a `breakpoint` event flips the marker to verified.
    let bp_evt = c.read_event("breakpoint", deadline);
    assert_eq!(bp_evt["body"]["breakpoint"]["id"], json!(bp_id), "verdict for our id: {bp_evt}");
    assert_eq!(
        bp_evt["body"]["breakpoint"]["verified"], json!(true),
        "an executable line binds: {bp_evt}"
    );

    // configurationDone → resume from entry → stop at the breakpoint (inside add).
    let cd = c.request("configurationDone", json!({}));
    c.read_response(cd, deadline);
    let stopped2 = c.read_event("stopped", deadline);
    assert_eq!(
        stopped2["body"]["reason"], json!("breakpoint"),
        "second stop is the user breakpoint: {stopped2}"
    );

    // stackTrace → top frame is `add` at line 2 (1-based).
    let st = c.request("stackTrace", json!({ "threadId": 1 }));
    let st_resp = c.read_response(st, deadline);
    let frames = st_resp["body"]["stackFrames"].as_array().expect("frames");
    assert!(!frames.is_empty(), "at least one frame: {st_resp}");
    let top = &frames[0];
    assert_eq!(top["name"], json!("add"), "top frame is add: {st_resp}");
    assert_eq!(top["line"], json!(2), "top frame at line 2 (1-based): {st_resp}");
    let frame_id = top["id"].as_i64().expect("frame id");

    // scopes → one Locals scope with a variablesReference.
    let sc = c.request("scopes", json!({ "frameId": frame_id }));
    let sc_resp = c.read_response(sc, deadline);
    let scopes = sc_resp["body"]["scopes"].as_array().expect("scopes");
    assert_eq!(scopes.len(), 1, "one Locals scope: {sc_resp}");
    let var_ref = scopes[0]["variablesReference"].as_i64().expect("var ref");

    // variables → a=2, b=3 among the locals.
    let vr = c.request("variables", json!({ "variablesReference": var_ref }));
    let vr_resp = c.read_response(vr, deadline);
    let vars = vr_resp["body"]["variables"].as_array().expect("variables");
    let find = |name: &str| -> Option<String> {
        vars.iter()
            .find(|v| v["name"].as_str() == Some(name))
            .and_then(|v| v["value"].as_str().map(String::from))
    };
    assert_eq!(find("a").as_deref(), Some("2"), "a=2 among locals: {vr_resp}");
    assert_eq!(find("b").as_deref(), Some("3"), "b=3 among locals: {vr_resp}");

    // continue → run to completion → output `5` + terminated.
    let cont = c.request("continue", json!({ "threadId": 1 }));
    let cont_resp = c.read_response(cont, deadline);
    assert_eq!(cont_resp["body"]["allThreadsContinued"], json!(true));

    let output = c.drain_output_until_terminated(deadline);
    assert!(
        output.contains("5"),
        "program output `5` arrived as an output event, got: {output:?}"
    );

    c.close_stdin();
    let _ = std::fs::remove_file(&program);
}

/// Test 1c (DBG Task 8 — `evaluate`) — pause inside `add` (a=2, b=3), then evaluate
/// expressions in the paused frame: `a + b` → "5" (params), a reference to the function
/// itself (a global) → success, and an undefined name → success=false with an error
/// string (no hang, no panic). The evaluator reuses the tree-walker on the parked Vm.
#[test]
fn dap_evaluate_in_paused_frame() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let program = temp_program("evaluate", PROGRAM);
    let mut c = DapClient::spawn_inspect(&program);

    // initialize → launch → entry stop.
    let init = c.request("initialize", json!({}));
    c.read_response(init, deadline);
    c.read_event("initialized", deadline);
    let launch = c.request("launch", json!({}));
    c.read_response(launch, deadline);
    c.read_event("stopped", deadline);

    // Breakpoint on the `let s` line (line 2) inside `add`, then configurationDone.
    let sb = c.request(
        "setBreakpoints",
        json!({
            "source": { "path": program.to_string_lossy() },
            "breakpoints": [ { "line": 2 } ],
        }),
    );
    c.read_response(sb, deadline);
    c.read_event("breakpoint", deadline);
    let cd = c.request("configurationDone", json!({}));
    c.read_response(cd, deadline);
    let stopped2 = c.read_event("stopped", deadline);
    assert_eq!(stopped2["body"]["reason"], json!("breakpoint"), "{stopped2}");

    // The top frame id (frame `add`).
    let st = c.request("stackTrace", json!({ "threadId": 1 }));
    let st_resp = c.read_response(st, deadline);
    let frames = st_resp["body"]["stackFrames"].as_array().expect("frames");
    let frame_id = frames[0]["id"].as_i64().expect("frame id");

    // (1) `a + b` over the paused params (2 + 3) → "5".
    let ev = c.request(
        "evaluate",
        json!({ "expression": "a + b", "frameId": frame_id, "context": "watch" }),
    );
    let ev_resp = c.read_response(ev, deadline);
    assert_eq!(ev_resp["success"], json!(true), "evaluate a+b ok: {ev_resp}");
    assert_eq!(ev_resp["body"]["result"], json!("5"), "a + b == 5: {ev_resp}");

    // (2) A reference to the function itself (a module global) → success (renders SOME
    // value; we only assert it resolved without error / hang).
    let ev2 = c.request(
        "evaluate",
        json!({ "expression": "add", "frameId": frame_id, "context": "repl" }),
    );
    let ev2_resp = c.read_response(ev2, deadline);
    assert_eq!(ev2_resp["success"], json!(true), "evaluate global `add` ok: {ev2_resp}");

    // (3) An undefined name → success=false with an error string (no hang, no panic).
    let ev3 = c.request(
        "evaluate",
        json!({ "expression": "no_such_name_here", "frameId": frame_id, "context": "hover" }),
    );
    let ev3_resp = c.read_response(ev3, deadline);
    assert_eq!(
        ev3_resp["success"], json!(false),
        "an undefined name fails gracefully: {ev3_resp}"
    );

    // Continue to completion — the program still runs to its `print(5)` output.
    let cont = c.request("continue", json!({ "threadId": 1 }));
    c.read_response(cont, deadline);
    let output = c.drain_output_until_terminated(deadline);
    assert!(output.contains("5"), "program still produced output `5`: {output:?}");

    c.close_stdin();
    let _ = std::fs::remove_file(&program);
}

/// Compile `as_path` to a sibling `.aso` via `ascript build` (debug info INCLUDED by
/// default unless `strip`), returning the `.aso` path.
fn build_aso(as_path: &std::path::Path, strip: bool) -> std::path::PathBuf {
    let aso = as_path.with_extension("aso");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ascript"));
    cmd.arg("build");
    if strip {
        cmd.arg("--strip");
    }
    let out = cmd
        .arg(as_path)
        .arg("-o")
        .arg(&aso)
        .output()
        .expect("spawn `ascript build`");
    assert!(
        out.status.success(),
        "ascript build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    aso
}

/// Review F1 — a compiled `.aso` with the (default) embedded debug section is debuggable:
/// `run --inspect program.aso` loads it, the embedded source drives line breakpoints, and
/// stackTrace shows the function name + source line. This is the consumer of Task 6's
/// `.aso` debug section.
#[test]
fn dap_inspect_aso_with_debug_info() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let program = temp_program("aso_dbg", PROGRAM);
    let aso = build_aso(&program, false); // debug info included
    let mut c = DapClient::spawn_inspect(&aso);

    let init = c.request("initialize", json!({}));
    c.read_response(init, deadline);
    c.read_event("initialized", deadline);
    let launch = c.request("launch", json!({}));
    c.read_response(launch, deadline);
    let stopped = c.read_event("stopped", deadline);
    assert_eq!(stopped["body"]["reason"], json!("entry"), "stops at entry: {stopped}");

    // A line breakpoint resolves against the EMBEDDED source (the .aso has no .as on disk).
    let sb = c.request(
        "setBreakpoints",
        json!({
            "source": { "path": aso.to_string_lossy() },
            "breakpoints": [ { "line": 2 } ],
        }),
    );
    c.read_response(sb, deadline);
    let bp_evt = c.read_event("breakpoint", deadline);
    assert_eq!(
        bp_evt["body"]["breakpoint"]["verified"], json!(true),
        "the line binds via the embedded debug section: {bp_evt}"
    );

    let cd = c.request("configurationDone", json!({}));
    c.read_response(cd, deadline);
    let stopped2 = c.read_event("stopped", deadline);
    assert_eq!(stopped2["body"]["reason"], json!("breakpoint"), "{stopped2}");

    let st = c.request("stackTrace", json!({ "threadId": 1 }));
    let st_resp = c.read_response(st, deadline);
    let frames = st_resp["body"]["stackFrames"].as_array().expect("frames");
    assert_eq!(frames[0]["name"], json!("add"), "function name from .aso debug info: {st_resp}");
    assert_eq!(frames[0]["line"], json!(2), "source line from .aso debug info: {st_resp}");

    let cont = c.request("continue", json!({ "threadId": 1 }));
    c.read_response(cont, deadline);
    let output = c.drain_output_until_terminated(deadline);
    assert!(output.contains("5"), "program output: {output:?}");

    c.close_stdin();
    let _ = std::fs::remove_file(&program);
    let _ = std::fs::remove_file(&aso);
}

/// Review F2 — an `evaluate` issued while the VM is NOT parked (between a `continue` and
/// the next stop) is rejected with `success:false` rather than dispatched to a non-parked
/// VM whose reply would never arrive (which would dangle the request forever).
#[test]
fn dap_evaluate_while_running_is_rejected() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let program = temp_program("eval_running", PROGRAM);
    let mut c = DapClient::spawn_inspect(&program);

    let init = c.request("initialize", json!({}));
    c.read_response(init, deadline);
    c.read_event("initialized", deadline);
    let launch = c.request("launch", json!({}));
    c.read_response(launch, deadline);
    c.read_event("stopped", deadline); // entry

    // No breakpoints — `continue` resumes to completion. `is_stopped` is cleared the
    // moment the continue is handled, so the following `evaluate` sees a not-parked VM.
    let cont = c.request("continue", json!({ "threadId": 1 }));
    c.read_response(cont, deadline);
    let ev = c.request("evaluate", json!({ "expression": "1 + 1", "context": "repl" }));
    let ev_resp = c.read_response(ev, deadline);
    assert_eq!(
        ev_resp["success"], json!(false),
        "evaluate while running is rejected, not dangled: {ev_resp}"
    );

    let output = c.drain_output_until_terminated(deadline);
    assert!(output.contains("5"), "program still completes: {output:?}");

    c.close_stdin();
    let _ = std::fs::remove_file(&program);
}

/// Test 1b (review F1 regression) — an UNBINDABLE breakpoint (a line past EOF) must
/// report `verified:false` via the authoritative `breakpoint` event. The pre-fix code
/// fabricated `verified:true` for ANY line when parked, including lines that never bind
/// or fire. This guards against that false-positive.
#[test]
fn dap_unbindable_breakpoint_reports_unverified() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let program = temp_program("unbindable", PROGRAM); // 5 lines
    let mut c = DapClient::spawn_inspect(&program);

    let init = c.request("initialize", json!({}));
    c.read_response(init, deadline);
    c.read_event("initialized", deadline);
    let launch = c.request("launch", json!({}));
    c.read_response(launch, deadline);
    c.read_event("stopped", deadline); // entry

    // A breakpoint on line 99 (well past the 5-line program) cannot bind.
    let sb = c.request(
        "setBreakpoints",
        json!({
            "source": { "path": program.to_string_lossy() },
            "breakpoints": [ { "line": 99 } ],
        }),
    );
    let sb_resp = c.read_response(sb, deadline);
    let bps = sb_resp["body"]["breakpoints"].as_array().expect("breakpoints array");
    assert_eq!(bps[0]["verified"], json!(false), "pending in the response: {sb_resp}");
    let bp_id = bps[0]["id"].as_i64().expect("breakpoint id");

    // The authoritative verdict must stay FALSE — the line never binds (no fabrication).
    let bp_evt = c.read_event("breakpoint", deadline);
    assert_eq!(bp_evt["body"]["breakpoint"]["id"], json!(bp_id));
    assert_eq!(
        bp_evt["body"]["breakpoint"]["verified"], json!(false),
        "an unbindable line must NOT be reported verified: {bp_evt}"
    );

    // configurationDone resumes; with no bound breakpoint the program runs to completion.
    let cd = c.request("configurationDone", json!({}));
    c.read_response(cd, deadline);
    let output = c.drain_output_until_terminated(deadline);
    assert!(output.contains("5"), "program still completes: {output:?}");

    c.close_stdin();
    let _ = std::fs::remove_file(&program);
}

/// Test 1c (review F2 / Gate-0 regression) — `--inspect` must honor the CLI capability
/// flags. Under `--sandbox` a gated `fs.read` is DENIED with the recoverable
/// `capability 'fs' denied` panic exactly as a normal `--sandbox` run, proving the
/// composed CapSet is threaded into the debuggee (the pre-fix `--inspect` returned
/// before `compose_caps` and ran all-granted). The denial rides DAP `output` events.
#[cfg(feature = "sys")]
#[test]
fn dap_inspect_honors_sandbox_capabilities() {
    let deadline = Instant::now() + Duration::from_secs(60);
    // recover() keeps the denial recoverable so the program completes and prints it.
    let src = "import * as fs from \"std/fs\"\n\
               let r = recover(() => fs.read(\"/etc/hosts\"))\n\
               if (r[1] != nil) { print(r[1].message) } else { print(\"read ok\") }\n";
    let program = temp_program("sandbox", src);
    let mut c = DapClient::spawn_inspect_with_flags(&program, &["--sandbox"]);

    let init = c.request("initialize", json!({}));
    c.read_response(init, deadline);
    c.read_event("initialized", deadline);
    let launch = c.request("launch", json!({}));
    c.read_response(launch, deadline);
    c.read_event("stopped", deadline); // entry
    let cd = c.request("configurationDone", json!({}));
    c.read_response(cd, deadline);

    let output = c.drain_output_until_terminated(deadline);
    assert!(
        output.contains("capability 'fs' denied"),
        "under --inspect --sandbox, fs.read must be denied (not all-granted); got: {output:?}"
    );

    c.close_stdin();
    let _ = std::fs::remove_file(&program);
}

/// Test 2 — Gate 9 (observation contract): the program's OBSERVABLE output is
/// byte-identical run normally (`run <file>`) vs under `--inspect` with a breakpoint
/// set-then-immediately-continued. Under `--inspect`, program output rides DAP `output`
/// events; we reconstruct it and compare to the plain run's stdout.
#[test]
fn dap_observation_contract_output_byte_identical() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let program = temp_program("observe", PROGRAM);

    // (a) Plain run — capture child stdout.
    let plain = Command::new(env!("CARGO_BIN_EXE_ascript"))
        .arg("run")
        .arg(&program)
        .output()
        .expect("plain run");
    let plain_out = String::from_utf8_lossy(&plain.stdout).to_string();
    assert!(plain.status.success(), "plain run succeeded");

    // (b) Under --inspect: launch → entry → setBreakpoints(line 2) → configurationDone →
    // stop → continue → reconstruct output from `output` events.
    let mut c = DapClient::spawn_inspect(&program);
    let init = c.request("initialize", json!({}));
    c.read_response(init, deadline);
    c.read_event("initialized", deadline);
    let launch = c.request("launch", json!({}));
    c.read_response(launch, deadline);
    c.read_event("stopped", deadline); // entry
    let sb = c.request(
        "setBreakpoints",
        json!({
            "source": { "path": program.to_string_lossy() },
            "breakpoints": [ { "line": 2 } ],
        }),
    );
    c.read_response(sb, deadline);
    let cd = c.request("configurationDone", json!({}));
    c.read_response(cd, deadline);
    c.read_event("stopped", deadline); // the breakpoint
    let cont = c.request("continue", json!({ "threadId": 1 }));
    c.read_response(cont, deadline);
    let inspect_out = c.drain_output_until_terminated(deadline);
    c.close_stdin();

    assert_eq!(
        inspect_out, plain_out,
        "observable output is byte-identical under --inspect vs plain run"
    );
    let _ = std::fs::remove_file(&program);
}

/// Test 3 — `ascript dap` (no pre-set program): the program comes from the `launch`
/// request's `program` argument. A focused smoke test that the `dap` subcommand path
/// also reaches a stop and terminates.
#[test]
fn dap_subcommand_launch_with_program_arg() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let program = temp_program("subcmd", PROGRAM);

    let mut child = Command::new(env!("CARGO_BIN_EXE_ascript"))
        .arg("dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `ascript dap`");
    let mut c = DapClient::wrap(
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
        child,
    );

    let init = c.request("initialize", json!({}));
    c.read_response(init, deadline);
    c.read_event("initialized", deadline);

    // launch carrying the program path explicitly.
    let launch = c.request("launch", json!({ "program": program.to_string_lossy() }));
    c.read_response(launch, deadline);
    let stopped = c.read_event("stopped", deadline);
    assert_eq!(stopped["body"]["reason"], json!("entry"));

    // No breakpoints — configurationDone resumes to completion.
    let cd = c.request("configurationDone", json!({}));
    c.read_response(cd, deadline);
    let output = c.drain_output_until_terminated(deadline);
    assert!(output.contains("5"), "got output: {output:?}");

    c.close_stdin();
    let _ = std::fs::remove_file(&program);
}
