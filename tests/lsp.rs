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

    // inlayHint over the whole p2 doc -> a type hint (`: int` for total) AND
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
    // NUM §5: `let total = 1` infers the concrete `int` subtype (was `number`).
    assert!(
        labels.iter().any(|l| l.contains("int")),
        "expected an inferred-type hint mentioning int: {labels:?}"
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

/// DX D3 Task 12 — the Task-11 shadowing edge holds END-TO-END through the LSP
/// providers (not just the `WorkspaceIndex` unit tests). File A exports `x`; file B
/// imports `x` AND declares a same-named local `let x` in a nested frame. Renaming
/// A's export must edit B's IMPORTED use + import clause but NEVER B's shadowing
/// local; renaming B's local must stay entirely within its frame. references mirror
/// rename. This proves references/rename match on the unified `GlobalBindingId`, not
/// on name text, all the way out at the wire.
#[test]
fn lsp_cross_file_rename_respects_shadowing_local() {
    let dir = std::env::temp_dir().join(format!("ascript_lsp_shadow_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a_path = dir.join("a.as");
    let b_path = dir.join("b.as");
    let a_text = "export let x = 1\n";
    // line0 import clause `x`@9; line2 local decl `x`@6; line3 local use `x`@9;
    // line5 imported use `x`@6 (module scope — the nested local is out of scope here).
    let b_text = "import { x } from \"./a\"\nfn g() {\n  let x = 2\n  return x\n}\nprint(x)\n";
    std::fs::write(&a_path, a_text).unwrap();
    std::fs::write(&b_path, b_text).unwrap();
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
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));
    for (uri, text) in [(&a_uri, a_text), (&b_uri, b_text)] {
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text } }),
        );
        let _ = client.read_notification("textDocument/publishDiagnostics", overall);
    }

    // Helper: the set of (line, character) start positions edited in a file by a
    // rename `changes` object.
    let edited_lines = |changes: &Value, uri: &str| -> Vec<(i64, i64)> {
        changes
            .get(uri)
            .and_then(|v| v.as_array())
            .map(|edits| {
                edits
                    .iter()
                    .map(|e| {
                        (
                            e["range"]["start"]["line"].as_i64().unwrap_or(-1),
                            e["range"]["start"]["character"].as_i64().unwrap_or(-1),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    // (1) Rename A's export `x` (decl at line 0 char 11) -> `z`.
    client.request(
        2,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": a_uri },
            "position": { "line": 0, "character": 11 },
            "newName": "z"
        }),
    );
    let ren = client.read_response(2, overall);
    let changes = &ren["result"]["changes"];
    let b_edits = edited_lines(changes, &b_uri);
    // B's import clause (line 0) + imported use (line 5) ARE edited.
    assert!(
        b_edits.contains(&(0, 9)),
        "import clause `x` should rename: {ren}"
    );
    assert!(
        b_edits.contains(&(5, 6)),
        "imported use `print(x)` should rename: {ren}"
    );
    // B's SHADOWING local (decl line 2, use line 3) must NOT be touched.
    assert!(
        !b_edits.iter().any(|&(l, _)| l == 2 || l == 3),
        "the shadowing local `let x`/`return x` must NOT be renamed: {b_edits:?} in {ren}"
    );

    // (2) Inverse: rename B's LOCAL `x` (decl at line 2 char 6) -> `w` stays in B's
    // frame — no edit to A, no edit to B's import clause or imported use.
    client.request(
        3,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": b_uri },
            "position": { "line": 2, "character": 6 },
            "newName": "w"
        }),
    );
    let ren2 = client.read_response(3, overall);
    let changes2 = &ren2["result"]["changes"];
    assert!(
        changes2.get(&a_uri).is_none(),
        "renaming B's local must not edit A: {ren2}"
    );
    let b_edits2 = edited_lines(changes2, &b_uri);
    assert!(
        b_edits2.contains(&(2, 6)) && b_edits2.contains(&(3, 9)),
        "the local decl + its use should rename: {b_edits2:?} in {ren2}"
    );
    assert!(
        !b_edits2.iter().any(|&(l, _)| l == 0 || l == 5),
        "the import clause + imported use must NOT be touched by the local rename: {b_edits2:?}"
    );

    // (3) references on A's export `x` excludes B's shadowing local but includes the
    // imported use.
    client.request(
        4,
        "textDocument/references",
        json!({
            "textDocument": { "uri": a_uri },
            "position": { "line": 0, "character": 11 },
            "context": { "includeDeclaration": false }
        }),
    );
    let refs = client.read_response(4, overall);
    let ref_arr = refs["result"].as_array().cloned().unwrap_or_default();
    let b_ref_lines: Vec<i64> = ref_arr
        .iter()
        .filter(|loc| loc["uri"].as_str() == Some(b_uri.as_str()))
        .map(|loc| loc["range"]["start"]["line"].as_i64().unwrap_or(-1))
        .collect();
    assert!(
        b_ref_lines.contains(&5),
        "references should include the imported use on line 5: {refs}"
    );
    assert!(
        !b_ref_lines.contains(&3),
        "references must NOT include the shadowing local use on line 3: {refs}"
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
///
/// The 180 s overall deadline (and 20 s exit budget) is deliberately generous: this
/// test checks LSP capability CORRECTNESS, not latency, and the extra headroom prevents
/// spurious timeouts when many binary-spawning tests run in parallel under a loaded CI
/// machine or during a full `cargo test` run.
#[test]
fn lsp_full_capability_surface() {
    let overall = Instant::now() + Duration::from_secs(180);
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
        client.wait_for_exit(Duration::from_secs(20)),
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

/// IFACE Task 11: semantic tokens for interfaces — the `interface` reserved
/// keyword and the contextual `implements`/`extends` introducers are KEYWORD
/// tokens (legend index 0); the interface NAME at the declaration site is a
/// CLASS-typed token (legend index 5 — interface names color as types). Asserts
/// over the decoded token stream (absolute line/char rebuilt from the deltas).
#[test]
fn lsp_interface_semantic_tokens() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://iface_tokens.as";
    // line 0: `interface Reader { fn read(b: bytes): int }`
    // line 1: `class File implements Reader { fn read(b: bytes): int { return 0 } }`
    let text = "interface Reader { fn read(b: bytes): int }\n\
class File implements Reader { fn read(b: bytes): int { return 0 } }\n";
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
    assert!(
        nums.len().is_multiple_of(5) && !nums.is_empty(),
        "token stream: {nums:?}"
    );

    // Decode the delta-encoded stream into absolute (line, char, len, type) tuples.
    let mut toks = Vec::new();
    let (mut line, mut ch) = (0u32, 0u32);
    for g in nums.chunks(5) {
        if g[0] != 0 {
            line += g[0];
            ch = g[1];
        } else {
            ch += g[1];
        }
        toks.push((line, ch, g[2], g[3]));
    }
    const KEYWORD: u32 = 0;
    const CLASS: u32 = 5;

    // `interface` keyword at line 0, char 0, len 9.
    assert!(
        toks.contains(&(0, 0, 9, KEYWORD)),
        "`interface` must be a KEYWORD token; got: {toks:?}"
    );
    // `Reader` (the interface NAME) at line 0, char 10, len 6 -> CLASS-typed.
    assert!(
        toks.iter().any(|&(l, c, len, ty)| l == 0 && c == 10 && len == 6 && ty == CLASS),
        "interface name `Reader` must be a CLASS-typed token; got: {toks:?}"
    );
    // `implements` (line 1, char 11, len 10) -> KEYWORD (contextual).
    assert!(
        toks.iter().any(|&(l, c, len, ty)| l == 1 && c == 11 && len == 10 && ty == KEYWORD),
        "`implements` must be a KEYWORD token; got: {toks:?}"
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

/// DEFER Task 4.3: `defer` is offered as a completion keyword + has a snippet.
#[test]
fn lsp_offers_defer_keyword_and_snippet() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://defer_comp.as";
    let text = "fn f() {\n  \n}\n";
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
            "position": { "line": 1, "character": 2 }
        }),
    );
    let comp_resp = client.read_response(2, overall);
    let items = comp_resp["result"]
        .as_array()
        .expect("completion array result");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    // `defer` must appear as a keyword completion item.
    assert!(
        labels.contains(&"defer"),
        "`defer` must appear in keyword completions; got: {labels:?}"
    );
    // A snippet item with label "defer" must also be offered.
    let has_defer_snippet = items.iter().any(|i| {
        i["label"].as_str() == Some("defer")
            && i["kind"].as_u64() == Some(15) // CompletionItemKind::SNIPPET = 15
    });
    assert!(
        has_defer_snippet,
        "`defer` snippet must be offered; got items: {items:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// DEFER Task 4.3: `defer` is a reserved keyword (§2.2), so the CST emits
/// `DeferKw` and the semantic-token classifier styles it as KEYWORD (legend
/// index 0). This test pins that the `defer` token in `fn f() { defer g() }`
/// is classified as KEYWORD.
#[test]
fn lsp_defer_is_keyword_semantic_token() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    // A simple function with a defer statement.  The `defer` keyword appears at
    // line 0, char 11, length 5.
    // text: "fn f() { defer g() }\n"
    //        0123456789012345...
    //                  ^char 9
    let uri = "ascript-test://defer_tokens.as";
    let text = "fn f() { defer g() }\n";
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
    assert!(
        nums.len().is_multiple_of(5) && !nums.is_empty(),
        "token stream must be non-empty groups of 5; got: {nums:?}"
    );

    // Decode delta-encoded stream to absolute (line, char, len, type) tuples.
    let mut toks: Vec<(u32, u32, u32, u32)> = Vec::new();
    let (mut line, mut ch) = (0u32, 0u32);
    for g in nums.chunks(5) {
        if g[0] != 0 {
            line += g[0];
            ch = g[1];
        } else {
            ch += g[1];
        }
        toks.push((line, ch, g[2], g[3]));
    }
    const KEYWORD: u32 = 0;
    // `defer` appears at line 0, char 9, length 5.
    assert!(
        toks.iter().any(|&(l, c, len, ty)| l == 0 && c == 9 && len == 5 && ty == KEYWORD),
        "`defer` at (line=0, char=9, len=5) must be classified as KEYWORD; got: {toks:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// DX D3 Task 13: end-to-end frame-precise identifier completion — the cursor in
/// `a`'s body offers `a`'s own local + a module-global + a builtin, but NOT a
/// sibling function `b`'s local.
#[test]
fn lsp_completion_frame_precise_excludes_sibling_local() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://frame_precise.as";
    // line 0: let g = 0
    // line 1: fn a() {
    // line 2:   let foo = 1
    // line 3:   f         <- cursor here, char 3
    // line 4: }
    // line 5: fn b() {
    // line 6:   let bar = 2
    // line 7: }
    let text = "let g = 0\nfn a() {\n  let foo = 1\n  f\n}\nfn b() {\n  let bar = 2\n}\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    client.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3, "character": 3 }
        }),
    );
    let comp_resp = client.read_response(2, overall);
    let items = comp_resp["result"]
        .as_array()
        .expect("completion array result");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(labels.contains(&"foo"), "a's own local foo offered: {labels:?}");
    assert!(!labels.contains(&"bar"), "sibling b's local bar NOT offered: {labels:?}");
    assert!(labels.contains(&"g"), "module-global g offered: {labels:?}");
    assert!(labels.contains(&"print"), "builtin print offered: {labels:?}");

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// DX D3 Task 13: end-to-end member completion on a TYPED VALUE receiver — `c.`
/// where `c: C` is inferred offers class `C`'s field `x` and method `m`.
#[test]
fn lsp_completion_member_on_typed_instance() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://typed_member.as";
    // line 0: class C {
    // line 1:   x: number
    // line 2:   fn m() {}
    // line 3: }
    // line 4: let c = C()
    // line 5: c.        <- cursor at char 2 (just past the dot)
    let text = "class C {\n  x: number\n  fn m() {}\n}\nlet c = C()\nc.\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    client.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 5, "character": 2 }
        }),
    );
    let comp_resp = client.read_response(2, overall);
    let items = comp_resp["result"]
        .as_array()
        .expect("completion array result");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(labels.contains(&"x"), "instance field x offered: {labels:?}");
    assert!(labels.contains(&"m"), "instance method m offered: {labels:?}");

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// ADT Task 13: end-to-end LSP for payload enums — hover on a payload variant
/// shows its declared signature, and completion after `Shape.` offers the variants
/// (the payload variant with a snippet insert + signature detail).
#[test]
fn lsp_adt_variant_hover_and_completion() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://adt.as";
    // line 0: enum Shape {
    // line 1:   Circle(radius: float),
    // line 2:   Point,
    // line 3: }
    // line 4: Shape.
    let text = "enum Shape {\n  Circle(radius: float),\n  Point,\n}\nShape.\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Hover on `Circle` in the declaration (line 1, char 2).
    client.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 2 }
        }),
    );
    let hover_resp = client.read_response(2, overall);
    let content = hover_resp["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(
        content.contains("Shape.Circle(radius: float)"),
        "variant hover must show the payload signature; got: {content:?}"
    );

    // Completion after `Shape.` (line 4, char 6 — just past the dot).
    client.request(
        3,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 4, "character": 6 }
        }),
    );
    let comp_resp = client.read_response(3, overall);
    let items = comp_resp["result"]
        .as_array()
        .expect("completion array result");
    let circle = items
        .iter()
        .find(|i| i["label"].as_str() == Some("Circle"))
        .expect("Circle variant offered after Shape.");
    assert_eq!(
        circle["detail"].as_str(),
        Some("(radius: float)"),
        "payload variant completion carries a signature detail; got: {circle}"
    );
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(
        labels.contains(&"Point"),
        "unit variant Point must be offered; got: {labels:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// DX D1 Task 4: hovering a declaration that carries a `///` doc-comment shows the
/// USER'S doc body (via the shared `syntax::doc_comment` extractor), not just the
/// kind label. One source of truth feeds both `ascript doc` and the LSP hover.
#[test]
fn lsp_hover_shows_user_doc_comment() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://documented.as";
    // line 0: /// Greets the world warmly.
    // line 1: fn greet() { print("hi") }
    // line 2: greet()
    let text = "/// Greets the world warmly.\nfn greet() { print(\"hi\") }\ngreet()\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Hover on `greet` in the declaration (line 1, char 3).
    client.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 3 }
        }),
    );
    let hover_resp = client.read_response(2, overall);
    let content = hover_resp["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(
        content.contains("Greets the world warmly."),
        "hover must show the user's /// doc body; got: {content:?}"
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
    // `Counter`'s `C` is at char 32 (char 31 is the space after `await`; the use
    // range is now the bare Ident token, not the NameRef node + leading trivia).
    client.request(
        2,
        "textDocument/definition",
        json!({
            "textDocument": { "uri": f_uri },
            "position": { "line": 1, "character": 32 }
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

/// DX D4 §5.2: a typo'd name produces an `undefined-variable` diagnostic AND a
/// "did you mean" `codeAction` quickfix that replaces the typo with the closest
/// in-scope name. The end-to-end path: didOpen → codeAction → a QUICKFIX action
/// whose WorkspaceEdit rewrites `lenght` → `length`.
#[test]
fn lsp_did_you_mean_quickfix() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();

    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://typo.as";
    // `length` is defined; `lenght` on line 1 is a one-edit typo of it.
    let text = "let length = 5\nprint(lenght)\n";
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text } }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Request code actions over the whole file.
    let whole_range = json!({
        "start": { "line": 0, "character": 0 },
        "end": { "line": 2, "character": 0 }
    });
    client.request(
        30,
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": uri }, "range": whole_range,
                "context": { "diagnostics": [] } }),
    );
    let resp = client.read_response(30, overall);
    let actions = resp["result"].as_array().expect("codeAction array");

    // Find a quickfix whose edit replaces with `length`.
    let found = actions.iter().any(|a| {
        let is_quickfix = a["kind"].as_str() == Some("quickfix");
        let replaces_with_length = a["edit"]["changes"][uri]
            .as_array()
            .map(|edits| edits.iter().any(|e| e["newText"].as_str() == Some("length")))
            .unwrap_or(false);
        is_quickfix && replaces_with_length
    });
    assert!(
        found,
        "expected a did-you-mean quickfix replacing `lenght` with `length`; got: {actions:#?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

// ─── LSP audit fixes 1/2/9: didChange sync machinery (previously zero coverage) ──

/// Audit FIX 1 (§4.2): a trigger-character completion fired in the SAME instant as
/// its `didChange` must be served from text that includes the just-typed character,
/// not the 40ms-debounce-stale model. didOpen WITHOUT the dot → didChange inserting
/// `math.` → IMMEDIATE completion at the post-edit cursor → math members offered.
///
/// Before the fix (no `flush_pending_for` in the completion handler) this failed:
/// the request landed inside the debounce window, the store model still lacked the
/// `.`, so `member_access_alias` saw no member context and returned baseline items
/// (no `sqrt`). Verified by running this test against the pre-fix handler.
#[test]
fn lsp_didchange_trigger_completion_sees_fresh_text() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://didchange_trigger.as";
    // line 0: import * as math from "std/math"
    // line 1: let y = math          <- the dot is typed via didChange
    let text = "import * as math from \"std/math\"\nlet y = math\n";
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text } }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // The editor types `.` after `math` on line 1 (char 12) and fires completion
    // immediately (trigger character `.`), exactly like a real client.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": { "start": { "line": 1, "character": 12 },
                           "end":   { "line": 1, "character": 12 } },
                "text": "."
            }]
        }),
    );
    client.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 13 }
        }),
    );
    let comp_resp = client.read_response(2, overall);
    let items = comp_resp["result"]
        .as_array()
        .expect("completion array result");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    // The MEMBER-ACCESS result is exports-only. Asserting bare `sqrt` presence is
    // NOT enough: the stale-text BASELINE also contains a `sqrt` item via the
    // auto-import flood (`detail: "auto-import from std/math"`), which is exactly
    // how a weak assertion would mask the staleness bug. So pin the member
    // context: a `sqrt` item whose detail is NOT the auto-import string, and NO
    // keyword items (the baseline's `let` would prove we were served the stale
    // pre-dot model). (Since SIG Task 2.3, module-member items now carry a
    // signature detail — we distinguish them from auto-import items by the
    // detail NOT starting with "auto-import".)
    assert!(
        items.iter().any(|i| i["label"].as_str() == Some("sqrt")
            && i["detail"].as_str().is_none_or(|d| !d.starts_with("auto-import"))),
        "completion right after the `.` didChange must offer the math MEMBER `sqrt` \
         (pending edit flushed, not a stale-model auto-import item); got: {labels:?}"
    );
    assert!(
        !labels.contains(&"let"),
        "member-access completion must not be the stale-text baseline \
         (keywords present means the pre-dot model was served): {labels:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// Audit FIX 2 (§4.1): two back-to-back `didChange` notifications must BOTH fold
/// into the rebuilt model — the lost-edit race (both tasks folding from the same
/// base because fold-read and pending-insert sat in separate lock acquisitions)
/// would silently drop the first edit. Each edit inserts a distinct binding; after
/// the debounce window, completion must offer BOTH.
#[test]
fn lsp_didchange_back_to_back_edits_both_apply() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://didchange_fold.as";
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": "\n\n\n" } }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Edit 1 inserts `let alpha = 1` on line 0; edit 2 (immediately after, well
    // inside the 40ms debounce window) inserts `let beta = 2` on line 1. Edit 2's
    // range is relative to the post-edit-1 document, as a real client sends it.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": { "start": { "line": 0, "character": 0 },
                           "end":   { "line": 0, "character": 0 } },
                "text": "let alpha = 1"
            }]
        }),
    );
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 3 },
            "contentChanges": [{
                "range": { "start": { "line": 1, "character": 0 },
                           "end":   { "line": 1, "character": 0 } },
                "text": "let beta = 2"
            }]
        }),
    );

    // Wait out the debounce (40ms) so the coalesced rebuild lands deterministically.
    std::thread::sleep(Duration::from_millis(80));

    client.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 0 }
        }),
    );
    let comp_resp = client.read_response(2, overall);
    let items = comp_resp["result"]
        .as_array()
        .expect("completion array result");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(
        labels.contains(&"alpha"),
        "the FIRST of two back-to-back edits must not be lost: {labels:?}"
    );
    assert!(
        labels.contains(&"beta"),
        "the second edit must fold on top of the first: {labels:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// Audit FIX 9 (§4.5): `didClose` right after a `didChange` must purge the URI's
/// pending entry, so the still-sleeping debounce task no-ops (its `still_latest`
/// seq lookup finds nothing) instead of resurrecting the closed document and
/// re-publishing its diagnostics. After the debounce window, the ONLY
/// publishDiagnostics ever seen for the URI besides didOpen's is the empty clear
/// from didClose.
#[test]
fn lsp_didclose_purges_pending_no_ghost_republish() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://didclose_ghost.as";
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": "let x = 1\n" } }),
    );
    let open_note = client.read_notification("textDocument/publishDiagnostics", overall);
    assert_eq!(open_note["params"]["uri"].as_str(), Some(uri));

    // A didChange introducing a syntax error (so a ghost re-publish would be
    // unmistakable: a non-empty diagnostics array), then an immediate didClose
    // while the debounce task is still sleeping.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": "let = broken\n" }]
        }),
    );
    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": uri } }),
    );

    // Wait well past the debounce window so a buggy server would have re-published.
    std::thread::sleep(Duration::from_millis(150));

    // Fence with a request: drain every notification ahead of the response and
    // assert any publishDiagnostics for the closed URI is the empty clear.
    client.request_no_params(99, "shutdown");
    loop {
        let msg = client
            .next_message(overall, "shutdown response")
            .expect("stream open until shutdown response");
        if msg.get("id").and_then(Value::as_i64) == Some(99) {
            break;
        }
        if msg.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && msg["params"]["uri"].as_str() == Some(uri)
        {
            let diags = msg["params"]["diagnostics"]
                .as_array()
                .expect("diagnostics array");
            assert!(
                diags.is_empty(),
                "closed document must never get diagnostics re-published \
                 (ghost resurrection from a stale pending edit): {msg}"
            );
        }
    }

    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// SIG §3.1(c): signature help for a cross-file imported user fn shows param NAMES
