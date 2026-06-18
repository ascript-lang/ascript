//! CNTR Phase 4 — INDEPENDENT REVIEWER adversarial probes (Plan Task 4.7).
//!
//! These are NOT part of the implementer's `tests/docker.rs` suite. They stand up a
//! deliberately HOSTILE mock daemon that:
//!   * lies about `Content-Length` (header says N, body has < N, then the socket drops);
//!   * drops the connection MID-multiplex-frame (mid-header and mid-payload), simulating
//!     an abrupt RESET while a `logs`/`exec` stream is live.
//!
//! Every probe runs the AScript program under a `tokio::time::timeout` so a HANG fails
//! the test (a hang is the worst outcome — worse than a panic). The contract under
//! review: a killed / lying daemon yields a clean terminal `[nil, err]` (or `[nil,nil]`
//! end), NEVER a hang or a Tier-2 panic that aborts the process.
//!
//! Gate: same as `tests/docker.rs` — `all(unix, feature = "net")`; the `std/docker`
//! probes additionally need `feature = "docker"`.
#![cfg(all(unix, feature = "net"))]

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// What a single accepted connection should do AFTER it has read the request head.
#[derive(Clone, Copy)]
enum Hostility {
    /// Send `head` + `partial` body bytes, then DROP (the head's Content-Length is a lie).
    LyingContentLength,
    /// 200 head, then a partial multiplex frame: a full 8-byte header claiming 64 payload
    /// bytes, only 3 payload bytes, then DROP (mid-payload RESET).
    KillMidPayload,
    /// 200 head, then only 3 of the 8 header bytes, then DROP (mid-header RESET).
    KillMidHeader,
    /// 200 head for a hijack 101 upgrade? No — here: a 200 chunked-less head then an
    /// IMMEDIATE drop with zero body bytes (EOF right at the body boundary).
    KillBeforeBody,
}

/// A hostile daemon that applies one `Hostility` to every connection. Returns
/// `(socket_path, guard)`; dropping the guard stops the accept loop.
struct HostileGuard {
    stop: Arc<Mutex<bool>>,
    _handle: tokio::task::JoinHandle<()>,
}
impl Drop for HostileGuard {
    fn drop(&mut self) {
        *self.stop.lock().unwrap() = true;
    }
}

async fn hostile_daemon(mode: Hostility) -> (String, HostileGuard) {
    let tmp = tempfile::Builder::new()
        .prefix("ascript_docker_hostile_")
        .suffix(".sock")
        .tempfile()
        .expect("temp sock");
    let sock_path = tmp.path().to_path_buf();
    drop(tmp);
    let listener = UnixListener::bind(&sock_path).expect("bind hostile sock");
    let sock_str = sock_path.to_string_lossy().to_string();
    let stop = Arc::new(Mutex::new(false));
    let stop_c = stop.clone();
    let handle = tokio::spawn(async move {
        loop {
            if *stop_c.lock().unwrap() {
                break;
            }
            match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
                Ok(Ok((stream, _))) => {
                    tokio::spawn(serve_hostile(stream, mode));
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
    });
    (
        sock_str,
        HostileGuard {
            stop,
            _handle: handle,
        },
    )
}

/// Read (and discard) the request head up to `\r\n\r\n`, then is_version? Route the
/// `/version` probe to a VALID version body so `docker.connect` succeeds; apply the
/// hostility only to the SUBSEQUENT (logs/inspect) request.
async fn serve_hostile(mut stream: UnixStream, mode: Hostility) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if head.len() > 64 * 1024 {
            return;
        }
        match tokio::time::timeout_at(deadline, stream.read(&mut byte)).await {
            Ok(Ok(0)) | Err(_) => return,
            Ok(Ok(_)) => head.push(byte[0]),
            Ok(Err(_)) => return,
        }
    }
    let line = String::from_utf8_lossy(&head);
    let req_line = line.lines().next().unwrap_or("");
    let is_version = req_line.contains("/version");

    if is_version {
        // Honest version response so connect() negotiates and we reach the hostile call.
        let body = br#"{"ApiVersion":"1.43"}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.write_all(body).await;
        let _ = stream.flush().await;
        return; // close
    }

    // The hostile second request.
    match mode {
        Hostility::LyingContentLength => {
            // Claim 100 bytes; send 4; drop.
            let head = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(head).await;
            let _ = stream.write_all(b"abcd").await;
            let _ = stream.flush().await;
            // Drop mid-body — the codec is waiting for 96 more bytes that never come.
        }
        Hostility::KillBeforeBody => {
            let head = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 50\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(head).await;
            let _ = stream.flush().await;
            // Drop with ZERO body bytes.
        }
        Hostility::KillMidPayload => {
            // A streaming logs head (read-to-EOF framing: Connection: close, no CL),
            // then a multiplex header claiming 64 payload bytes, only 3 sent, then drop.
            let head = b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.raw-stream\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(head).await;
            // mux header: stdout(1), reserved 0,0,0, size = 64 (big-endian).
            let mut frame = vec![1u8, 0, 0, 0];
            frame.extend_from_slice(&64u32.to_be_bytes());
            frame.extend_from_slice(b"abc"); // 3 of 64
            let _ = stream.write_all(&frame).await;
            let _ = stream.flush().await;
            // Drop mid-payload.
        }
        Hostility::KillMidHeader => {
            let head = b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.raw-stream\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(head).await;
            // Only 3 of the 8 header bytes, then drop.
            let _ = stream.write_all(&[1u8, 0, 0]).await;
            let _ = stream.flush().await;
        }
    }
    // Function returns → `stream` dropped → connection RESET/EOF.
}

