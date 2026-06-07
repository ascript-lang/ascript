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
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// What the background reader thread yields for each frame it pulls off stdout.
enum ReadItem {
    /// A fully-framed, parsed JSON-RPC message.
    Msg(Value),
    /// The stream hit EOF before a full message (e.g. the child died).
    Eof,
}

/// A minimal LSP client driving the spawned server over its stdio.
///
/// Reads are performed on a dedicated background thread that pushes each framed
/// message onto an `mpsc` channel; the test thread pulls with `recv_timeout` bounded
/// by the per-test `deadline`. This is what makes the deadline *real*: a slow or
/// missing response fails with a clear, deterministic panic at the deadline instead
/// of blocking indefinitely inside a `read_line`/`read_exact` (which ignore the
/// wall-clock check that only ran *between* reads).
struct LspClient {
    child: Child,
    // `Option` so we can close stdin (drop it) to send EOF after `exit`; tower-lsp's
    // serve loop ends on stdin EOF, which is what actually drives a clean exit.
    stdin: Option<ChildStdin>,
    rx: Receiver<ReadItem>,
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

        // Background reader: frame messages off stdout and forward them. It exits when
        // the stream EOFs (it sends one `Eof` then stops) or when the receiver is
        // dropped (the `send` fails). It never blocks the test thread.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = stdout;
            loop {
                match read_framed_message(&mut stdout) {
                    Some(msg) => {
                        if tx.send(ReadItem::Msg(msg)).is_err() {
                            return; // client gone
                        }
                    }
                    None => {
                        let _ = tx.send(ReadItem::Eof);
                        return;
                    }
                }
            }
        });

        LspClient { child, stdin, rx }
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

    /// Pull the next framed message from the reader thread, honoring `deadline`.
    ///
    /// Returns `None` on EOF (the child closed stdout). Panics — with a clear,
    /// deterministic message plus the child's stderr — if no message arrives before
    /// `deadline`, so a genuinely hung server fails cleanly instead of blocking
    /// forever inside a blocking read.
    fn next_message(&mut self, deadline: Instant, waiting_for: &str) -> Option<Value> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!(
                    "timed out waiting for {waiting_for}{}",
                    self.drain_stderr()
                );
            }
            // Cap each wait so we re-check the deadline promptly even if the channel
            // never produces; `recv_timeout` itself does the blocking wait.
            let step = remaining.min(Duration::from_millis(250));
            match self.rx.recv_timeout(step) {
                Ok(ReadItem::Msg(msg)) => return Some(msg),
                Ok(ReadItem::Eof) => return None,
                Err(RecvTimeoutError::Timeout) => continue, // re-check deadline
                Err(RecvTimeoutError::Disconnected) => return None, // reader gone
            }
        }
    }

    /// Read messages until one with the given `id` (a response) arrives, skipping
    /// any notifications (which have no `id`) that interleave. Bounded by `deadline`.
    fn read_response(&mut self, id: i64, deadline: Instant) -> Value {
        let waiting = format!("response id={id}");
        loop {
            let msg = self.next_message(deadline, &waiting).unwrap_or_else(|| {
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
        let waiting = format!("`{method}` notification");
        loop {
            let msg = self.next_message(deadline, &waiting).unwrap_or_else(|| {
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

/// Read exactly one `Content-Length`-framed JSON-RPC message and parse it. Runs on
/// the background reader thread (blocking reads are fine there — the test thread is
/// never blocked). Returns `None` if the stream hits EOF before a full message.
fn read_framed_message(stdout: &mut BufReader<ChildStdout>) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    // Read header lines until the blank separator line.
    loop {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).ok()?;
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
    stdout.read_exact(&mut body).ok()?;
    Some(serde_json::from_slice(&body).expect("parse JSON body"))
}

#[test]
fn lsp_protocol_end_to_end() {
    let overall = Instant::now() + Duration::from_secs(90);
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
    // Phase 4: documentColor / colorPresentation.
    assert!(
        !caps["colorProvider"].is_null(),
        "missing colorProvider: {resp}"
    );
    // Phase 4: codeLens (resolve advertised).
    assert!(
        !caps["codeLensProvider"].is_null(),
        "missing codeLensProvider: {resp}"
    );
    assert_eq!(
        caps["codeLensProvider"]["resolveProvider"], true,
        "codeLens resolve advertised: {resp}"
    );
    // Phase 4: linkedEditingRange.
    assert!(
        !caps["linkedEditingRangeProvider"].is_null(),
        "missing linkedEditingRangeProvider: {resp}"
    );
    // Phase 4: pull diagnostics.
    assert!(
        !caps["diagnosticProvider"].is_null(),
        "missing diagnosticProvider: {resp}"
    );
    // Phase 4: workspace file-operations (willRenameFiles) + multi-root folders.
    assert!(
        !caps["workspace"]["fileOperations"]["willRename"].is_null(),
        "missing workspace.fileOperations.willRename: {resp}"
    );
    assert_eq!(
        caps["workspace"]["workspaceFolders"]["supported"], true,
        "multi-root workspace folders supported: {resp}"
    );
    // executeCommandProvider lists ONLY server-executed commands (ascript.fixAll). The
    // client-owned codeLens commands (ascript.run/runTest) MUST NOT appear here, or the
    // client's auto-registration collides with the editor extension ("command already exists").
    {
        let cmds = caps["executeCommandProvider"]["commands"]
            .as_array()
            .expect("executeCommand commands");
        let cmd_strs: Vec<&str> = cmds.iter().filter_map(|c| c.as_str()).collect();
        assert!(
            cmd_strs.contains(&"ascript.fixAll"),
            "executeCommand should advertise ascript.fixAll: {cmd_strs:?}"
        );
        assert!(
            !cmd_strs.contains(&"ascript.run") && !cmd_strs.contains(&"ascript.runTest"),
            "executeCommand must NOT advertise client-owned run commands: {cmd_strs:?}"
        );
    }
    // signatureHelp trigger chars `(` and `,`.
    let sig_triggers = caps["signatureHelpProvider"]["triggerCharacters"]
        .as_array()
        .expect("signatureHelp triggerCharacters");
    let trigger_strs: Vec<&str> = sig_triggers.iter().filter_map(|t| t.as_str()).collect();
    assert!(
        trigger_strs.contains(&"(") && trigger_strs.contains(&","),
        "signatureHelp trigger chars: {trigger_strs:?}"
    );

    // Phase 3 capabilities: navigation + structure depth.
    assert!(
        !caps["declarationProvider"].is_null(),
        "missing declarationProvider: {resp}"
    );
    assert!(
        !caps["typeDefinitionProvider"].is_null(),
        "missing typeDefinitionProvider: {resp}"
    );
    assert!(
        !caps["implementationProvider"].is_null(),
        "missing implementationProvider: {resp}"
    );
    assert!(
        !caps["foldingRangeProvider"].is_null(),
        "missing foldingRangeProvider: {resp}"
    );
    assert!(
        !caps["selectionRangeProvider"].is_null(),
        "missing selectionRangeProvider: {resp}"
    );
    assert!(
        !caps["documentLinkProvider"].is_null(),
        "missing documentLinkProvider: {resp}"
    );
    assert!(
        !caps["callHierarchyProvider"].is_null(),
        "missing callHierarchyProvider: {resp}"
    );
    // type hierarchy is advertised via the experimental escape hatch (lsp-types 0.94
    // has no standard `typeHierarchyProvider` capability field).
    assert_eq!(
        caps["experimental"]["typeHierarchyProvider"], true,
        "missing experimental.typeHierarchyProvider: {resp}"
    );
    // workspaceSymbol advertises lazy resolve.
    assert_eq!(
        caps["workspaceSymbolProvider"]["resolveProvider"], true,
        "missing workspaceSymbol resolveProvider: {resp}"
    );

    // Phase 7: the FULL advertised capability set. This list is the source-of-truth
    // mirror of `server_capabilities()` in `src/lsp/server.rs` — every standard
    // provider field it sets to `Some(..)` must appear here (type hierarchy is the one
    // exception: it rides the `experimental` escape hatch, asserted above). If you add
    // or remove a capability there, update this list (and the LSP capability docs).
    for cap in [
        "textDocumentSync",
        "completionProvider",
        "hoverProvider",
        "definitionProvider",
        "declarationProvider",
        "typeDefinitionProvider",
        "implementationProvider",
        "documentSymbolProvider",
        "referencesProvider",
        "renameProvider",
        "workspaceSymbolProvider",
        "documentHighlightProvider",
        "foldingRangeProvider",
        "selectionRangeProvider",
        "documentLinkProvider",
        "signatureHelpProvider",
        "semanticTokensProvider",
        "inlayHintProvider",
        "codeActionProvider",
        "codeLensProvider",
        "executeCommandProvider",
        "documentFormattingProvider",
        "documentRangeFormattingProvider",
        "colorProvider",
        "linkedEditingRangeProvider",
        "diagnosticProvider",
        "callHierarchyProvider",
        "workspace",
    ] {
        assert!(
            !caps[cap].is_null(),
            "missing advertised capability `{cap}`: {resp}"
        );
    }

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

    // Phase 3 end-to-end: foldingRange on the symbols doc -> >=1 fold (the
    // multi-line class/fn bodies).
    client.request(
        10,
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": sym_uri } }),
    );
    let fold_resp = client.read_response(10, overall);
    let folds = fold_resp["result"]
        .as_array()
        .expect("foldingRange array result");
    assert!(!folds.is_empty(), "expected >=1 fold: {fold_resp}");

    // prepareTypeHierarchy on `Point` (line 1, char 6) -> a CLASS item. (This uses
    // the in-memory model, not the path index, so it works for any document URI.
    // prepareCallHierarchy is index-backed and exercised in the file-based
    // cross-file test below, where real `file://` paths resolve.)
    client.request(
        12,
        "textDocument/prepareTypeHierarchy",
        json!({
            "textDocument": { "uri": sym_uri },
            "position": { "line": 1, "character": 6 }
        }),
    );
    let th_resp = client.read_response(12, overall);
    let th_items = th_resp["result"]
        .as_array()
        .expect("prepareTypeHierarchy array result");
    assert!(
        th_items.iter().any(|i| i["name"].as_str() == Some("Point")),
        "expected a `Point` type-hierarchy item: {th_resp}"
    );

    // selectionRange at the `name` use (line 0, char 24) -> a non-null chain.
    client.request(
        13,
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": sym_uri },
            "positions": [{ "line": 0, "character": 24 }]
        }),
    );
    let sel_resp = client.read_response(13, overall);
    let sels = sel_resp["result"]
        .as_array()
        .expect("selectionRange array result");
    assert!(!sels.is_empty(), "expected a selection range: {sel_resp}");
    assert!(
        !sels[0]["range"].is_null(),
        "selection range has a range: {sel_resp}"
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

    let overall = Instant::now() + Duration::from_secs(90);
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

    // prepareCallHierarchy on `f`'s decl in a.as (line 0 char 10) -> a FUNCTION item,
    // then its incoming calls include b.as (where `f` is called).
    client.request(
        5,
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": a_uri },
            "position": { "line": 0, "character": 10 }
        }),
    );
    let ch = client.read_response(5, overall);
    let ch_items = ch["result"].as_array().expect("call-hierarchy items");
    let item = ch_items
        .iter()
        .find(|i| i["name"].as_str() == Some("f"))
        .unwrap_or_else(|| panic!("expected an `f` call-hierarchy item: {ch}"))
        .clone();

    client.request(6, "callHierarchy/incomingCalls", json!({ "item": item }));
    let inc = client.read_response(6, overall);
    let calls = inc["result"].as_array().expect("incomingCalls array");
    assert!(
        calls.iter().any(|c| c["from"]["uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("b.as"))),
        "incoming calls should include b.as: {inc}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    let _ = client.wait_for_exit(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Phase 7: exercise one representative request per provider family that the main
/// smoke test does not already hit end-to-end (documentLink, formatting, range
/// formatting, codeAction, codeLens, documentColor, linkedEditingRange, declaration,
/// typeDefinition, implementation, and pull diagnostics). Each must return a
/// well-formed response (a `result` member — `null`/empty is acceptable) and none may
/// deadlock within the overall deadline.
#[test]
fn lsp_full_capability_surface() {
    let overall = Instant::now() + Duration::from_secs(90);
    let mut client = LspClient::spawn();

    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    // A content-rich document: an import (documentLink), a class (typeDefinition /
    // implementation), a color literal (documentColor), and a local identifier with
    // multiple occurrences (linkedEditingRange).
    let uri = "ascript-test://surface.as";
    let text = "import { rgb } from \"std/color\"\n\
class Animal {\n  name: string\n}\n\
class Dog {\n  name: string\n}\n\
fn paint() {\n  let c = rgb(10, 20, 30)\n  return c\n}\n\
let count = 1\ncount = count + 1\n";
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text } }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Each tuple: (id, method, params). Every response must carry a `result` member.
    let line_count = text.lines().count() as i64;
    let whole_range = json!({
        "start": { "line": 0, "character": 0 },
        "end": { "line": line_count, "character": 0 }
    });
    let requests: Vec<(i64, &str, Value)> = vec![
        (
            10,
            "textDocument/documentLink",
            json!({ "textDocument": { "uri": uri } }),
        ),
        (
            11,
            "textDocument/formatting",
            json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }),
        ),
        (
            12,
            "textDocument/rangeFormatting",
            json!({ "textDocument": { "uri": uri }, "range": whole_range,
                    "options": { "tabSize": 2, "insertSpaces": true } }),
        ),
        (
            13,
            "textDocument/codeAction",
            json!({ "textDocument": { "uri": uri }, "range": whole_range,
                    "context": { "diagnostics": [] } }),
        ),
        (
            14,
            "textDocument/codeLens",
            json!({ "textDocument": { "uri": uri } }),
        ),
        (
            15,
            "textDocument/documentColor",
            json!({ "textDocument": { "uri": uri } }),
        ),
        (
            16,
            "textDocument/linkedEditingRange",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 11, "character": 4 } }),
        ),
        (
            17,
            "textDocument/declaration",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 8, "character": 10 } }),
        ),
        (
            18,
            "textDocument/typeDefinition",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 8, "character": 6 } }),
        ),
        (
            19,
            "textDocument/implementation",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 1, "character": 6 } }),
        ),
        (
            20,
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
        ),
        (
            21,
            "textDocument/selectionRange",
            json!({ "textDocument": { "uri": uri }, "positions": [{ "line": 8, "character": 6 }] }),
        ),
    ];

    for (id, method, params) in requests {
        client.request(id, method, params);
        let r = client.read_response(id, overall);
        assert!(
            r.get("result").is_some() && r.get("error").is_none(),
            "`{method}` malformed (missing result or carried an error): {r}"
        );
    }

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    assert!(
        client.wait_for_exit(Duration::from_secs(10)),
        "server did not exit cleanly"
    );
}