/// and annotations (the index's `exported_fn_sig_from_decl` ParamList walk returns
/// names and types, not just arity). A defaulted param is rendered as `name?: type`.
#[test]
fn lsp_signature_help_cross_file_imported_fn() {
    let dir = std::env::temp_dir()
        .join(format!("ascript_lsp_sig_xfile_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // util.as: exports `add` with one required and one defaulted param.
    std::fs::write(
        dir.join("util.as"),
        "export fn add(first: number, second: number = 0) { return first + second }\n",
    )
    .unwrap();
    let main_path = dir.join("main.as");
    // main.as: imports `add` from util and calls it.  The file must parse cleanly so
    // the workspace index can record the import edge (an unparseable file gets an
    // empty-imports placeholder).  The cursor lands INSIDE the arg list of `add(0)`.
    let main_text = "import { add } from \"./util\"\nlet r = add(0)\n";
    std::fs::write(&main_path, main_text).unwrap();

    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();

    let root_uri = format!("file://{}", dir.display());
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": root_uri, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let util_uri = format!("file://{}", dir.join("util.as").display());
    let main_uri = format!("file://{}", main_path.display());

    // Open util.as first so the workspace index picks up the export.
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": util_uri,
                "languageId": "ascript",
                "version": 1,
                "text": "export fn add(first: number, second: number = 0) { return first + second }\n"
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Open main.as — this triggers indexing of the import edge.
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": main_uri,
                "languageId": "ascript",
                "version": 1,
                "text": main_text
            }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Request signature help inside `add(0)` on line 1.
    // "let r = add(0)\n" — `add(` starts at char 8, so char 12 is inside the arg list.
    client.request(
        2,
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": main_uri },
            "position": { "line": 1, "character": 12 }
        }),
    );
    let resp = client.read_response(2, overall);
    let label = resp["result"]["signatures"][0]["label"]
        .as_str()
        .unwrap_or_else(|| panic!("missing signature label: {resp}"));
    assert_eq!(
        label,
        "add(first: number, second?: number)",
        "names + annotations expected; defaulted param should be `second?: number`: {resp}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    let _ = client.wait_for_exit(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

/// SIG Task 2.3: stdlib member completion items carry real kind/detail; a
/// `completionItem/resolve` on a stdlib member fills documentation from the static
/// sig table (no SemanticModel round-trip needed).
#[test]
fn lsp_stdlib_member_kind_detail_and_docs() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://stdlib_member.as";
    // Cursor at the end of `math.` on line 1.
    let text = "import * as math from \"std/math\"\nlet y = math.\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // `math.` — cursor at char 13 on line 1 (just past the dot).
    client.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 13 }
        }),
    );
    let comp_resp = client.read_response(2, overall);
    let items = comp_resp["result"]
        .as_array()
        .expect("completion array result");

    // `pi` is a constant export — kind must be CONSTANT (21), detail must be "float".
    let pi = items.iter().find(|i| i["label"].as_str() == Some("pi"))
        .unwrap_or_else(|| panic!("pi not offered; got: {:?}", items.iter().map(|i| i["label"].as_str()).collect::<Vec<_>>()));
    assert_eq!(
        pi["kind"].as_u64(), Some(21),
        "pi kind must be CONSTANT (21): {:?}", pi
    );
    assert_eq!(
        pi["detail"].as_str(), Some("float"),
        "pi detail must be 'float': {:?}", pi
    );

    // `pow` is a function — kind must be FUNCTION (3), detail must contain the param list.
    let pow = items.iter().find(|i| i["label"].as_str() == Some("pow"))
        .unwrap_or_else(|| panic!("pow not offered"));
    assert_eq!(
        pow["kind"].as_u64(), Some(3),
        "pow kind must be FUNCTION (3): {:?}", pow
    );
    let pow_detail = pow["detail"].as_str().unwrap_or("");
    assert!(
        pow_detail.contains("base") && pow_detail.contains("exp"),
        "pow detail must contain param names; got: {pow_detail:?}"
    );

    // `completionItem/resolve` on the `pow` item must fill documentation.
    client.request(3, "completionItem/resolve", pow.clone());
    let resolve_resp = client.read_response(3, overall);
    let doc = resolve_resp["result"]["documentation"]["value"].as_str().unwrap_or("");
    assert!(
        doc.contains("Raise a base"),
        "resolved pow documentation must contain 'Raise a base'; got: {doc:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

// ─── DX D3 Task 11: the LSP identity unification ─────────────────────────────
//
// These exercise `WorkspaceIndex` directly (in-process — no wire) to pin the
// unified file-qualified identity (`GlobalBindingId`) and the shadow-correct
// `references_at` join it powers. They are the LOCKED edges from spec §4.1.
mod identity_unification {
    use ascript::lsp::workspace::{canon, GlobalBindingId, WorkspaceIndex};
    use std::path::{Path, PathBuf};

    /// Build an index over in-memory `(path, text)` pairs.
    fn idx(files: &[(&str, &str)]) -> WorkspaceIndex {
        let owned: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(p, t)| (PathBuf::from(p), t.to_string()))
            .collect();
        WorkspaceIndex::build_from_files(&owned)
    }

    /// A cursor ON an import-clause name (`f` in `import { f } from "./a"`) resolves
    /// to the DEFINER's identity (via the import edge), so references/rename from the
    /// clause find the def + every cross-file use — not an empty/importer-local set
    /// that would otherwise produce a corrupt partial rename. (Holistic-review BLOCKER:
    /// `def_identity` previously routed imports through `definition_at`, which can't
    /// see the clause name since it is not a `UseSite`.)
    #[test]
    fn import_clause_cursor_resolves_to_definer_identity() {
        let a_src = "export fn f() { return 1 }\n";
        let b_src = "import { f } from \"./a\"\nprint(f(1))\n";
        let index = idx(&[("/ws/a.as", a_src), ("/ws/b.as", b_src)]);
        let a = canon(Path::new("/ws/a.as"));
        let b = canon(Path::new("/ws/b.as"));
        let clause_off = b_src.find("{ f }").unwrap() + 2; // the `f` inside `{ f }`
        let refs = index.references_at(&b, clause_off, true);
        assert!(
            refs.iter().any(|(p, _)| *p == a),
            "refs from the import clause must include a.as's def: {refs:?}"
        );
        assert!(
            refs.iter().any(|(p, _)| *p == b),
            "refs from the import clause must include b.as's use: {refs:?}"
        );
        // Rename from the clause edits BOTH files (def + clause + use), not just the
        // clause token.
        let edits = index
            .rename_edits(&b, clause_off, "g")
            .expect("import-clause rename is allowed");
        assert!(
            edits.iter().any(|(p, _)| *p == a) && edits.iter().any(|(p, _)| *p == b),
            "rename from the import clause must edit both files: {edits:?}"
        );
    }

    /// Shadowed-local cross-file rename/refs: A exports `x`; B imports `x` AND has
    /// its OWN `let x`. Refs of A's export must include B's IMPORTED uses but NOT
    /// B's local `x` uses.
    #[test]
    fn shadowed_local_refs_do_not_cross_import_boundary() {
        let a_src = "export let x = 1\n";
        // B: imports x, prints it (imported use), then shadows with a local `let x`
        // and prints THAT (local use).
        let b_src = "import { x } from \"./a\"\nprint(x)\nfn g() {\n  let x = 2\n  print(x)\n}\n";
        let index = idx(&[("/ws/a.as", a_src), ("/ws/b.as", b_src)]);
        let a = canon(Path::new("/ws/a.as"));
        let b = canon(Path::new("/ws/b.as"));

        // Refs of A's exported `x` (cursor on the decl in a.as — `let x`, NOT the
        // `x` inside "export").
        let a_decl = a_src.find("let x").unwrap() + "let ".len();
        let refs = index.references_at(&a, a_decl, true);

        // Must include: a.as decl, and B's IMPORTED use `print(x)` (the FIRST `x` use
        // in b, inside the import clause is a def not a use — the use is `print(x)`).
        assert!(
            refs.iter().any(|(p, _)| *p == a),
            "refs must include a.as decl: {refs:?}"
        );
        // B's imported use offset: the `x` inside the FIRST `print(x)` (before the
        // local block).
        let imported_use_off = b_src.find("print(x)").unwrap() + "print(".len();
        assert!(
            refs.iter()
                .any(|(p, r)| *p == b && r.start <= imported_use_off && imported_use_off < r.end),
            "refs of A.x must include B's IMPORTED use: {refs:?}"
        );

        // Must NOT include B's LOCAL `x` use (inside fn g, the `print(x)` after `let x = 2`).
        let local_use_off = b_src.rfind("print(x)").unwrap() + "print(".len();
        assert!(
            local_use_off != imported_use_off,
            "test setup: local and imported uses must differ"
        );
        assert!(
            !refs
                .iter()
                .any(|(p, r)| *p == b && r.start <= local_use_off && local_use_off < r.end),
            "refs of A.x must NOT include B's shadowing LOCAL `x` use: {refs:?}"
        );
    }

    /// The inverse: refs of B's LOCAL `x` must NOT include the import or A's def.
    #[test]
    fn shadowing_local_refs_exclude_import_and_definer() {
        let a_src = "export let x = 1\n";
        let b_src = "import { x } from \"./a\"\nprint(x)\nfn g() {\n  let x = 2\n  print(x)\n}\n";
        let index = idx(&[("/ws/a.as", a_src), ("/ws/b.as", b_src)]);
        let a = canon(Path::new("/ws/a.as"));
        let b = canon(Path::new("/ws/b.as"));

        // Cursor on B's LOCAL decl `let x = 2`.
        let local_decl_off = b_src.find("let x = 2").unwrap() + "let ".len();
        let refs = index.references_at(&b, local_decl_off, true);

        // All refs are in B only.
        assert!(
            refs.iter().all(|(p, _)| *p == b),
            "local refs must stay in b.as: {refs:?}"
        );
        // Must NOT touch a.as at all.
        assert!(
            !refs.iter().any(|(p, _)| *p == a),
            "local refs must not include the definer file: {refs:?}"
        );
        // Must include the local use `print(x)` inside fn g.
        let local_use_off = b_src.rfind("print(x)").unwrap() + "print(".len();
        assert!(
            refs.iter()
                .any(|(_, r)| r.start <= local_use_off && local_use_off < r.end),
            "local refs must include the shadowing use: {refs:?}"
        );
        // Must NOT include the imported use `print(x)` before the block.
        let imported_use_off = b_src.find("print(x)").unwrap() + "print(".len();
        assert!(
            !refs
                .iter()
                .any(|(_, r)| r.start <= imported_use_off && imported_use_off < r.end),
            "local refs must not include the imported use: {refs:?}"
        );
    }

    /// Same-name, same-byte-range, different file → DISTINCT `GlobalBindingId`s.
    /// Two files each with a `let x` whose decl token sits at the SAME byte offset.
    /// The old `Local(TextRange)` would collide; `Local(FileId, _)` disambiguates.
    #[test]
    fn same_range_different_file_ids_are_distinct() {
        // Identical bodies → identical byte ranges for the `x` decl.
        let src = "fn g() {\n  let x = 1\n  print(x)\n}\n";
        let index = idx(&[("/ws/p.as", src), ("/ws/q.as", src)]);
        let p = canon(Path::new("/ws/p.as"));
        let q = canon(Path::new("/ws/q.as"));

        let decl_off = src.find("let x").unwrap() + "let ".len();
        let id_p = index
            .binding_id_at(&p, decl_off)
            .expect("p binding identity");
        let id_q = index
            .binding_id_at(&q, decl_off)
            .expect("q binding identity");

        // Both are Local with the SAME TextRange but DIFFERENT FileId → not equal.
        assert_ne!(
            id_p, id_q,
            "same-range locals in different files must be distinct GlobalBindingIds: {id_p:?} vs {id_q:?}"
        );
        match (&id_p, &id_q) {
            (GlobalBindingId::Local(fp, rp), GlobalBindingId::Local(fq, rq)) => {
                assert_eq!(rp, rq, "ranges should be identical (same source)");
                assert_ne!(fp, fq, "FileIds must differ");
            }
            other => panic!("expected two Local ids, got {other:?}"),
        }
    }

    /// A name bound INSIDE an or-pattern alternative (`Circle(r) | Square(r) => r`)
    /// is now declared by the CST resolver, so go-to-definition and find-references
    /// on the arm-body use of `r` resolve to a pattern bind site (not a Global
    /// fallback that would yield no definition). Regression for the `OrPat`-arm fix.
    #[test]
    fn or_pattern_binding_resolves_for_goto_and_references() {
        let src = "enum Shape {\n  Circle(radius: int),\n  Square(side: int),\n}\n\
fn dim(s: Shape): int {\n  return match s {\n    Shape.Circle(r) | Shape.Square(r) => r,\n  }\n}\n";
        let index = idx(&[("/ws/m.as", src)]);
        let m = canon(Path::new("/ws/m.as"));

        // Cursor on the arm BODY use of `r` (after `=> `).
        let body_use = src.find("=> r").unwrap() + "=> ".len();
        // Go-to-definition must resolve to SOME location in this file (a pattern bind
        // site), not fail with no definition (the pre-fix Global fallback).
        let def = index.definition_at(&m, body_use);
        assert!(
            def.is_some(),
            "goto-def on an or-pattern-bound name must resolve, got None"
        );
        let (def_path, def_span) = def.unwrap();
        assert_eq!(def_path, m, "definition must be in the same file");
        // The def span must land on one of the two `r` bind sites in the pattern
        // (the FIRST alternative's `r` owns the shared slot).
        let first_bind = src.find("Circle(r)").unwrap() + "Circle(".len();
        assert!(
            def_span.start <= first_bind && first_bind < def_span.end,
            "definition should point at the first `r` bind site; span={def_span:?}"
        );

        // find-references (including the decl) must include the body use of `r`.
        let refs = index.references_at(&m, body_use, true);
        assert!(
            refs.iter()
                .any(|(p, r)| *p == m && r.start <= body_use && body_use < r.end),
            "references of the or-pattern-bound `r` must include the body use: {refs:?}"
        );
    }

    /// All importers' uses of an imported name + the definer's def share ONE
    /// `Global(definerFileId, name)`. An importer's use resolves THROUGH the import
    /// edge to the definer's FileId.
    #[test]
    fn imported_use_lifts_to_definer_global_identity() {
        let a_src = "export fn f(x) { return x }\n";
        let b_src = "import { f } from \"./a\"\nprint(f(1))\n";
        let index = idx(&[("/ws/a.as", a_src), ("/ws/b.as", b_src)]);
        let a = canon(Path::new("/ws/a.as"));
        let b = canon(Path::new("/ws/b.as"));

        // The definer's def identity (cursor on `f`'s decl in a.as).
        let a_decl = a_src.find("f(x)").unwrap();
        let id_def = index.binding_id_at(&a, a_decl).expect("def identity");
        // B's use of `f` (in `print(f(1))`).
        let b_use = b_src.find("f(1)").unwrap();
        let id_use = index.binding_id_at(&b, b_use).expect("use identity");

        assert_eq!(
            id_def, id_use,
            "importer's use must lift to the definer's Global identity: {id_def:?} vs {id_use:?}"
        );
        match &id_def {
            GlobalBindingId::Global(fid, name) => {
                assert_eq!(name, "f");
                assert_eq!(
                    index.path_of(*fid).map(|p| p.to_path_buf()),
                    Some(a.clone()),
                    "the Global's FileId must be the DEFINER (a.as)"
                );
            }
            other => panic!("expected a Global identity, got {other:?}"),
        }
    }

    /// Uniform cross-file references: find-references on a binding returns the
    /// frame-precise local def AND its cross-file uses through ONE identity model.
    #[test]
    fn uniform_cross_file_references_through_one_identity() {
        let a_src = "export fn f(x) { return x }\nlet helper = 1\n";
        let b_src = "import { f } from \"./a\"\nprint(f(1))\nprint(f(2))\n";
        let index = idx(&[("/ws/a.as", a_src), ("/ws/b.as", b_src)]);
        let a = canon(Path::new("/ws/a.as"));
        let b = canon(Path::new("/ws/b.as"));

        let a_decl = a_src.find("f(x)").unwrap();
        let refs = index.references_at(&a, a_decl, true);

        // The decl in a + BOTH uses in b, all reached through the unified identity.
        assert!(refs.iter().any(|(p, _)| *p == a), "a decl: {refs:?}");
        let b_uses = refs.iter().filter(|(p, _)| *p == b).count();
        assert_eq!(b_uses, 2, "both f(1) and f(2) uses in b: {refs:?}");
    }

    /// Renaming a file (the `didRenameFiles` re-key) must FULLY unindex the old
    /// path from EVERY map — `defs_by_name`, `import_edges`, AND `importers` —
    /// not just `files`. Otherwise workspace-symbols / go-to-def return stale
    /// results pointing at the now-deleted old path, and a reverse import edge to
    /// the old path lingers. Regression for Task 0.15.
    ///
    /// NOTE: this drives the index ops directly (`fully_unindex(old)` +
    /// `reindex_file(new)`) — NOT through the live server's `didRenameFiles`
    /// notification handler, so the LSP wire/protocol path is not exercised here.
    #[test]
    fn rename_file_fully_unindexes_old_path() {
        // old.as: exports `Widget` AND imports `./dep` (an outgoing edge).
        let old_src = "import { helper } from \"./dep\"\nexport fn Widget() { return helper() }\n";
        let dep_src = "export fn helper() { return 1 }\n";
        let mut index = idx(&[("/ws/old.as", old_src), ("/ws/dep.as", dep_src)]);
        let old = canon(Path::new("/ws/old.as"));
        let new = canon(Path::new("/ws/new.as"));
        let dep = canon(Path::new("/ws/dep.as"));

        // Sanity: before the rename the symbol resolves under the OLD path, and
        // dep records old.as as an importer.
        let syms = index.workspace_symbols("Widget");
        assert!(
            syms.iter().any(|d| d.path == old && d.name == "Widget"),
            "before rename, Widget is defined in old.as: {syms:?}"
        );
        assert!(
            index
                .defs_by_name
                .get("Widget")
                .is_some_and(|v| v.iter().any(|d| d.path == old)),
            "defs_by_name has Widget@old before rename"
        );
        assert!(
            index
                .importers
                .get(&dep)
                .is_some_and(|s| s.contains(&old)),
            "dep records old.as as an importer before rename"
        );

        // The re-key the handler performs (Task 0.15 fix): the combined
        // `fully_unindex` makes the old "files-only remove" footgun impossible.
        index.fully_unindex(&old);
        let new_src = old_src; // the renamed file keeps its contents
        index.reindex_file(&new, new_src);

        // 1) The OLD path is gone from `files`.
        assert!(
            !index.files.contains_key(&old),
            "old.as must be removed from files"
        );
        // 2) No `defs_by_name` entry points at the OLD path anywhere.
        for (name, defs) in &index.defs_by_name {
            assert!(
                defs.iter().all(|d| d.path != old),
                "stale def `{name}` still points at old.as: {defs:?}"
            );
        }
        // 3) workspace-symbols for Widget now resolves under NEW, never OLD.
        let syms = index.workspace_symbols("Widget");
        assert!(
            syms.iter().any(|d| d.path == new && d.name == "Widget"),
            "after rename, Widget is defined in new.as: {syms:?}"
        );
        assert!(
            !syms.iter().any(|d| d.path == old),
            "workspace-symbols must not return the stale old.as path: {syms:?}"
        );
        // 4) No outgoing import edge keyed on the OLD path.
        assert!(
            !index.import_edges.contains_key(&old),
            "import_edges must not retain the old.as key"
        );
        // 5) The reverse edge: dep no longer records old.as as an importer (the
        //    new path takes its place).
        let dep_importers = index.importers.get(&dep);
        assert!(
            dep_importers.is_none_or(|s| !s.contains(&old)),
            "importers[dep] must not retain old.as: {dep_importers:?}"
        );
        assert!(
            dep_importers.is_some_and(|s| s.contains(&new)),
            "importers[dep] must now record new.as: {dep_importers:?}"
        );
    }
}