/// Run an AScript program under a hard timeout; a hang is a test failure.
#[cfg(feature = "docker")]
async fn run_bounded(src: &str) -> String {
    match tokio::time::timeout(Duration::from_secs(20), ascript::run_source(src)).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => panic!("AScript program errored (Tier-2 escaped to abort?): {}", e.message),
        Err(_) => panic!("HANG: AScript program did not finish within 20s against a hostile daemon"),
    }
}

// ── Item 4: lying Content-Length on a UNARY call → clean Tier-1 err, no hang ──
#[cfg(feature = "docker")]
#[tokio::test]
async fn lying_content_length_unary_is_tier1_err_no_hang() {
    let (sock, _g) = hostile_daemon(Hostility::LyingContentLength).await;
    // `info()` is a plain GET that reads the body to end — the codec must hit EOF before
    // the 100-byte Content-Length is satisfied and surface a Tier-1 [nil, err].
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, err] = await docker.connect({{ socketPath: "{sock}" }})
print(err)
let [v, e2] = await d.info()
print(v)
print(e2 != nil)
print(e2.message)
"#
    );
    let out = run_bounded(&src).await;
    assert!(out.starts_with("nil\nnil\n"), "connect ok, info value nil, got:\n{out}");
    assert!(out.contains("true"), "info must yield a Tier-1 err, got:\n{out}");
    // The codec's premature-EOF message should surface (via the `docker: <e>` wrap).
    assert!(
        out.contains("Content-Length") || out.contains("closed") || out.contains("body read"),
        "err should name the transport failure, got:\n{out}"
    );
}

// ── Item 4 variant: head then immediate EOF (zero body) under a lying CL ──
#[cfg(feature = "docker")]
#[tokio::test]
async fn kill_before_body_unary_is_tier1_err_no_hang() {
    let (sock, _g) = hostile_daemon(Hostility::KillBeforeBody).await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, err] = await docker.connect({{ socketPath: "{sock}" }})
print(err)
let [v, e2] = await d.info()
print(v)
print(e2 != nil)
"#
    );
    let out = run_bounded(&src).await;
    assert!(out.starts_with("nil\nnil\n"), "connect ok, value nil, got:\n{out}");
    assert!(out.trim_end().ends_with("true"), "must be a Tier-1 err, got:\n{out}");
}

// ── Item 3: kill mid-PAYLOAD on a logs stream → terminal [nil, err] via next(), no hang ──
#[cfg(feature = "docker")]
#[tokio::test]
async fn kill_mid_payload_logs_stream_is_terminal_err_no_hang() {
    let (sock, _g) = hostile_daemon(Hostility::KillMidPayload).await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, e0] = await d.logs("victim", {{}})