/// Phase 7: a ~300 KiB file must not hang the server. The large-file degradation path
/// (semantic tokens range-only, inlay skipped) means these may return null/empty, but
/// they MUST return a well-formed response within the overall deadline.
#[test]
fn lsp_large_file_does_not_hang() {
    // This is a NON-HANG test, not a latency test: it asserts the server eventually
    // returns a well-formed response via the large-file degradation path. Building a
    // ~300 KiB `SemanticModel` in a debug binary under heavy concurrent compile/clippy
    // load can legitimately take tens of seconds, so the deadline is generous — large
    // enough to never trip on a merely-slow (but progressing) server, while still
    // bounded so a *true* infinite hang fails cleanly (the deadline-honoring reads turn
    // it into a deterministic panic rather than a blocked process).
    let overall = Instant::now() + Duration::from_secs(120);
    let mut client = LspClient::spawn();

    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    // Build a >256 KiB document from repeated small function units so the parser /
    // resolver / checker do real work (not one giant token).
    let mut big = String::new();
    let mut i = 0usize;
    while big.len() < 300 * 1024 {
        big.push_str(&format!(
            "fn f_{i}(a, b) {{\n  let s = a + b\n  return s * 2\n}}\n"
        ));
        i += 1;
    }
    let uri = "ascript-test://big.as";
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": big } }),
    );
    // Diagnostics still run (always) — drain the publish.
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // semanticTokens/full: degraded to null on a large file, but well-formed + prompt.
    client.request(
        30,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let st = client.read_response(30, overall);
    assert!(
        st.get("result").is_some() && st.get("error").is_none(),
        "large-file semanticTokens/full hung or malformed: {st}"
    );

    // inlayHint: skipped (empty) on a large file, but well-formed + prompt.
    client.request(
        31,
        "textDocument/inlayHint",
        json!({ "textDocument": { "uri": uri },
                "range": { "start": { "line": 0, "character": 0 },
                           "end": { "line": 10, "character": 0 } } }),
    );
    let ih = client.read_response(31, overall);
    assert!(
        ih.get("result").is_some() && ih.get("error").is_none(),
        "large-file inlayHint hung or malformed: {ih}"
    );

    // The range variant of semantic tokens stays served (bounded) — sanity check it
    // still answers well-formed.
    client.request(
        32,
        "textDocument/semanticTokens/range",
        json!({ "textDocument": { "uri": uri },
                "range": { "start": { "line": 0, "character": 0 },
                           "end": { "line": 8, "character": 0 } } }),
    );
    let str_ = client.read_response(32, overall);
    assert!(
        str_.get("result").is_some() && str_.get("error").is_none(),
        "large-file semanticTokens/range malformed: {str_}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    assert!(
        client.wait_for_exit(Duration::from_secs(10)),
        "server did not exit cleanly"
    );
}