/// SIG §3.3: hovering a stdlib member (`math.sqrt`) over the wire returns a
/// hover response whose content includes the curated signature label.
#[test]
fn lsp_hover_stdlib_member_shows_signature() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://stdlib_hover.as";
    // line 0: import * as math from "std/math"
    // line 1: let y = math.sqrt(2)
    let text = "import * as math from \"std/math\"\nlet y = math.sqrt(2)\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Hover on `sqrt` in `math.sqrt(2)` (line 1, char 13 — inside `sqrt`).
    client.request(
        2,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 13 }
        }),
    );
    let hover_resp = client.read_response(2, overall);
    let content = hover_resp["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(
        content.contains("math.sqrt("),
        "hover on math.sqrt must show the signature; got: {content:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// SIG Task 3.1 C1: manual-invoke completion at `math.sq` (no trigger char) offers
/// `sqrt` with `filterText` = `"sq"` over the wire.
#[test]
fn lsp_completion_partial_identifier_member_context() {
    let overall = Instant::now() + Duration::from_secs(60);
    let mut client = LspClient::spawn();
    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": null, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    let uri = "ascript-test://partial_member.as";
    // line 0: import * as math from "std/math"
    // line 1: let y = math.sq
    // `let y = math.sq` — chars 0..15, cursor after `q` = char 15.
    let text = "import * as math from \"std/math\"\nlet y = math.sq\n";
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "ascript", "version": 1, "text": text }
        }),
    );
    let _ = client.read_notification("textDocument/publishDiagnostics", overall);

    // Manual invoke (no triggerCharacter) at the end of `math.sq`.
    client.request(
        2,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 15 },
            "context": { "triggerKind": 1 }
        }),
    );
    let resp = client.read_response(2, overall);
    let items = resp["result"]
        .as_array()
        .expect("completion result array");
    let sqrt = items
        .iter()
        .find(|i| i["label"].as_str() == Some("sqrt"))
        .unwrap_or_else(|| panic!("sqrt must be offered at math.sq|; labels: {:?}",
            items.iter().map(|i| i["label"].as_str().unwrap_or("")).collect::<Vec<_>>()
        ));
    assert_eq!(
        sqrt["filterText"].as_str(),
        Some("sq"),
        "filterText must equal the typed prefix 'sq': {sqrt}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
}