print(e0)
let [item, err] = await logs.next()
print(item)
print(err != nil)
print(err.message)
let [item2, err2] = await logs.next()
print(item2)
print(err2)
"#
    );
    let out = run_bounded(&src).await;
    // open ok (e0 nil), first next() = terminal err, second next() = clean ended [nil,nil].
    assert!(out.starts_with("nil\nnil\ntrue\n"), "open ok + first next() is err, got:\n{out}");
    assert!(
        out.contains("truncated") || out.contains("read failed") || out.contains("closed"),
        "err should name truncation/transport, got:\n{out}"
    );
    assert!(out.trim_end().ends_with("nil\nnil"), "post-err next() is the terminal pair, got:\n{out}");
}

// ── Item 3 variant: kill mid-HEADER (3 of 8 header bytes) → terminal err, no hang ──
#[cfg(feature = "docker")]
#[tokio::test]
async fn kill_mid_header_logs_stream_is_terminal_err_no_hang() {
    let (sock, _g) = hostile_daemon(Hostility::KillMidHeader).await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, e0] = await d.logs("victim", {{}})
print(e0)
let [item, err] = await logs.next()
print(item)
print(err != nil)
"#
    );
    let out = run_bounded(&src).await;
    assert!(out.starts_with("nil\nnil\ntrue"), "open ok + first next() is err, got:\n{out}");
}

// ──────────────────────────────────────────────────────────────────────────────
// Item 5 — lifecycle: an HONEST daemon (valid version + a 2-frame logs stream + a
// ping) to exercise break-then-reuse, close-then-use, and stream-survives-client-close.
// ──────────────────────────────────────────────────────────────────────────────

/// An honest daemon: `/version` → 1.43; a logs path → a 2-frame multiplex stream
/// (stdout "a\n", stderr "b\n") then EOF; `/_ping` → "OK". Every other path → 404.
async fn honest_daemon() -> (String, HostileGuard) {
    let tmp = tempfile::Builder::new()
        .prefix("ascript_docker_honest_")
        .suffix(".sock")
        .tempfile()
        .expect("temp sock");
    let sock_path = tmp.path().to_path_buf();
    drop(tmp);
    let listener = UnixListener::bind(&sock_path).expect("bind honest sock");
    let sock_str = sock_path.to_string_lossy().to_string();
    let stop = Arc::new(Mutex::new(false));
    let stop_c = stop.clone();
    let handle = tokio::spawn(async move {
        loop {
            if *stop_c.lock().unwrap() {
                break;
            }
            match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
                Ok(Ok((stream, _))) => {
                    tokio::spawn(serve_honest(stream));
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
    });
    (
        sock_str,
        HostileGuard {
            stop,
            _handle: handle,
        },
    )
}

async fn serve_honest(mut stream: UnixStream) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if head.len() > 64 * 1024 {
            return;
        }
        match tokio::time::timeout_at(deadline, stream.read(&mut byte)).await {
            Ok(Ok(0)) | Err(_) => return,
            Ok(Ok(_)) => head.push(byte[0]),
            Ok(Err(_)) => return,
        }
    }
    let line = String::from_utf8_lossy(&head);
    let req = line.lines().next().unwrap_or("");
    if req.contains("/version") {
        let body = br#"{"ApiVersion":"1.43"}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.write_all(body).await;
    } else if req.contains("/_ping") {
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .await;
    } else if req.contains("/logs") {
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.raw-stream\r\nConnection: close\r\n\r\n")
            .await;
        // frame 1: stdout "a\n"; frame 2: stderr "b\n"; then EOF.
        for (ty, payload) in [(1u8, b"a\n" as &[u8]), (2u8, b"b\n")] {
            let mut f = vec![ty, 0, 0, 0];
            f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            f.extend_from_slice(payload);
            let _ = stream.write_all(&f).await;
            // tiny pause so the consumer can pull one frame, break, and reuse.
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    } else {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    }
    let _ = stream.flush().await;
}

// ── Item 5a: for-await {break} then REUSE the same stream via next() (NO close) ──
// The stream resource must survive the break and yield the SECOND frame on next().
#[cfg(feature = "docker")]
#[tokio::test]
async fn for_await_break_then_reuse_next_yields_remaining_no_hang() {
    let (sock, _g) = honest_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, e0] = await d.logs("c", {{}})
print(e0)
let seen = ""
for await (entry in logs) {{ seen = entry.text; break }}
print(seen)
let [item, err] = await logs.next()
print(err)
print(item.text)
let [item2, err2] = await logs.next()
print(item2)
print(err2)
"#
    );
    let out = run_bounded(&src).await;
    // break after frame1 ("a\n"); reuse yields frame2 ("b\n"); then clean end [nil,nil].
    assert!(out.starts_with("nil\na\n"), "open ok + first frame via for-await, got:\n{out}");
    assert!(out.contains("b\n"), "reuse next() must yield the 2nd frame, got:\n{out}");
    assert!(out.trim_end().ends_with("nil\nnil"), "then clean end, got:\n{out}");
}