/// Phase 7 consistency invariant: the set of diagnostic codes the LSP publishes for a
/// file equals the set `ascript check --json` emits for the SAME file under the SAME
/// `ascript.toml [lint]` config. Both paths run `analyze_with_config` over the nearest
/// config, so they must agree; this proves it end-to-end over the real binary.
#[test]
fn lsp_diagnostics_match_ascript_check() {
    let dir =
        std::env::temp_dir().join(format!("ascript_lsp_consistency_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A config that turns an otherwise-default lint on, plus a file that trips it AND a
    // hard syntax/semantic issue, so the code set is non-trivial.
    std::fs::write(
        dir.join("ascript.toml"),
        "[lint]\nunused-binding = \"warn\"\n",
    )
    .unwrap();
    let f = dir.join("m.as");
    std::fs::write(&f, "fn main() {\n  let unused = 1\n}\n").unwrap();
    let uri = format!("file://{}", f.display());
    let root_uri = format!("file://{}", dir.display());

    // (a) LSP diagnostics for the file, as a set of `code` strings.
    let overall = Instant::now() + Duration::from_secs(90);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": root_uri, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));
    let file_text = std::fs::read_to_string(&f).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": file_text } }),
    );
    let note = client.read_notification("textDocument/publishDiagnostics", overall);
    let mut lsp_codes: Vec<String> = note["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .filter_map(|d| d["code"].as_str().map(|s| s.to_string()))
        .collect();
    lsp_codes.sort();
    lsp_codes.dedup();

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));

    // (b) `ascript check --json` on the same file, in the same dir (so it resolves the
    // same `ascript.toml`). Output is a JSON array of { code, ... } objects.
    let out = Command::new(env!("CARGO_BIN_EXE_ascript"))
        .arg("check")
        .arg("--json")
        .arg(&f)
        .current_dir(&dir)
        .output()
        .expect("run `ascript check --json`");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("check --json output not JSON ({e}): {stdout}"));
    let mut check_codes: Vec<String> = parsed
        .as_array()
        .expect("check --json is a JSON array")
        .iter()
        .filter_map(|d| d["code"].as_str().map(|s| s.to_string()))
        .collect();
    check_codes.sort();
    check_codes.dedup();

    // (c) The two code SETS must be equal (order-independent).
    assert_eq!(
        lsp_codes, check_codes,
        "LSP diagnostics ({lsp_codes:?}) must match `ascript check` ({check_codes:?}) \
         for the same source + config"
    );
    // Sanity: the corpus is non-trivial (the unused-binding lint did fire).
    assert!(
        check_codes.contains(&"unused-binding".to_string()),
        "expected the unused-binding lint to fire: {check_codes:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Task 12: worker fn LSP tests ──────────────────────────────────────────────

/// Task 12 Step 1a: the `worker` contextual keyword is emitted as a semantic
/// token of type KEYWORD (legend index 0). The first token in
/// `worker fn f() { return 1 }` is `worker` at line 0, char 0, length 6,
/// token_type 0 (KEYWORD).
#[test]
fn lsp_worker_is_keyword_token() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://worker_kw.as";
    let text = "worker fn f() { return 1 }\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "ascript",
                "version": 1,
                "text": text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    client.request(
        2,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let st_resp = client.read_response(2, overall);
    let raw = st_resp["result"]["data"]
        .as_array()
        .expect("semanticTokens data array");
    // The wire format is groups of 5 integers: [delta_line, delta_start, length,
    // token_type, token_modifiers_bitset].
    // KEYWORD = legend index 0; `worker` is the FIRST token (delta_line=0,
    // delta_start=0, length=6, token_type=0).
    let nums: Vec<u32> = raw.iter().filter_map(|v| v.as_u64().map(|n| n as u32)).collect();
    // The first token must be `worker` at (line=0, char=0, len=6, type=KEYWORD).
    assert!(
        nums.len() >= 5 && nums[0] == 0 && nums[1] == 0 && nums[2] == 6 && nums[3] == 0,
        "`worker` at position 0 must be the first token (KEYWORD, len 6); raw data: {nums:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// Task 12 Step 1b: `worker` is offered as a completion keyword at a top-level
/// position.
#[test]
fn lsp_offers_worker_completion() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    // An empty document; we request completions at the top level.
    let uri = "ascript-test://worker_comp.as";
    let text = "\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "ascript",
                "version": 1,
                "text": text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    client.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 0 }
        }),
    );
    let comp_resp = client.read_response(2, overall);
    let items = comp_resp["result"]
        .as_array()
        .expect("completion array result");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(
        labels.contains(&"worker"),
        "`worker` must appear in keyword completions; got: {labels:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// Task 12 Step 1c: hovering the name of a `worker fn` declaration (or a call
/// to it) mentions "worker" and "future" in the hover response, reflecting that
/// calls return `future<T>`.
#[test]
fn lsp_hover_worker_fn_mentions_future() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    // `render` is declared as a worker fn; hover its name in the declaration.
    let uri = "ascript-test://worker_hover.as";
    // "worker fn render(s: number): number { return s }"
    // The return-type annotation lets hover_type_at infer future<number> for calls.
    let text = "worker fn render(s: number): number { return s }\nlet x = render(1)\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "ascript",
                "version": 1,
                "text": text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Hover on `render` at the declaration (line 0).
    // "worker fn render" — `render` starts at byte offset 10 = char 10.
    let render_decl_char: u32 = 10;
    client.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": render_decl_char }
        }),
    );
    let hover_resp = client.read_response(2, overall);
    let hover_value = &hover_resp["result"];
    assert!(
        !hover_value.is_null(),
        "hover on `render` (worker fn decl) should not be null: {hover_resp}"
    );
    let content = hover_value["contents"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(
        content.contains("worker"),
        "hover on a worker fn must mention 'worker'; got: {content:?}"
    );

    // Hover over the render(1) CALL on line 1 — the inferred type is future<number>.
    // "let x = render(1)" — `render` is at char 8 on line 1.
    client.request(
        3,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 8 }
        }),
    );
    let call_hover = client.read_response(3, overall);
    // The hover must carry a result with contents mentioning "future".
    let call_content = call_hover["result"]["contents"]["value"]
        .as_str()
        .unwrap_or("");
    assert!(
        call_content.contains("future"),
        "hover on a worker fn CALL should show future<T>; got: {call_content:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// Task 12 Step 1d: a `worker-capture` violation is surfaced as an LSP
/// diagnostic with code `"worker-capture"`.
#[test]
fn lsp_worker_capture_flows_to_diagnostics() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://worker_capture.as";
    // Capturing a mutable `let` binding from an outer scope is a worker-capture error.
    let text = "let c = 0\nworker fn g(n) { return n + c }\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "ascript",
                "version": 1,
                "text": text
            }
        }),
    );
    let note = client.read_notification("textDocument/publishDiagnostics", overall);
    let diags = note["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array");
    let codes: Vec<&str> = diags
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert!(
        codes.contains(&"worker-capture"),
        "expected a `worker-capture` diagnostic; got codes: {codes:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// Task 12 Step 1e: goto-definition on a call to a `worker fn` lands at the
/// declaration, just like a plain `fn` — navigation reuses the existing resolver
/// index.
#[test]
fn lsp_navigation_finds_worker_fn() {
    let dir =
        std::env::temp_dir().join(format!("ascript_lsp_worker_nav_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f_path = dir.join("wf.as");
    // A `worker fn g` declared and called in the same file.
    std::fs::write(
        &f_path,
        "worker fn g() { return 1 }\nlet x = g()\n",
    )
    .unwrap();
    let f_uri = format!("file://{}", f_path.display());
    let root_uri = format!("file://{}", dir.display());

    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": root_uri, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let file_text = std::fs::read_to_string(&f_path).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": f_uri,
                "languageId": "ascript",
                "version": 1,
                "text": file_text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // `g` in `g()` on line 1 (the call). "let x = g()" — `g` is at char 8.
    client.request(
        2,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": f_uri },
            "position": { "line": 1, "character": 8 }
        }),
    );
    let def_resp = client.read_response(2, overall);
    // The result is either a Location or an array; either way it must be non-null
    // and point to the same file (same-file goto-def).
    let result = &def_resp["result"];
    assert!(
        !result.is_null(),
        "goto-def on a worker fn call must not be null: {def_resp}"
    );
    // Normalise: accept a single Location or an array with one element.
    let loc_uri = if result.is_array() {
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1, "expected exactly 1 definition: {def_resp}");
        arr[0]["uri"].as_str().unwrap_or("").to_string()
    } else {
        result["uri"].as_str().unwrap_or("").to_string()
    };
    assert!(
        loc_uri.ends_with("wf.as"),
        "goto-def should point to wf.as; got: {loc_uri}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

// ── Task 9: worker class / actor handle / worker fn* LSP tests ───────────────

/// Task 9 Step 1a: the `worker` keyword before `class` is emitted as a KEYWORD
/// semantic token.  `worker class Db { fn query(): string { return "ok" } }` —
/// `worker` at line 0 char 0, length 6, token_type 0 (KEYWORD).
#[test]
fn lsp_worker_class_is_keyword_token() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://worker_class_kw.as";
    let text = "worker class Db { fn query(): string { return \"ok\" } }\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "ascript",
                "version": 1,
                "text": text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    client.request(
        2,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let st_resp = client.read_response(2, overall);
    let raw = st_resp["result"]["data"]
        .as_array()
        .expect("semanticTokens data array");
    let nums: Vec<u32> = raw.iter().filter_map(|v| v.as_u64().map(|n| n as u32)).collect();
    // The first token must be `worker` at (line=0, char=0, len=6, type=KEYWORD=0).
    assert!(
        nums.len() >= 5 && nums[0] == 0 && nums[1] == 0 && nums[2] == 6 && nums[3] == 0,
        "`worker` before `class` must be the first KEYWORD token (len 6); raw: {nums:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// Task 9 Step 1b: a `worker class` appears in document symbols as CLASS (kind
/// 5), and its methods are listed as METHOD children.
#[test]
fn lsp_worker_class_appears_in_document_symbols() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://worker_class_sym.as";
    let text = "worker class Counter {\n  fn init() {}\n  fn inc(): number { return 1 }\n}\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "ascript",
                "version": 1,
                "text": text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    client.request(
        2,
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let sym_resp = client.read_response(2, overall);
    let symbols = sym_resp["result"]
        .as_array()
        .expect("documentSymbol array result");
    let names: Vec<&str> = symbols.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(
        names.contains(&"Counter"),
        "`Counter` worker class must appear in symbols; got: {names:?}"
    );

    // The `Counter` symbol must have kind CLASS (5).
    let counter = symbols
        .iter()
        .find(|s| s["name"].as_str() == Some("Counter"))
        .expect("Counter in symbols");
    assert_eq!(
        counter["kind"].as_i64(),
        Some(5),
        "Counter worker class must have kind CLASS (5): {counter}"
    );

    // Its methods (`init`, `inc`) must appear as children of kind METHOD (6).
    let children = counter["children"].as_array().expect("Counter children");
    let child_names: Vec<&str> = children.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(
        child_names.contains(&"init") && child_names.contains(&"inc"),
        "worker class methods must appear as children; got: {child_names:?}"
    );
    assert!(
        children.iter().all(|c| c["kind"].as_i64() == Some(6)),
        "worker class method children must be METHOD (6): {children:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// Task 9 Step 1c: hovering the name of a `worker class` mentions "worker" in
/// the hover text.
#[test]
fn lsp_hover_worker_class_mentions_worker() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://worker_class_hover.as";
    // Line 0: "worker class Db { fn query(): string { return "ok" } }"
    // `Db` starts at char 13.
    let text = "worker class Db { fn query(): string { return \"ok\" } }\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "ascript",
                "version": 1,
                "text": text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Hover over `Db` in the declaration (char 13 on line 0).
    client.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 13 }
        }),
    );
    let hover_resp = client.read_response(2, overall);
    assert!(
        !hover_resp["result"].is_null(),
        "hover on worker class name must not be null: {hover_resp}"
    );
    let content = hover_resp["result"]["contents"]["value"]
        .as_str()
        .unwrap_or("");
    assert!(
        content.contains("worker"),
        "hover on a `worker class` must mention 'worker'; got: {content:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// Task 9 Step 1d: goto-definition on `Counter` used in a `Counter.spawn()`
/// call lands at the `worker class` declaration.
#[test]
fn lsp_navigation_finds_worker_class() {
    let dir = std::env::temp_dir()
        .join(format!("ascript_lsp_worker_class_nav_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f_path = dir.join("wc.as");
    // `worker class Counter` declared; its name referenced in an async fn.
    std::fs::write(
        &f_path,
        "worker class Counter { fn inc(): number { return 1 } }\nasync fn main() { let h = await Counter.spawn() }\n",
    )
    .unwrap();
    let f_uri = format!("file://{}", f_path.display());
    let root_uri = format!("file://{}", dir.display());

    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": root_uri, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let file_text = std::fs::read_to_string(&f_path).unwrap();
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": f_uri,
                "languageId": "ascript",
                "version": 1,
                "text": file_text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // `Counter` in `Counter.spawn()` on line 1.
    // "async fn main() { let h = await Counter.spawn() }"
    // `Counter` starts at char 31 on line 1.
    client.request(
        2,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": f_uri },
            "position": { "line": 1, "character": 31 }
        }),
    );
    let def_resp = client.read_response(2, overall);
    let result = &def_resp["result"];
    assert!(
        !result.is_null(),
        "goto-def on a worker class use must not be null: {def_resp}"
    );
    let loc_uri = if result.is_array() {
        let arr = result.as_array().unwrap();
        assert!(!arr.is_empty(), "expected at least 1 definition: {def_resp}");
        arr[0]["uri"].as_str().unwrap_or("").to_string()
    } else {
        result["uri"].as_str().unwrap_or("").to_string()
    };
    assert!(
        loc_uri.ends_with("wc.as"),
        "goto-def should point to wc.as; got: {loc_uri}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Task 9 Step 1e: a `worker fn*` (generator) is emitted as a KEYWORD token
/// (the `worker` token) AND appears in document symbols as a FUNCTION.
#[test]
fn lsp_worker_gen_fn_symbol_and_token() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://worker_gen.as";
    let text = "worker fn* stream(n: number) { for i in 1..=n { yield i } }\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "ascript",
                "version": 1,
                "text": text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Semantic tokens: `worker` (len 6) is a KEYWORD.
    client.request(
        2,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let st_resp = client.read_response(2, overall);
    let raw = st_resp["result"]["data"]
        .as_array()
        .expect("semanticTokens data");
    let nums: Vec<u32> = raw.iter().filter_map(|v| v.as_u64().map(|n| n as u32)).collect();
    // The first token must be `worker` at (line=0, char=0, len=6, type=KEYWORD=0).
    assert!(
        nums.len() >= 5 && nums[0] == 0 && nums[1] == 0 && nums[2] == 6 && nums[3] == 0,
        "`worker` before `fn*` must be the first KEYWORD token (len 6); raw: {nums:?}"
    );

    // Document symbols: `stream` appears as FUNCTION (kind 12).
    client.request(
        3,
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let sym_resp = client.read_response(3, overall);
    let symbols = sym_resp["result"]
        .as_array()
        .expect("documentSymbol array");
    let names: Vec<&str> = symbols.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(
        names.contains(&"stream"),
        "`stream` worker fn* must appear in document symbols; got: {names:?}"
    );
    let stream_sym = symbols
        .iter()
        .find(|s| s["name"].as_str() == Some("stream"))
        .unwrap();
    assert_eq!(
        stream_sym["kind"].as_i64(),
        Some(12),
        "worker fn* must be a FUNCTION symbol (kind 12): {stream_sym}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}