/// SIG C2: `workspace/diagnostic` yields between files so interleaved requests
/// (hover, completion) are serviced promptly rather than starved.
///
/// Test strategy: create a workspace with many `.as` files and force-index ALL of
/// them via `didOpen` (draining `publishDiagnostics` to confirm reindexing). Then
/// fire `workspace/diagnostic` (id A) and IMMEDIATELY fire `textDocument/hover` (id B)
/// on the hover-target file. Assert that the hover response (id B) arrives within a
/// tight deadline — substantially before the workspace diagnostic would complete if it
/// ran the full file list without yielding.
///
/// This is a SOUND WEAKER assertion rather than strict arrival-order because strict
/// arrival-order is non-deterministic over a subprocess stdio channel on a
/// `current_thread` runtime under scheduler load: the subprocess's internal task order
/// is unobservable from outside. What IS deterministic: if the hover request is
/// answered correctly within a tight wall-clock deadline, the handler CANNOT have been
/// blocked behind the full workspace scan (which itself takes many seconds on FILE_COUNT
/// files in a debug binary). The logical implication holds because the workspace scan
/// under the "no yield" hypothesis would block the entire LocalSet until all files are
/// processed, preventing the hover from being serviced until after the scan completes.
///
/// WHY sound-weaker rather than strict order: the LSP server's `current_thread` tokio
/// runtime serves requests concurrently via `spawn_local`, but the stdio transport
/// layer on the TEST side is a separate OS-level pipe buffer — we can only observe
/// byte arrival order across the pipe, which is subject to OS scheduler decisions
/// independent of internal async ordering. The deadline approach is deterministic:
/// if the hover completes within the deadline, it was NOT blocked for the full scan
/// duration; if it timed out, the scan was blocking. This is a one-sided proof that
/// is immune to scheduling jitter.
#[test]
fn lsp_workspace_diagnostic_yields() {
    // FILE_COUNT large enough that a no-yield full scan takes >> the hover time.
    // In debug mode on a typical machine, SemanticModel::build per file ~10-50ms,
    // so 40 files = 400ms–2s of scan time — easily observable vs a ~5ms hover.
    const FILE_COUNT: usize = 40;

    let dir = std::env::temp_dir().join(format!("ascript_lsp_diag_yield_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Each file has several functions so SemanticModel::build does real work.
    let mut file_texts: Vec<(String, String)> = Vec::with_capacity(FILE_COUNT);
    for i in 0..FILE_COUNT {
        let content = format!(
            "fn func_{i}_a(x) {{ return x + 1 }}\n\
             fn func_{i}_b(y) {{ return y * 2 }}\n\
             fn func_{i}_c(z) {{ return z - 1 }}\n"
        );
        let fname = format!("f{i:02}.as");
        std::fs::write(dir.join(&fname), &content).unwrap();
        file_texts.push((fname, content));
    }

    let root_uri = format!("file://{}", dir.display());
    // hover-target = first file; it will be in the document store so C2's reuse path is exercised.
    let hover_uri = format!("file://{}/{}", dir.display(), file_texts[0].0);

    // Generous overall deadline — this is a correctness test, not a latency test.
    let overall = Instant::now() + Duration::from_secs(120);
    // The hover must arrive within this tight window. 30 s is intentionally generous
    // to avoid flakiness under load, while still being well below what a no-yield full
    // scan of 40 files would take in a debug binary under heavy concurrent compilation.
    let hover_deadline = Duration::from_secs(30);

    let mut client = LspClient::spawn();

    client.request(
        1,
        "initialize",
        json!({ "processId": null, "rootUri": root_uri, "capabilities": {} }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    // Force-index ALL files via didOpen so the workspace/diagnostic scan has real work
    // to do. Draining publishDiagnostics for each file guarantees reindexing completed
    // before we fire the workspace pull (reindex_uri runs synchronously inside did_open,
    // before analyze_and_publish, which emits publishDiagnostics when it finishes).
    for (fname, text) in &file_texts {
        let uri = format!("file://{}/{}", dir.display(), fname);
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
    }
    // Drain all FILE_COUNT publishDiagnostics notifications before querying.
    for _ in 0..FILE_COUNT {
        let _ = client.read_notification("textDocument/publishDiagnostics", overall);
    }

    // All FILE_COUNT files are now in the index AND the document store.
    // Fire workspace/diagnostic (id 10) immediately followed by hover (id 11).
    // With no yields, the diagnostic scan blocks the task loop until all 40 files
    // are processed; the hover is starved. With yields, the hover is serviced
    // between file iterations.
    client.request(10, "workspace/diagnostic", json!({ "previousResultIds": [] }));
    // Send hover immediately — no sleep — so it races the ongoing diagnostic scan.
    client.request(
        11,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": hover_uri },
            "position": { "line": 0, "character": 3 }
        }),
    );

    // The hover (id 11) must complete within `hover_deadline`. The harness skips any
    // message with a different id, so we read specifically for id 11 while id 10 may
    // still be in-flight.
    let hover_abs_deadline = Instant::now() + hover_deadline;
    let hover_resp = client.read_response(11, hover_abs_deadline);
    assert!(
        hover_resp.get("result").is_some() && hover_resp.get("error").is_none(),
        "hover must return a well-formed result even while workspace/diagnostic is in flight: {hover_resp}"
    );

    // Now drain the workspace/diagnostic response (id 10) — it arrives after hover.
    let diag_resp = client.read_response(10, overall);
    assert!(
        diag_resp.get("result").is_some() && diag_resp.get("error").is_none(),
        "workspace/diagnostic must return a well-formed result: {diag_resp}"
    );
    let items = diag_resp["result"]["items"]
        .as_array()
        .expect("workspace/diagnostic items array");
    // All FILE_COUNT files were indexed before the pull — the report must cover them all.
    assert_eq!(
        items.len(),
        FILE_COUNT,
        "workspace/diagnostic must return one report per indexed file"
    );
    // The hover-target file must appear in the workspace report.
    assert!(
        items.iter().any(|r| r["uri"].as_str() == Some(hover_uri.as_str())),
        "workspace/diagnostic must include the hover-target file: {hover_uri}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

/// SIG C4: `didChangeWorkspaceFolders` with a removal purges the removed root's
/// files from the workspace index. Symbols defined only in the removed root must
/// vanish from `workspace/symbol`; symbols from surviving roots must remain.
#[test]
fn lsp_workspace_folder_removal_unindexes() {
    let dir =
        std::env::temp_dir().join(format!("ascript_lsp_folder_rm_{}", std::process::id()));
    let root_a = dir.join("root_a");
    let root_b = dir.join("root_b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();

    // Each root has a uniquely-named function so we can distinguish them in symbol search.
    let text_a = "fn alpha_only() { return 1 }\n";
    let text_b = "fn beta_only() { return 2 }\n";
    let path_a = root_a.join("a.as");
    let path_b = root_b.join("b.as");
    std::fs::write(&path_a, text_a).unwrap();
    std::fs::write(&path_b, text_b).unwrap();

    let uri_a_root = format!("file://{}", root_a.display());
    let uri_b_root = format!("file://{}", root_b.display());
    let uri_a_file = format!("file://{}", path_a.display());
    let uri_b_file = format!("file://{}", path_b.display());

    let overall = Instant::now() + Duration::from_secs(90);
    let mut client = LspClient::spawn();

    // Initialize with both roots. Use root_a as rootUri.
    client.request(
        1,
        "initialize",
        json!({
            "processId": null,
            "rootUri": uri_a_root,
            "capabilities": {},
            "workspaceFolders": [
                { "uri": uri_a_root, "name": "root_a" },
                { "uri": uri_b_root, "name": "root_b" }
            ]
        }),
    );
    let _ = client.read_response(1, overall);
    client.notify("initialized", json!({}));

    // Open both files via didOpen — `reindex_uri` runs synchronously inside did_open,
    // so once publishDiagnostics arrives both files are guaranteed to be in the index.
    for (uri, text) in [(&uri_a_file, text_a), (&uri_b_file, text_b)] {
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
        // Drain publishDiagnostics to confirm reindexing completed for this file.
        let _ = client.read_notification("textDocument/publishDiagnostics", overall);
    }

    // Both symbols must be visible before the removal.
    client.request(2, "workspace/symbol", json!({ "query": "only" }));
    let before = client.read_response(2, overall);
    let empty_before = vec![];
    let before_names: Vec<&str> = before["result"]
        .as_array()
        .unwrap_or(&empty_before)
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(
        before_names.contains(&"alpha_only"),
        "alpha_only must be indexed before removal: {before_names:?}"
    );
    assert!(
        before_names.contains(&"beta_only"),
        "beta_only must be indexed before removal: {before_names:?}"
    );

    // Remove root_b via didChangeWorkspaceFolders.
    client.notify(
        "workspace/didChangeWorkspaceFolders",
        json!({
            "event": {
                "added": [],
                "removed": [{ "uri": uri_b_root, "name": "root_b" }]
            }
        }),
    );

    // The notification is handled asynchronously; we cannot await its completion.
    // Use a round-trip request as a fence: the server processes notifications in
    // FIFO order before starting the next request, so a workspace/symbol query sent
    // after the notification is guaranteed to run AFTER the removal handler finishes.
    // We give it a small sleep as extra headroom for the write-lock release.
    std::thread::sleep(Duration::from_millis(100));

    // After removal: beta_only must be GONE; alpha_only must survive.
    client.request(3, "workspace/symbol", json!({ "query": "only" }));
    let after = client.read_response(3, overall);
    let empty_after = vec![];
    let after_names: Vec<&str> = after["result"]
        .as_array()
        .unwrap_or(&empty_after)
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(
        !after_names.contains(&"beta_only"),
        "beta_only must be gone after root_b removal: {after_names:?}"
    );
    assert!(
        after_names.contains(&"alpha_only"),
        "alpha_only must survive after root_b removal: {after_names:?}"
    );

    client.request_no_params(99, "shutdown");
    let _ = client.read_response(99, overall);
    client.notify_no_params("exit");
    client.close_stdin();
    let _ = client.wait_for_exit(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}