// ── Item 5b: d.close() then d.ping() → a clean Tier-1 err, NOT a panic ──
#[cfg(feature = "docker")]
#[tokio::test]
async fn client_close_then_ping_is_clean_err_no_panic() {
    let (sock, _g) = honest_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
d.close()
let [v, err] = await d.ping()
print(v)
print(err != nil)
print(err.message)
"#
    );
    let out = run_bounded(&src).await;
    assert!(out.starts_with("nil\ntrue\n"), "ping after close → Tier-1 err, got:\n{out}");
    assert!(out.contains("closed"), "err should say the client is closed, got:\n{out}");
}

// ── Item 5c: an open stream + client.close() — the stream must still drain cleanly ──
// (The stream owns its own connection; closing the CLIENT handle must not break it.)
#[cfg(feature = "docker")]
#[tokio::test]
async fn open_stream_survives_client_close_no_hang() {
    let (sock, _g) = honest_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, _e] = await d.logs("c", {{}})
d.close()
let n = 0
for await (entry in logs) {{ n = n + 1 }}
print(n)
"#
    );
    let out = run_bounded(&src).await;
    // The stream is independent of the client handle → both frames still drain.
    assert!(out.trim_end().ends_with("2"), "stream survives client.close(), got:\n{out}");
}

// ── Item 5d: double-close a stream then next() — no double-free / panic ──
#[cfg(feature = "docker")]
#[tokio::test]
async fn double_close_stream_then_next_is_ended_no_panic() {
    let (sock, _g) = honest_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, _e] = await d.logs("c", {{}})
logs.close()
logs.close()
let [item, err] = await logs.next()
print(item)
print(err)
"#
    );
    let out = run_bounded(&src).await;
    assert_eq!(out, "nil\nnil\n", "double close then next() = terminal pair, got:\n{out}");
}

// ── Item 3: the same mid-payload kill, drained via `d.exec`-shaped manual loop ──
// (exec drains internally; here we drive next() in a loop to prove the loop terminates.)
#[cfg(feature = "docker")]
#[tokio::test]
async fn kill_mid_payload_drain_loop_terminates_no_hang() {
    let (sock, _g) = hostile_daemon(Hostility::KillMidPayload).await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, _e] = await d.logs("victim", {{}})
let count = 0
let done = false
while (!done) {{
    let [item, err] = await logs.next()
    if (err != nil) {{ print("ERR"); done = true }}
    else if (item == nil) {{ print("END"); done = true }}
    else {{ count = count + 1; if (count > 1000) {{ done = true }} }}
}}
print(count)
"#
    );
    let out = run_bounded(&src).await;
    // The drain loop must terminate at the terminal err (not spin to 1000, not hang).
    assert!(out.contains("ERR"), "drain loop should terminate on the err, got:\n{out}");
    assert!(!out.contains("1000"), "must not spin past the terminal, got:\n{out}");
}

// ── Item 6: PROBE the actual d.ping() return value (spec §4.2 says [true, err]) ──
#[cfg(feature = "docker")]
#[tokio::test]
async fn ping_return_value_probe() {
    let (sock, _g) = honest_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [pong, err] = await d.ping()
print(err)
print(pong)
print(pong == true)
"#
    );
    let out = run_bounded(&src).await;
    // RECORD the observed shape (this test documents the deviation, it does not assert true).
    eprintln!("PING RETURN OBSERVED:\n{out}");
}
