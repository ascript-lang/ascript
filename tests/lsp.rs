//! End-to-end LSP protocol smoke test.
//!
//! This spawns the real `ascript lsp` binary as a subprocess and speaks LSP
//! JSON-RPC (`Content-Length`-framed messages) over its stdin/stdout, proving the
//! server actually talks the protocol — distinct from the pure-analysis unit tests
//! in `src/lsp/analysis.rs`, which never touch the wire.
//!
//! Gated on the `lsp` feature; under `--no-default-features` the whole file (and
//! the `ascript lsp` subcommand) compiles out, so the file is empty there.

#![cfg(feature = "lsp")]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// A minimal LSP client driving the spawned server over its stdio.
struct LspClient {
    child: Child,
    // `Option` so we can close stdin (drop it) to send EOF after `exit`; tower-lsp's
    // serve loop ends on stdin EOF, which is what actually drives a clean exit.
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl LspClient {
    fn spawn() -> Self {
        // Cargo builds the default-features binary (lsp on) for integration tests.
        let mut child = Command::new(env!("CARGO_BIN_EXE_ascript"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn `ascript lsp`");
        let stdin = Some(child.stdin.take().expect("child stdin"));
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        LspClient { child, stdin, stdout }
    }

    /// Write a single `Content-Length`-framed JSON-RPC message.
    fn send(&mut self, msg: &Value) {
        let body = serde_json::to_vec(msg).expect("serialize message");
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("write header");
        stdin.write_all(&body).expect("write body");
        stdin.flush().expect("flush");
    }

    /// Close the child's stdin (EOF). tower-lsp's serve loop terminates on stdin
    /// EOF, so this is what lets the server actually exit after an `exit` notice.
    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    fn request(&mut self, id: i64, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    /// A request carrying no `params` member (some methods, e.g. `shutdown`,
    /// reject an explicit `null`).
    fn request_no_params(&mut self, id: i64, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method }));
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// A notification carrying no `params` member (e.g. `exit`).
    fn notify_no_params(&mut self, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    /// Read exactly one `Content-Length`-framed JSON-RPC message and parse it.
    ///
    /// Returns `None` if the stream hits EOF before a full message is read (e.g.
    /// the child died — the caller can then surface stderr).
    fn read_message(&mut self) -> Option<Value> {
        let mut content_length: Option<usize> = None;
        // Read header lines until the blank separator line.
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).ok()?;
            if n == 0 {
                return None; // EOF
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break; // end of headers
            }
            if let Some(rest) = trimmed
                .strip_prefix("Content-Length:")
                .or_else(|| trimmed.strip_prefix("content-length:"))
            {
                content_length = Some(rest.trim().parse().expect("parse Content-Length"));
            }
        }
        let len = content_length.expect("message had no Content-Length header");
        let mut body = vec![0u8; len];
        self.stdout.read_exact(&mut body).ok()?;
        Some(serde_json::from_slice(&body).expect("parse JSON body"))
    }

    /// Read messages until one with the given `id` (a response) arrives, skipping
    /// any notifications (which have no `id`) that interleave. Bounded by `deadline`.
    fn read_response(&mut self, id: i64, deadline: Instant) -> Value {
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for response id={id}");
            let msg = self.read_message().unwrap_or_else(|| {
                panic!("server closed stream before response id={id}{}", self.drain_stderr())
            });
            if msg.get("id").and_then(Value::as_i64) == Some(id) {
                return msg;
            }
            // Otherwise it's a notification or a different response — keep reading.
        }
    }

    /// Read messages until a notification with the given `method` arrives. Bounded.
    fn read_notification(&mut self, method: &str, deadline: Instant) -> Value {
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for `{method}` notification");
            let msg = self.read_message().unwrap_or_else(|| {
                panic!("server closed stream before `{method}`{}", self.drain_stderr())
            });
            if msg.get("id").is_none() && msg.get("method").and_then(Value::as_str) == Some(method) {
                return msg;
            }
        }
    }

    /// Best-effort drain of the child's stderr for diagnostics in panic messages.
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

    /// Wait for the child to exit, killing it if it overruns the timeout.
    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return false,
            }
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Never leave a stray server behind if a test panics mid-flight.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn lsp_protocol_end_to_end() {
    let overall = Instant::now() + Duration::from_secs(30);
    let mut client = LspClient::spawn();

    // 1. initialize -> capabilities.
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let resp = client.read_response(1, overall);
    let caps = &resp["result"]["capabilities"];
    assert!(!caps["textDocumentSync"].is_null(), "missing textDocumentSync: {resp}");
    assert!(!caps["completionProvider"].is_null(), "missing completionProvider: {resp}");
    assert!(!caps["hoverProvider"].is_null(), "missing hoverProvider: {resp}");
    assert!(!caps["definitionProvider"].is_null(), "missing definitionProvider: {resp}");
    assert!(!caps["documentSymbolProvider"].is_null(), "missing documentSymbolProvider: {resp}");

    // 2. initialized notification.
    client.notify("initialized", json!({}));

    // 3. didOpen a doc with a parse error -> publishDiagnostics with an error.
    let uri = "ascript-test://t.as";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "ascript",
                "version": 1,
                "text": "let = 5\n"
            }
        }),
    );
    let note = client.read_notification("textDocument/publishDiagnostics", overall);
    assert_eq!(note["params"]["uri"], uri, "diagnostics for the wrong uri: {note}");
    let diags = note["params"]["diagnostics"].as_array().expect("diagnostics array");
    assert!(!diags.is_empty(), "expected >=1 diagnostic for a parse error: {note}");
    let first = &diags[0];
    assert_eq!(first["severity"].as_i64(), Some(1), "expected Error severity (1): {first}");
    assert!(
        first["message"].as_str().is_some_and(|m| !m.is_empty()),
        "diagnostic should carry a message: {first}"
    );

    // 4. documentSymbol on a doc with a fn + class -> the symbols are listed.
    let sym_uri = "ascript-test://sym.as";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": sym_uri,
                "languageId": "ascript",
                "version": 1,
                "text": "fn greet(name) { return name }\nclass Point { fn init() {} }\n"
            }
        }),
    );
    // Drain its (empty) diagnostics notification so it doesn't confuse later reads.
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    client.request(
        2,
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": sym_uri } }),
    );
    let sym_resp = client.read_response(2, overall);
    let symbols = sym_resp["result"].as_array().expect("documentSymbol array result");
    let names: Vec<&str> =
        symbols.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"greet"), "expected `greet` in symbols: {names:?}");
    assert!(names.contains(&"Point"), "expected `Point` in symbols: {names:?}");

    // 5. hover over the `greet` identifier (line 0, char 3) -> a sensible result.
    client.request(
        3,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": sym_uri },
            "position": { "line": 0, "character": 3 }
        }),
    );
    let hover_resp = client.read_response(3, overall);
    // Hover may be null for some positions, but it must be a well-formed response.
    assert!(hover_resp.get("result").is_some(), "hover response missing result: {hover_resp}");

    // 6. shutdown -> result; exit -> clean exit.
    client.request_no_params(4, "shutdown");
    let shutdown_resp = client.read_response(4, overall);
    assert!(
        shutdown_resp.get("result").is_some() && shutdown_resp.get("error").is_none(),
        "shutdown should succeed: {shutdown_resp}"
    );
    client.notify_no_params("exit");
    client.close_stdin();

    assert!(
        client.wait_for_exit(Duration::from_secs(10)),
        "server did not exit cleanly after `exit`"
    );
}
