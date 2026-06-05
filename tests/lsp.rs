//! End-to-end LSP protocol smoke test.
//!
//! This spawns the real `ascript lsp` binary as a subprocess and speaks LSP
//! JSON-RPC (`Content-Length`-framed messages) over its stdin/stdout, proving the
//! server actually talks the protocol — distinct from the pure-analysis unit tests
//! in `src/lsp/providers/` and `src/lsp/model.rs`, which never touch the wire.
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
        LspClient {
            child,
            stdin,
            stdout,
        }
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
            assert!(
                Instant::now() < deadline,
                "timed out waiting for response id={id}"
            );
            let msg = self.read_message().unwrap_or_else(|| {
                panic!(
                    "server closed stream before response id={id}{}",
                    self.drain_stderr()
                )
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
            assert!(
                Instant::now() < deadline,
                "timed out waiting for `{method}` notification"
            );
            let msg = self.read_message().unwrap_or_else(|| {
                panic!(
                    "server closed stream before `{method}`{}",
                    self.drain_stderr()
                )
            });
            if msg.get("id").is_none() && msg.get("method").and_then(Value::as_str) == Some(method)
            {
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
    assert!(
        !caps["textDocumentSync"].is_null(),
        "missing textDocumentSync: {resp}"
    );
    assert!(
        !caps["completionProvider"].is_null(),
        "missing completionProvider: {resp}"
    );
    assert!(
        !caps["hoverProvider"].is_null(),
        "missing hoverProvider: {resp}"
    );
    assert!(
        !caps["definitionProvider"].is_null(),
        "missing definitionProvider: {resp}"
    );
    assert!(
        !caps["documentSymbolProvider"].is_null(),
        "missing documentSymbolProvider: {resp}"
    );
    assert!(
        !caps["documentFormattingProvider"].is_null(),
        "missing documentFormattingProvider: {resp}"
    );
    assert!(
        !caps["documentRangeFormattingProvider"].is_null(),
        "missing documentRangeFormattingProvider: {resp}"
    );
    assert!(
        !caps["codeActionProvider"].is_null(),
        "missing codeActionProvider: {resp}"
    );
    assert!(
        !caps["executeCommandProvider"].is_null(),
        "missing executeCommandProvider: {resp}"
    );
    assert_eq!(
        caps["completionProvider"]["resolveProvider"], true,
        "completion resolve advertised: {resp}"
    );
    // Phase 2 capabilities.
    assert!(
        !caps["semanticTokensProvider"].is_null(),
        "missing semanticTokensProvider: {resp}"
    );
    assert!(
        !caps["documentHighlightProvider"].is_null(),
        "missing documentHighlightProvider: {resp}"
    );
    assert!(
        !caps["signatureHelpProvider"].is_null(),
        "missing signatureHelpProvider: {resp}"
    );
    assert!(
        !caps["inlayHintProvider"].is_null(),
        "missing inlayHintProvider: {resp}"
    );
    // signatureHelp trigger chars `(` and `,`.
    let sig_triggers = caps["signatureHelpProvider"]["triggerCharacters"]
        .as_array()
        .expect("signatureHelp triggerCharacters");
    let trigger_strs: Vec<&str> = sig_triggers.iter().filter_map(|t| t.as_str()).collect();
    assert!(
        trigger_strs.contains(&"(") && trigger_strs.contains(&","),
        "signatureHelp trigger chars: {trigger_strs:?}"
    );

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
    assert_eq!(
        note["params"]["uri"], uri,
        "diagnostics for the wrong uri: {note}"
    );
    let diags = note["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    assert!(
        !diags.is_empty(),
        "expected >=1 diagnostic for a parse error: {note}"
    );
    let first = &diags[0];
    assert_eq!(
        first["severity"].as_i64(),
        Some(1),
        "expected Error severity (1): {first}"
    );
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
                "text": "fn greet(name) { return name }\nclass Point {\n  x: number\n  label: string?\n  fn init() {}\n}\n"
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
    let symbols = sym_resp["result"]
        .as_array()
        .expect("documentSymbol array result");
    let names: Vec<&str> = symbols.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(
        names.contains(&"greet"),
        "expected `greet` in symbols: {names:?}"
    );
    assert!(
        names.contains(&"Point"),
        "expected `Point` in symbols: {names:?}"
    );

    // Declared class fields are emitted as PROPERTY (kind 7) children, before methods.
    let point = symbols
        .iter()
        .find(|s| s["name"].as_str() == Some("Point"))
        .expect("Point class symbol");
    let children = point["children"].as_array().expect("Point children array");
    let child_names: Vec<&str> = children.iter().filter_map(|c| c["name"].as_str()).collect();
    assert_eq!(
        child_names,
        vec!["x", "label", "init"],
        "expected fields before methods: {child_names:?}"
    );
    // `x` and `label` are PROPERTY (SymbolKind::PROPERTY == 7); `init` is METHOD (6).
    assert_eq!(
        children[0]["kind"].as_i64(),
        Some(7),
        "x should be PROPERTY"
    );
    assert_eq!(
        children[1]["kind"].as_i64(),
        Some(7),
        "label should be PROPERTY"
    );
    assert_eq!(
        children[2]["kind"].as_i64(),
        Some(6),
        "init should be METHOD"
    );

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
    assert!(
        hover_resp.get("result").is_some(),
        "hover response missing result: {hover_resp}"
    );

    // 5b. completion on the symbols doc -> the rewritten scope-aware provider
    // offers in-scope bindings (`greet`, `Point`) AND control-flow snippets.
    client.request(
        5,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": sym_uri },
            "position": { "line": 5, "character": 0 }
        }),
    );
    let comp_resp = client.read_response(5, overall);
    let comp_items = comp_resp["result"]
        .as_array()
        .expect("completion array result");
    let comp_labels: Vec<&str> = comp_items
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();
    assert!(
        comp_labels.contains(&"greet") && comp_labels.contains(&"Point"),
        "completion should offer in-scope bindings: {comp_labels:?}"
    );
    assert!(
        comp_labels.contains(&"print") && comp_labels.contains(&"let"),
        "completion should preserve builtins + keywords: {comp_labels:?}"
    );
    // A snippet item (`match`) carries insertTextFormat == Snippet (2).
    let snippet = comp_items
        .iter()
        .find(|i| i["label"].as_str() == Some("match") && i["insertTextFormat"].as_i64() == Some(2))
        .expect("a snippet completion item");
    assert!(
        snippet["insertText"].as_str().is_some_and(|t| t.contains("$")),
        "snippet has a tab-stop: {snippet}"
    );

    // 5c. semanticTokens/full on the symbols doc -> a non-empty token stream.
    client.request(
        6,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": sym_uri } }),
    );
    let st_resp = client.read_response(6, overall);
    let st_data = st_resp["result"]["data"]
        .as_array()
        .expect("semanticTokens data array");
    assert!(
        !st_data.is_empty() && st_data.len().is_multiple_of(5),
        "semantic tokens are 5-int-per-token and non-empty: {}",
        st_data.len()
    );

    // 5d. A doc exercising signatureHelp, inlayHint, documentHighlight.
    let p2_uri = "ascript-test://p2.as";
    let p2_text = "fn add(a, b) { return a + b }\nlet total = 1\ntotal = add(total, 2)\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": p2_uri,
                "languageId": "ascript",
                "version": 1,
                "text": p2_text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // signatureHelp inside `add(` on line 2 (after "total = add(").
    client.request(
        7,
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": p2_uri },
            "position": { "line": 2, "character": 11 }
        }),
    );
    let sig_resp = client.read_response(7, overall);
    let signatures = sig_resp["result"]["signatures"]
        .as_array()
        .expect("signatureHelp signatures array");
    assert!(
        signatures
            .iter()
            .any(|s| s["label"].as_str() == Some("add(a, b)")),
        "expected `add(a, b)` signature: {sig_resp}"
    );

    // inlayHint over the whole p2 doc -> a type hint (`: number` for total) AND
    // parameter-name hints (`a:`, `b:`) at the call args.
    client.request(
        8,
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": p2_uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 3, "character": 0 }
            }
        }),
    );
    let inlay_resp = client.read_response(8, overall);
    let hints = inlay_resp["result"]
        .as_array()
        .expect("inlayHint array result");
    let labels: Vec<String> = hints
        .iter()
        .filter_map(|h| h["label"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        labels.iter().any(|l| l.contains("number")),
        "expected an inferred-type hint mentioning number: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == "a:") && labels.iter().any(|l| l == "b:"),
        "expected parameter-name hints a:/b:: {labels:?}"
    );

    // documentHighlight on `total` (line 1, char 4) -> read + write occurrences.
    client.request(
        9,
        "textDocument/documentHighlight",
        json!({
            "textDocument": { "uri": p2_uri },
            "position": { "line": 1, "character": 4 }
        }),
    );
    let hl_resp = client.read_response(9, overall);
    let highlights = hl_resp["result"]
        .as_array()
        .expect("documentHighlight array result");
    assert!(
        highlights.len() >= 2,
        "expected >=2 occurrences of `total`: {hl_resp}"
    );
    // At least one WRITE (kind 3) occurrence (the reassignment target / decl).
    assert!(
        highlights.iter().any(|h| h["kind"].as_i64() == Some(3)),
        "expected a WRITE highlight: {hl_resp}"
    );

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

#[test]
fn lsp_cross_file_goto_definition_and_rename() {
    // SP4 §4: open two files (a defines + exports `f`, b imports + uses it) under
    // a workspace root; goto-definition on b's use of `f` lands in a.as, and a
    // workspace symbol query + rename span the files.
    let dir = std::env::temp_dir().join(format!("ascript_lsp_xfile_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a_path = dir.join("a.as");
    let b_path = dir.join("b.as");
    std::fs::write(&a_path, "export fn f(x) { return x }\n").unwrap();
    std::fs::write(&b_path, "import { f } from \"./a\"\nprint(f(1))\n").unwrap();
    let a_uri = format!("file://{}", a_path.display());
    let b_uri = format!("file://{}", b_path.display());
    let root_uri = format!("file://{}", dir.display());

    let overall = Instant::now() + Duration::from_secs(30);
    let mut client = LspClient::spawn();

    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": root_uri, "capabilities": {} }),
    );
    let resp = client.read_response(1, overall);
    let caps = &resp["result"]["capabilities"];
    assert!(!caps["referencesProvider"].is_null(), "missing references: {resp}");
    assert!(!caps["renameProvider"].is_null(), "missing rename: {resp}");
    assert!(
        !caps["workspaceSymbolProvider"].is_null(),
        "missing workspace symbols: {resp}"
    );
    client.notify("initialized", json!({}));

    // Open both files so the server has their text (didOpen also reindexes).
    for (uri, text) in [
        (&a_uri, "export fn f(x) { return x }\n"),
        (&b_uri, "import { f } from \"./a\"\nprint(f(1))\n"),
    ] {
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text } }),
        );
        // Drain the diagnostics notification.
        let _ = client.read_notification("textDocument/publishDiagnostics", overall);
    }

    // goto-definition on b's use of `f` (line 1 `print(f(1))`, `f` at char 6).
    client.request(
        2,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": b_uri },
            "position": { "line": 1, "character": 6 }
        }),
    );
    let def = client.read_response(2, overall);
    let loc = &def["result"];
    let def_uri = loc["uri"].as_str().unwrap_or_else(|| loc[0]["uri"].as_str().unwrap_or(""));
    assert!(
        def_uri.ends_with("a.as"),
        "cross-file goto-def should land in a.as, got: {def}"
    );

    // workspace/symbol "f" returns a match.
    client.request(3, "workspace/symbol", json!({ "query": "f" }));
    let syms = client.read_response(3, overall);
    let arr = syms["result"].as_array().expect("symbol array");
    assert!(
        arr.iter().any(|s| s["name"] == "f"),
        "workspace symbol f missing: {syms}"
    );

    // rename `f` (at its decl in a.as line 0 char 10) to `g` → edits in a + b.
    client.request(
        4,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": a_uri },
            "position": { "line": 0, "character": 10 },
            "newName": "g"
        }),
    );
    let ren = client.read_response(4, overall);
    let changes = &ren["result"]["changes"];
    assert!(
        changes.get(&a_uri).is_some() && changes.get(&b_uri).is_some(),
        "rename should edit both a.as and b.as: {ren}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    let _ = client.wait_for_exit(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}
