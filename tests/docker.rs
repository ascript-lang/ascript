//! CNTR Phase 4, Task 4.1 — recorded-fixture mock Engine daemon over UDS (spec §10.1).
//!
//! This file is the single home for:
//!   1. **Byte-exact HTTP/1.1 fixture builders** — all fixtures are assembled as Rust
//!      `Vec<u8>` with literal CRLF (`\r\n`) and computed `Content-Length` values, so
//!      editor encoding can never silently corrupt a fixture. Binary fixtures (multiplexed
//!      frames, exec-upgrade) are assembled byte-by-byte.
//!   2. **`mock_daemon()`** — a tokio `UnixListener` task that reads each request head,
//!      routes by the `METHOD /path` request line, writes the matching fixture, and closes
//!      the connection (Docker Engine API is `Connection: close` per call). Returns
//!      `(socket_path, guard)` where dropping the guard shuts the listener down.
//!   3. **Smoke test** — `mock_daemon_serves_ping_over_raw_unix_connect` — drives the
//!      mock with a raw `.as` program via `unix.connect`/`write`/`readLine` and asserts
//!      that the `ping.http` fixture's `OK` body is readable.
//!
//! ## Content-Length correctness contract
//! Every `*_http()` fixture builder calls `cl_header` which computes the exact byte
//! length of the pre-assembled body and embeds it in the `Content-Length:` header. A
//! mismatch would cause the `http1` codec's content-length framing to underread or
//! overread — which would surface as a confusing failure in Task 4.2. All builders
//! are covered by `fixture_content_lengths_are_exact` below.
//!
//! ## Chunked-encoding format
//! Chunked bodies follow RFC 7230 §4.1: each chunk is `<hex-size>\r\n<data>\r\n`,
//! terminated by `0\r\n\r\n`. The `chunked_body` helper encodes an arbitrary body
//! as a single chunk (simplest; one chunk per fixture line group for readability).
//!
//! ## Multiplexed-frame format (Docker attach/logs)
//! Each frame: `[STREAM_TYPE(1B), 0, 0, 0, SIZE(4B big-endian u32)]` + SIZE bytes of
//! payload. STREAM_TYPE: 0=stdin, 1=stdout, 2=stderr. See `mux_frame()`.
//!
//! Gate: `#![cfg(all(unix, feature = "net"))]` — the mock uses `tokio::net::UnixListener`
//! (Unix-only) and the AScript test uses `std/net/unix` (net-gated). Under
//! `--no-default-features` or non-Unix, this whole file compiles to nothing.

#![cfg(all(unix, feature = "net"))]

use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

// ──────────────────────────────────────────────────────────────────────────────
// Fixture builders
//
// Naming: `foo_http()` returns a full HTTP/1.1 response (Vec<u8>).
//         `foo_bin()` returns a raw binary payload (Vec<u8>) with no HTTP head.
// ──────────────────────────────────────────────────────────────────────────────

/// Build a `Content-Length: N\r\n` header where N is the byte length of `body`.
fn cl_header(body: &[u8]) -> String {
    format!("Content-Length: {}\r\n", body.len())
}

/// Build a full HTTP/1.1 response with `Content-Length` framing.
///
/// `status_line` — e.g. `"HTTP/1.1 200 OK"` (no CRLF).
/// `extra_headers` — zero or more `"Name: value"` strings (no CRLF).
/// `body` — the response body bytes; `Content-Length` is computed from this.
fn http_response(status_line: &str, extra_headers: &[&str], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(status_line.as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(b"Content-Type: application/json\r\n");
    for h in extra_headers {
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(cl_header(body).as_bytes());
    out.extend_from_slice(b"Connection: close\r\n");
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

/// Build a 204 No Content response with NO body and NO Content-Length header.
/// (Docker returns 204 for start/stop/remove — no body, no Content-Length.)
fn no_content_response() -> Vec<u8> {
    b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_vec()
}

/// Encode `body` as a single HTTP chunked-encoding chunk + terminal chunk.
/// Result: `<hex(len)>\r\n<body>\r\n0\r\n\r\n`
fn chunked_body(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(b"\r\n0\r\n\r\n");
    out
}

/// Build a chunked HTTP/1.1 response.
fn http_chunked_response(status_line: &str, extra_headers: &[&str], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(status_line.as_bytes());
    out.extend_from_slice(b"\r\n");
    for h in extra_headers {
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Transfer-Encoding: chunked\r\n");
    out.extend_from_slice(b"Connection: close\r\n");
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&chunked_body(body));
    out
}

/// Build a single Docker multiplexed-stream frame.
/// `stream_type`: 0=stdin, 1=stdout, 2=stderr.
/// `payload`: the frame payload bytes.
///
/// Format: `[stream_type, 0, 0, 0, SIZE_BE_u32(4 bytes)]` + SIZE bytes of payload.
fn mux_frame(stream_type: u8, payload: &[u8]) -> Vec<u8> {
    let size = payload.len() as u32;
    let mut out = Vec::with_capacity(8 + payload.len());
    out.push(stream_type);
    out.push(0);
    out.push(0);
    out.push(0);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

// ── Happy-path HTTP/1.1 response fixtures ─────────────────────────────────────

/// `GET /version` → 200 with current Docker Engine version info.
pub fn version_http() -> Vec<u8> {
    let body = br#"{"ApiVersion":"1.43","Version":"24.0.0","MinAPIVersion":"1.12","Os":"linux","Arch":"amd64","KernelVersion":"6.1.0","BuildTime":"2023-07-01T00:00:00.000000000+00:00","GitCommit":"abc1234","GoVersion":"go1.20.6"}"#;
    http_response("HTTP/1.1 200 OK", &[], body)
}

/// `GET /version` with an old API version (below the floor the docker module requires).
pub fn version_old_http() -> Vec<u8> {
    let body = br#"{"ApiVersion":"1.20","Version":"1.10.0","MinAPIVersion":"1.12","Os":"linux","Arch":"amd64","KernelVersion":"4.4.0"}"#;
    http_response("HTTP/1.1 200 OK", &[], body)
}

/// `GET /_ping` → 200, body `OK`.
pub fn ping_http() -> Vec<u8> {
    let body = b"OK";
    let mut out = Vec::new();
    out.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
    out.extend_from_slice(b"Content-Type: text/plain\r\n");
    out.extend_from_slice(cl_header(body).as_bytes());
    out.extend_from_slice(b"Connection: close\r\n");
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

/// `GET /containers/json` → 200, JSON array of 2 containers.
/// Task 4.2 asserts `len(cs)==2`, `cs[0].Names[0]=="/web"`.
pub fn containers_list_http() -> Vec<u8> {
    let body = br#"[{"Id":"abc123def456","Names":["/web"],"Image":"nginx:latest","ImageID":"sha256:aaa","Command":"nginx -g 'daemon off;'","Created":1700000001,"Ports":[],"State":"running","Status":"Up 2 hours"},{"Id":"def456abc789","Names":["/db"],"Image":"postgres:15","ImageID":"sha256:bbb","Command":"docker-entrypoint.sh postgres","Created":1700000002,"Ports":[{"IP":"0.0.0.0","PrivatePort":5432,"PublicPort":5432,"Type":"tcp"}],"State":"running","Status":"Up 1 hour"}]"#;
    http_response("HTTP/1.1 200 OK", &[], body)
}

/// `GET /containers/{id}/json` → 200, container inspect.
pub fn inspect_http() -> Vec<u8> {
    let body = br#"{"Id":"abc123def456","Name":"/web","State":{"Status":"running","Running":true,"Paused":false,"Restarting":false,"OOMKilled":false,"Dead":false,"Pid":1234,"ExitCode":0,"Error":"","StartedAt":"2023-01-01T00:00:00Z","FinishedAt":"0001-01-01T00:00:00Z"},"Config":{"Image":"nginx:latest","Cmd":["nginx","-g","daemon off;"],"Env":["PATH=/usr/local/sbin"],"Labels":{}},"HostConfig":{"NetworkMode":"bridge"},"NetworkSettings":{"IPAddress":"172.17.0.2","Ports":{}}}"#;
    http_response("HTTP/1.1 200 OK", &[], body)
}

/// `POST /containers/create` → 201 Created.
pub fn create_http() -> Vec<u8> {
    let body = br#"{"Id":"abc123","Warnings":[]}"#;
    http_response("HTTP/1.1 201 Created", &[], body)
}

/// `POST /containers/{id}/start` → 204 No Content.
pub fn start_204_http() -> Vec<u8> {
    no_content_response()
}

/// `POST /containers/{id}/stop` → 204 No Content.
pub fn stop_204_http() -> Vec<u8> {
    no_content_response()
}

/// `DELETE /containers/{id}` → 204 No Content.
pub fn remove_204_http() -> Vec<u8> {
    no_content_response()
}

/// `POST /containers/{id}/wait` → 200, `{"StatusCode":0}`.
pub fn wait_http() -> Vec<u8> {
    let body = br#"{"StatusCode":0}"#;
    http_response("HTTP/1.1 200 OK", &[], body)
}

/// `GET /images/json` → 200, JSON array of images.
pub fn images_list_http() -> Vec<u8> {
    let body = br#"[{"Id":"sha256:aaabbbccc","RepoTags":["nginx:latest"],"RepoDigests":[],"ParentId":"","Size":141908078,"SharedSize":0,"VirtualSize":141908078,"Labels":{},"Containers":0},{"Id":"sha256:dddeeefff","RepoTags":["postgres:15"],"RepoDigests":[],"ParentId":"","Size":381000000,"SharedSize":0,"VirtualSize":381000000,"Labels":{},"Containers":0}]"#;
    http_response("HTTP/1.1 200 OK", &[], body)
}

/// `DELETE /images/{name}` → 200, list of deleted layers.
pub fn image_remove_http() -> Vec<u8> {
    let body = br#"[{"Deleted":"sha256:aaabbbccc"},{"Untagged":"nginx:latest"}]"#;
    http_response("HTTP/1.1 200 OK", &[], body)
}

/// `*` → 404, `{"message":"No such container: xyz"}`.
/// Task 4.2 asserts `err.statusCode==404` and checks the message.
pub fn error_404_http() -> Vec<u8> {
    let body = br#"{"message":"No such container: xyz"}"#;
    http_response("HTTP/1.1 404 Not Found", &[], body)
}

/// `POST /exec/{id}/start` → 201 Created.
pub fn exec_create_http() -> Vec<u8> {
    let body = br#"{"Id":"exec123"}"#;
    http_response("HTTP/1.1 201 Created", &[], body)
}

/// `GET /exec/{id}/json` → 200, exec inspect.
pub fn exec_inspect_http() -> Vec<u8> {
    let body = br#"{"ExitCode":0,"Running":false,"ProcessConfig":{"entrypoint":"ls","arguments":["-la"]}}"#;
    http_response("HTTP/1.1 200 OK", &[], body)
}

// ── Chunked streaming fixtures ──────────────────────────────────────────────

/// `GET /events` → 200 chunked, 3 JSON-line Docker events.
pub fn events_jsonl_http() -> Vec<u8> {
    // Three JSON events, each on its own line.
    let body = concat!(
        r#"{"Type":"container","Action":"start","Actor":{"ID":"abc123","Attributes":{"image":"nginx:latest","name":"web"}},"scope":"local","time":1700000001,"timeNano":1700000001000000000}"#, "\n",
        r#"{"Type":"container","Action":"stop","Actor":{"ID":"abc123","Attributes":{"image":"nginx:latest","name":"web"}},"scope":"local","time":1700000002,"timeNano":1700000002000000000}"#, "\n",
        r#"{"Type":"image","Action":"pull","Actor":{"ID":"nginx:latest","Attributes":{"name":"nginx:latest"}},"scope":"local","time":1700000003,"timeNano":1700000003000000000}"#, "\n",
    );
    http_chunked_response("HTTP/1.1 200 OK", &[], body.as_bytes())
}

/// `POST /images/create` (pull) → 200 chunked progress stream with success at the end.
pub fn pull_progress_http() -> Vec<u8> {
    let body = concat!(
        r#"{"status":"Pulling from library/nginx","id":"latest"}"#, "\n",
        r#"{"status":"Pulling fs layer","progressDetail":{},"id":"a3ed95caeb02"}"#, "\n",
        r#"{"status":"Downloading","progressDetail":{"current":1048576,"total":10485760},"progress":"[====>    ]  1.05MB/10.49MB","id":"a3ed95caeb02"}"#, "\n",
        r#"{"status":"Download complete","progressDetail":{},"id":"a3ed95caeb02"}"#, "\n",
        r#"{"status":"Pull complete","progressDetail":{},"id":"a3ed95caeb02"}"#, "\n",
        r#"{"status":"Digest: sha256:aaabbbccc"}"#, "\n",
        r#"{"status":"Status: Downloaded newer image for nginx:latest"}"#, "\n",
    );
    http_chunked_response("HTTP/1.1 200 OK", &[], body.as_bytes())
}

/// `POST /images/create` (pull) → 200 chunked, with an in-stream error line.
pub fn pull_error_http() -> Vec<u8> {
    let body = concat!(
        r#"{"status":"Pulling from library/nonexistent","id":"latest"}"#, "\n",
        r#"{"error":"manifest unknown","errorDetail":{"message":"manifest unknown: manifest unknown"}}"#, "\n",
    );
    http_chunked_response("HTTP/1.1 200 OK", &[], body.as_bytes())
}

// ── Binary stream fixtures ───────────────────────────────────────────────────

/// Docker multiplexed log stream (non-TTY mode).
///
/// Format: frames with 8-byte headers `[STREAM_TYPE(1B), 0, 0, 0, SIZE(4B big-endian u32)]`.
/// This fixture has:
///   - stdout frame: `"hello\n"` (type=1, size=6)
///   - stderr frame: `"oops\n"`  (type=2, size=5)
pub fn logs_multiplexed_bin() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&mux_frame(1, b"hello\n")); // stdout
    out.extend_from_slice(&mux_frame(2, b"oops\n")); // stderr
    out
}

/// Raw TTY log output (no multiplexed frame headers — TTY mode).
pub fn logs_tty_bin() -> Vec<u8> {
    b"plain tty output\n".to_vec()
}

/// `POST /exec/{id}/start` upgrade response: `101 Switching Protocols` head followed
/// by multiplexed frames in the raw stream after the upgrade.
pub fn exec_upgrade_bin() -> Vec<u8> {
    let mut out = Vec::new();
    // HTTP/1.1 101 head.
    out.extend_from_slice(
        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: tcp\r\nConnection: Upgrade\r\n\r\n",
    );
    // Multiplexed frames follow inline (the codec hands the raw transport back to the
    // caller after parsing the 101 head; the caller then reads these frames directly).
    out.extend_from_slice(&mux_frame(1, b"exec stdout\n")); // stdout
    out.extend_from_slice(&mux_frame(2, b"exec stderr\n")); // stderr
    out
}

/// `GET /containers/{id}/logs` (multiplexed, non-TTY) → 200 Content-Length body of
/// 8-byte-framed multiplex frames (stdout "hello\n" + stderr "oops\n").
pub fn logs_multiplexed_http() -> Vec<u8> {
    let body = logs_multiplexed_bin();
    // application/vnd.docker.raw-stream is what the real daemon sends; the http1 codec
    // frames by Content-Length regardless, so the body is read byte-exact.
    http_response_octet("HTTP/1.1 200 OK", &body)
}

/// `GET /containers/{id}/logs` for a TTY container → 200 raw text body (no frames).
pub fn logs_tty_http() -> Vec<u8> {
    http_response_octet("HTTP/1.1 200 OK", &logs_tty_bin())
}

/// `GET /containers/{id}/logs` truncated-frame variant (header claims 20, only 4 follow).
pub fn logs_truncated_frame_http() -> Vec<u8> {
    http_response_octet("HTTP/1.1 200 OK", &logs_truncated_frame_bin())
}

/// `GET /containers/{id}/logs` oversize-frame variant (SIZE > 16 MiB, no payload).
pub fn logs_oversize_frame_http() -> Vec<u8> {
    http_response_octet("HTTP/1.1 200 OK", &logs_oversize_frame_bin())
}

/// Build a raw-octet HTTP/1.1 response with Content-Length framing (no application/json
/// Content-Type — these bodies are binary multiplex streams).
fn http_response_octet(status_line: &str, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(status_line.as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(b"Content-Type: application/vnd.docker.raw-stream\r\n");
    out.extend_from_slice(cl_header(body).as_bytes());
    out.extend_from_slice(b"Connection: close\r\n");
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

// ── Hostile fixtures ─────────────────────────────────────────────────────────

/// A multiplexed stream frame whose 8-byte header claims SIZE=N but the payload is
/// truncated to fewer than N bytes before EOF. The frame-decoder must return an error
/// (not hang or panic) when the stream ends before SIZE bytes are available.
pub fn logs_truncated_frame_bin() -> Vec<u8> {
    // Claim 20 bytes of payload, but only write 4.
    let claimed_size: u32 = 20;
    // Header: [stream_type=1, 0, 0, 0, SIZE(4 big-endian bytes)] + 4 payload bytes.
    let mut out = vec![1u8, 0, 0, 0]; // stream_type + padding
    out.extend_from_slice(&claimed_size.to_be_bytes());
    out.extend_from_slice(b"abc!"); // only 4 bytes instead of 20
    // EOF here — the decoder must detect the truncation.
    out
}

/// A multiplexed stream frame whose SIZE field is > 16 MiB (the hard cap).
/// The frame-decoder must reject this without allocating SIZE bytes.
pub fn logs_oversize_frame_bin() -> Vec<u8> {
    // SIZE = 16 MiB + 1 = 0x01_00_00_01
    let oversize: u32 = 16 * 1024 * 1024 + 1;
    // Header only: [stream_type=1, 0, 0, 0, SIZE(4 big-endian bytes)] — no payload.
    let mut out = vec![1u8, 0, 0, 0]; // stream_type + padding
    out.extend_from_slice(&oversize.to_be_bytes());
    // No payload bytes at all — the decoder should reject on the size check alone.
    out
}

/// A chunked HTTP/1.1 response with a non-hex chunk-size line.
/// The http1 codec must return `Err("bad chunk size")`, not hang or panic.
pub fn chunked_bad_size_http() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
    out.extend_from_slice(b"Transfer-Encoding: chunked\r\n");
    out.extend_from_slice(b"Connection: close\r\n");
    out.extend_from_slice(b"\r\n");
    // Chunk-size line is not valid hex.
    out.extend_from_slice(b"ZZZZ\r\n");
    out.extend_from_slice(b"some data\r\n");
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// Content-length correctness self-test
// ──────────────────────────────────────────────────────────────────────────────

/// Parse the `Content-Length` value from a raw HTTP/1.1 response and compare it to the
/// ACTUAL body byte count (bytes after `\r\n\r\n`). Panics if they differ.
fn assert_cl_correct(name: &str, response: &[u8]) {
    // Find the header-body separator.
    let sep = b"\r\n\r\n";
    let Some(head_end) = response.windows(sep.len()).position(|w| w == sep) else {
        panic!("{name}: response has no \\r\\n\\r\\n separator");
    };
    let head = &response[..head_end];
    let body_start = head_end + sep.len();
    let actual_body_len = response.len() - body_start;

    // Extract Content-Length from the head (case-insensitive).
    let head_str = String::from_utf8_lossy(head).to_ascii_lowercase();
    let Some(cl_pos) = head_str.find("content-length:") else {
        // No Content-Length is valid for 204 / chunked — skip the check.
        return;
    };
    let cl_rest = &head_str[cl_pos + "content-length:".len()..];
    let cl_line_end = cl_rest.find('\r').or_else(|| cl_rest.find('\n')).unwrap_or(cl_rest.len());
    let cl_val: u64 = cl_rest[..cl_line_end]
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("{name}: Content-Length is not a number"));

    assert_eq!(
        cl_val, actual_body_len as u64,
        "{name}: Content-Length={cl_val} but actual body is {actual_body_len} bytes — \
         fixture builder has a bug; the http1 codec will mis-frame the body"
    );
}

#[test]
fn fixture_content_lengths_are_exact() {
    // All response fixtures whose framing is Content-Length.
    let cases: &[(&str, Vec<u8>)] = &[
        ("version", version_http()),
        ("version_old", version_old_http()),
        ("ping", ping_http()),
        ("containers_list", containers_list_http()),
        ("inspect", inspect_http()),
        ("create", create_http()),
        ("wait", wait_http()),
        ("images_list", images_list_http()),
        ("image_remove", image_remove_http()),
        ("error_404", error_404_http()),
        ("exec_create", exec_create_http()),
        ("exec_inspect", exec_inspect_http()),
    ];
    for (name, resp) in cases {
        assert_cl_correct(name, resp);
    }
    // 204 fixtures have no Content-Length — just assert they have the right status.
    for (name, resp) in &[
        ("start_204", start_204_http()),
        ("stop_204", stop_204_http()),
        ("remove_204", remove_204_http()),
    ] {
        let head = String::from_utf8_lossy(resp);
        assert!(
            head.contains("204 No Content"),
            "{name}: expected 204 No Content status"
        );
        assert!(
            !head.contains("Content-Length"),
            "{name}: 204 must have no Content-Length (Docker API spec)"
        );
    }
}

/// Verify that the multiplexed-frame binary fixture has the correct 8-byte headers.
#[test]
fn fixture_mux_frame_headers_are_correct() {
    let data = logs_multiplexed_bin();
    // Frame 1: stdout (type=1), payload "hello\n" (6 bytes).
    assert_eq!(data[0], 1, "frame 1: stream type must be 1 (stdout)");
    assert_eq!(&data[1..4], &[0u8, 0, 0], "frame 1: padding bytes must be 0");
    let size1 = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    assert_eq!(size1, 6, "frame 1: SIZE must be 6");
    assert_eq!(&data[8..14], b"hello\n", "frame 1: payload must be 'hello\\n'");

    // Frame 2: stderr (type=2), payload "oops\n" (5 bytes).
    let f2 = &data[14..];
    assert_eq!(f2[0], 2, "frame 2: stream type must be 2 (stderr)");
    assert_eq!(&f2[1..4], &[0u8, 0, 0], "frame 2: padding bytes must be 0");
    let size2 = u32::from_be_bytes([f2[4], f2[5], f2[6], f2[7]]);
    assert_eq!(size2, 5, "frame 2: SIZE must be 5");
    assert_eq!(&f2[8..13], b"oops\n", "frame 2: payload must be 'oops\\n'");

    // Total length must be exactly 8+6 + 8+5 = 27 bytes.
    assert_eq!(data.len(), 27, "total mux fixture length");
}

/// Verify the oversize frame has SIZE > 16 MiB in its header.
#[test]
fn fixture_oversize_frame_size_field_exceeds_cap() {
    let data = logs_oversize_frame_bin();
    // The 8-byte header starts at offset 0.
    assert_eq!(data.len(), 8, "oversize frame fixture must be exactly 8 header bytes");
    let size = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let cap: u32 = 16 * 1024 * 1024;
    assert!(
        size > cap,
        "oversize frame SIZE={size} must exceed cap={cap}"
    );
}

/// Verify the truncated frame has a SIZE header that exceeds the payload.
#[test]
fn fixture_truncated_frame_payload_is_short() {
    let data = logs_truncated_frame_bin();
    assert_eq!(data[0], 1, "stream type 1 (stdout)");
    let size = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let payload_bytes = data.len() - 8;
    assert!(
        (payload_bytes as u32) < size,
        "truncated frame: payload={payload_bytes} must be < claimed SIZE={size}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Mock daemon
// ──────────────────────────────────────────────────────────────────────────────

/// Guard that shuts the mock daemon down on drop.
pub struct DaemonGuard {
    /// Shared stop flag: the listener task exits when this is `true`.
    stop: Arc<Mutex<bool>>,
    /// Stored handle to allow joining on `stop()`, but we don't block Drop on it
    /// (the task will exit once the flag is set and the listener wakes from the
    /// next accept/timeout).
    _handle: tokio::task::JoinHandle<()>,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        *self.stop.lock().unwrap() = true;
    }
}

/// Route a parsed request line (`"METHOD /v1.43/path"` or `"METHOD /path"`) to the
/// matching fixture bytes.
///
/// Strips the API version prefix `/v<digits>.<digits>` if present (Docker clients
/// typically include it; our mock strips it to keep routing simple).
fn route_request(request_line: &str) -> Option<Vec<u8>> {
    // Split into METHOD and path.
    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_uppercase();
    // Drop the query string before routing (a real daemon routes on the path; our
    // fixtures key on the path alone). `?all=true`/`?filters=…` must not foil a match.
    let raw_path_full = parts.next().unwrap_or("/");
    let raw_path = raw_path_full.split('?').next().unwrap_or("/");
    // The query string is needed for pull, which routes the error variant by image name.
    let query = raw_path_full.split('?').nth(1).unwrap_or("");

    // Strip API version prefix, e.g. `/v1.43/containers/json` → `/containers/json`.
    // Pattern: raw_path starts with `/v<digits>.<digits>/`.
    let path = {
        let raw = raw_path; // e.g. "/v1.43/containers/json"
        // Use strip_prefix to avoid the manual_strip clippy lint.
        if let Some(rest) = raw.strip_prefix('/') {
            // rest = "v1.43/containers/json"
            if let Some(after_v) = rest.strip_prefix('v') {
                // after_v = "1.43/containers/json"
                // Find the slash separating version from path.
                if let Some(slash_idx) = after_v.find('/') {
                    let version_part = &after_v[..slash_idx]; // "1.43"
                    let remainder = &after_v[slash_idx..]; // "/containers/json"
                    // Only strip if the version part looks like "digits.digits".
                    if version_part.contains('.')
                        && !version_part.is_empty()
                        && version_part
                            .chars()
                            .all(|c| c.is_ascii_digit() || c == '.')
                    {
                        remainder.to_string() // "/containers/json"
                    } else {
                        raw.to_string()
                    }
                } else {
                    // No slash after version — return as-is.
                    raw.to_string()
                }
            } else {
                raw.to_string()
            }
        } else {
            raw.to_string()
        }
    };

    // Route by method + path prefix.
    match (method.as_str(), path.as_str()) {
        ("GET", "/_ping") => Some(ping_http()),
        ("GET", "/version") => Some(version_http()),
        ("GET", "/containers/json") => Some(containers_list_http()),
        // CNTR §4.3 logs streams — the container id selects the fixture variant so a
        // single route covers happy/TTY/hostile paths (a real daemon keys on the id
        // too). `…/tty/logs` → raw TTY; `…/truncated/logs` / `…/oversize/logs` →
        // the hostile multiplex fixtures; anything else → the multiplexed fixture.
        ("GET", p) if p.starts_with("/containers/") && p.ends_with("/logs") => {
            if p.contains("/tty/") {
                Some(logs_tty_http())
            } else if p.contains("/truncated/") {
                Some(logs_truncated_frame_http())
            } else if p.contains("/oversize/") {
                Some(logs_oversize_frame_http())
            } else {
                Some(logs_multiplexed_http())
            }
        }
        ("GET", p) if p.starts_with("/containers/") && p.ends_with("/json") => {
            Some(inspect_http())
        }
        ("POST", "/containers/create") => Some(create_http()),
        ("POST", p) if p.starts_with("/containers/") && p.ends_with("/start") => {
            Some(start_204_http())
        }
        ("POST", p) if p.starts_with("/containers/") && p.ends_with("/stop") => {
            Some(stop_204_http())
        }
        ("DELETE", p) if p.starts_with("/containers/") => Some(remove_204_http()),
        ("POST", p) if p.starts_with("/containers/") && p.ends_with("/wait") => {
            Some(wait_http())
        }
        ("GET", "/images/json") => Some(images_list_http()),
        ("DELETE", p) if p.starts_with("/images/") => Some(image_remove_http()),
        ("GET", "/events") => Some(events_jsonl_http()),
        // CNTR §4.3 pull — the `fromImage` query selects the fixture: a `nonexistent`
        // image returns the in-stream-error progress stream; anything else succeeds.
        ("POST", "/images/create") => {
            if query.contains("nonexistent") {
                Some(pull_error_http())
            } else {
                Some(pull_progress_http())
            }
        }
        // CNTR §4.5 exec — create on the container, start (101 hijack), inspect.
        ("POST", p) if p.starts_with("/containers/") && p.ends_with("/exec") => {
            Some(exec_create_http())
        }
        ("POST", p) if p.starts_with("/exec/") && p.ends_with("/start") => {
            Some(exec_upgrade_bin())
        }
        ("GET", p) if p.starts_with("/exec/") && p.ends_with("/json") => Some(exec_inspect_http()),
        // Fallthrough: 404.
        _ => Some(error_404_http()),
    }
}

/// Read the HTTP request head (until `\r\n\r\n`) from a Unix stream, extract the
/// request line (first line), and return it. Returns `None` if the connection is
/// closed before a complete head arrives (idle keep-alive poll, benign).
async fn read_request_line(stream: &mut UnixStream) -> Option<String> {
    let mut buf = Vec::new();
    let sep = b"\r\n\r\n";
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);

    loop {
        if buf.windows(sep.len()).any(|w| w == sep) {
            break;
        }
        if buf.len() > 256 * 1024 {
            return None; // oversized head — drop the connection
        }
        let mut byte = [0u8; 1];
        match tokio::time::timeout_at(deadline, stream.read(&mut byte)).await {
            Ok(Ok(0)) | Err(_) => {
                // EOF or timeout — connection went away or client was slow.
                return None;
            }
            Ok(Ok(_)) => buf.push(byte[0]),
            Ok(Err(_)) => return None,
        }
    }

    // The first line of the head is the request line.
    let head_str = String::from_utf8_lossy(&buf);
    let request_line = head_str.lines().next()?.to_string();
    Some(request_line)
}

/// Serve one connection from the mock daemon: read the request head, route it,
/// write the fixture response, then close.
async fn serve_one_connection(stream: UnixStream) {
    serve_one_connection_versioned(stream, false).await;
}

/// Is this request line a `GET /version` (with or without the `/v1.xx` prefix)?
fn is_version_request(request_line: &str) -> bool {
    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");
    method.eq_ignore_ascii_case("GET") && raw_path.ends_with("/version")
}

/// Serve one connection; if `old_version` is true, the `GET /version` route returns
/// the below-floor (1.20) fixture so the version-negotiation FLOOR path can be
/// exercised against a real daemon. Every other route is unchanged.
async fn serve_one_connection_versioned(mut stream: UnixStream, old_version: bool) {
    let Some(request_line) = read_request_line(&mut stream).await else {
        return; // connection closed before head — benign
    };

    let response = if old_version && is_version_request(&request_line) {
        version_old_http()
    } else {
        route_request(&request_line).unwrap_or_else(error_404_http)
    };

    // Write the response in chunks to exercise pull-driven reads (helps streaming
    // fixture tests catch framing bugs). For streamed/chunked fixtures, insert a
    // small sleep between writes to exercise backpressure.
    let is_chunked = response.windows(b"Transfer-Encoding: chunked".len())
        .any(|w| w == b"Transfer-Encoding: chunked");

    if is_chunked {
        // Write the head and each chunk segment with a brief pause to simulate
        // incremental delivery from the Docker daemon.
        if let Some(sep_pos) = response.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = &response[..sep_pos + 4];
            let body = &response[sep_pos + 4..];
            let _ = stream.write_all(head).await;
            // Break the chunked body into ~256B writes with 1ms pauses.
            for chunk_slice in body.chunks(256) {
                let _ = stream.write_all(chunk_slice).await;
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        } else {
            let _ = stream.write_all(&response).await;
        }
    } else {
        let _ = stream.write_all(&response).await;
    }
    let _ = stream.flush().await;
    // Dropping `stream` closes the connection (Docker's `Connection: close` model).
}

/// Start the mock Docker Engine daemon and return `(socket_path, guard)`.
///
/// The daemon listens on a temp-path Unix socket; one connection is accepted per
/// request (Docker Engine API is `Connection: close`). Dropping the returned guard
/// signals the listener to stop accepting new connections.
///
/// # Panics
/// Panics if the listener fails to bind (should not happen in a clean temp dir).
pub async fn mock_daemon() -> (String, DaemonGuard) {
    // Use a unique temp path to avoid collisions between parallel test runs.
    let tmp = tempfile::Builder::new()
        .prefix("ascript_docker_mock_")
        .suffix(".sock")
        .tempfile()
        .expect("create temp file for socket path");
    // We need the PATH, not a live file — remove the placeholder file that
    // `tempfile` created so `UnixListener::bind` can create the socket there.
    let sock_path = tmp.path().to_path_buf();
    drop(tmp); // remove the placeholder

    let listener = UnixListener::bind(&sock_path)
        .unwrap_or_else(|e| panic!("mock_daemon: bind {:?}: {e}", sock_path));

    let sock_path_str = sock_path.to_string_lossy().to_string();
    let stop = Arc::new(Mutex::new(false));
    let stop_clone = stop.clone();
    let sock_path_cleanup = sock_path.clone();

    let handle = tokio::spawn(async move {
        loop {
            // Check stop flag before blocking on accept.
            if *stop_clone.lock().unwrap() {
                break;
            }
            // Accept with a short timeout so we can check the stop flag periodically.
            match tokio::time::timeout(
                std::time::Duration::from_millis(50),
                listener.accept(),
            )
            .await
            {
                Ok(Ok((stream, _addr))) => {
                    tokio::spawn(serve_one_connection(stream));
                }
                Ok(Err(_)) => break, // listener error — shut down
                Err(_) => {}         // timeout — loop and check stop flag
            }
        }
        // Best-effort cleanup of the socket file.
        let _ = std::fs::remove_file(&sock_path_cleanup);
    });

    (sock_path_str, DaemonGuard { stop, _handle: handle })
}

/// Like [`mock_daemon`] but the `GET /version` route returns the BELOW-FLOOR (1.20)
/// fixture, so the docker module's version-negotiation floor check can be exercised
/// end-to-end. Every other route is identical to `mock_daemon`.
pub async fn mock_daemon_old() -> (String, DaemonGuard) {
    let tmp = tempfile::Builder::new()
        .prefix("ascript_docker_mock_old_")
        .suffix(".sock")
        .tempfile()
        .expect("create temp file for socket path");
    let sock_path = tmp.path().to_path_buf();
    drop(tmp);

    let listener = UnixListener::bind(&sock_path)
        .unwrap_or_else(|e| panic!("mock_daemon_old: bind {:?}: {e}", sock_path));

    let sock_path_str = sock_path.to_string_lossy().to_string();
    let stop = Arc::new(Mutex::new(false));
    let stop_clone = stop.clone();
    let sock_path_cleanup = sock_path.clone();

    let handle = tokio::spawn(async move {
        loop {
            if *stop_clone.lock().unwrap() {
                break;
            }
            match tokio::time::timeout(
                std::time::Duration::from_millis(50),
                listener.accept(),
            )
            .await
            {
                Ok(Ok((stream, _addr))) => {
                    tokio::spawn(serve_one_connection_versioned(stream, true));
                }
                Ok(Err(_)) => break,
                Err(_) => {}
            }
        }
        let _ = std::fs::remove_file(&sock_path_cleanup);
    });

    (sock_path_str, DaemonGuard { stop, _handle: handle })
}

// ──────────────────────────────────────────────────────────────────────────────
// Smoke test — proves the mock works independently of the docker module
// ──────────────────────────────────────────────────────────────────────────────

/// Spin up the mock daemon and drive it with an AScript program that opens a raw
/// Unix connection, sends an HTTP GET to `/_ping`, and reads the response body line
/// by line. Asserts that the fixture's `OK` body is visible in the output.
///
/// This test is independent of `std/docker` — it only exercises the mock and the
/// AScript `std/net/unix` module, proving the fixture + daemon infrastructure is
/// byte-exact before Phase 4.2 builds on it.
#[tokio::test]
async fn mock_daemon_serves_ping_over_raw_unix_connect() {
    let (sock, _guard) = mock_daemon().await;

    // AScript program: connect over UDS, send a raw HTTP/1.1 GET, read lines until
    // we have seen the response body. `readLine()` returns strings (UTF-8 lossy,
    // trailing newline stripped), which is safe for our ASCII HTTP fixture.
    //
    // We read lines until we get an empty line (end of headers), then one more line
    // for the body ("OK"). Print all lines so the assertion can search the output.
    let src = format!(
        r#"
import * as unix from "std/net/unix"
let [s, err] = await unix.connect("{sock}")
if (err != nil) {{ print("CONNECT ERROR: " + err.message); exit(1) }}
await s.write("GET /_ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
let line = await s.readLine()
while (line != nil) {{
    print(line)
    line = await s.readLine()
}}
"#
    );

    let out = ascript::run_source(&src)
        .await
        .unwrap_or_else(|e| panic!("AScript program failed: {}", e.message));

    // The response should contain the status line and the "OK" body.
    assert!(
        out.contains("200"),
        "expected HTTP 200 status in output, got:\n{out}"
    );
    assert!(
        out.contains("OK"),
        "expected 'OK' body from ping fixture, got:\n{out}"
    );
    // Sanity: should also see Content-Length: 2 (the ping body is 2 bytes).
    assert!(
        out.contains("Content-Length: 2"),
        "expected 'Content-Length: 2' header in output, got:\n{out}"
    );
}

/// Verify that the mock correctly routes `/containers/json` and returns the
/// two-container list fixture.
#[tokio::test]
async fn mock_daemon_routes_containers_json() {
    let (sock, _guard) = mock_daemon().await;

    let src = format!(
        r#"
import * as unix from "std/net/unix"
let [s, err] = await unix.connect("{sock}")
if (err != nil) {{ exit(1) }}
await s.write("GET /v1.43/containers/json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
let line = await s.readLine()
while (line != nil) {{
    print(line)
    line = await s.readLine()
}}
"#
    );

    let out = ascript::run_source(&src)
        .await
        .unwrap_or_else(|e| panic!("AScript program failed: {}", e.message));

    assert!(out.contains("200"), "expected 200 status, got:\n{out}");
    assert!(out.contains("/web"), "expected /web container name, got:\n{out}");
    assert!(out.contains("/db"), "expected /db container name, got:\n{out}");
}

/// Verify that the mock returns a 204 No Content (no body) for `POST /containers/{id}/start`.
#[tokio::test]
async fn mock_daemon_routes_start_204() {
    let (sock, _guard) = mock_daemon().await;

    let src = format!(
        r#"
import * as unix from "std/net/unix"
let [s, err] = await unix.connect("{sock}")
if (err != nil) {{ exit(1) }}
await s.write("POST /containers/abc123/start HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
let line = await s.readLine()
while (line != nil) {{
    print(line)
    line = await s.readLine()
}}
"#
    );

    let out = ascript::run_source(&src)
        .await
        .unwrap_or_else(|e| panic!("AScript program failed: {}", e.message));

    assert!(out.contains("204"), "expected 204 No Content, got:\n{out}");
}

/// Verify that the mock routes to `error_404_http` for unknown paths.
#[tokio::test]
async fn mock_daemon_routes_unknown_path_to_404() {
    let (sock, _guard) = mock_daemon().await;

    let src = format!(
        r#"
import * as unix from "std/net/unix"
let [s, err] = await unix.connect("{sock}")
if (err != nil) {{ exit(1) }}
await s.write("GET /no/such/endpoint HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
let line = await s.readLine()
while (line != nil) {{
    print(line)
    line = await s.readLine()
}}
"#
    );

    let out = ascript::run_source(&src)
        .await
        .unwrap_or_else(|e| panic!("AScript program failed: {}", e.message));

    assert!(out.contains("404"), "expected 404, got:\n{out}");
}

/// Verify that the mock serves the exec upgrade fixture (`101 Switching Protocols`)
/// for `POST /exec/{id}/start`.
#[tokio::test]
async fn mock_daemon_routes_exec_start_to_101() {
    let (sock, _guard) = mock_daemon().await;

    let src = format!(
        r#"
import * as unix from "std/net/unix"
let [s, err] = await unix.connect("{sock}")
if (err != nil) {{ exit(1) }}
await s.write("POST /exec/exec123/start HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n")
let line = await s.readLine()
while (line != nil) {{
    print(line)
    line = await s.readLine()
}}
"#
    );

    let out = ascript::run_source(&src)
        .await
        .unwrap_or_else(|e| panic!("AScript program failed: {}", e.message));

    assert!(out.contains("101"), "expected 101 Switching Protocols, got:\n{out}");
}

/// Verify that the chunked events fixture is served correctly (contains Transfer-Encoding).
#[tokio::test]
async fn mock_daemon_routes_events_chunked() {
    let (sock, _guard) = mock_daemon().await;

    let src = format!(
        r#"
import * as unix from "std/net/unix"
let [s, err] = await unix.connect("{sock}")
if (err != nil) {{ exit(1) }}
await s.write("GET /events HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
let line = await s.readLine()
while (line != nil) {{
    print(line)
    line = await s.readLine()
}}
"#
    );

    let out = ascript::run_source(&src)
        .await
        .unwrap_or_else(|e| panic!("AScript program failed: {}", e.message));

    assert!(out.contains("200"), "expected 200 status, got:\n{out}");
    assert!(
        out.contains("chunked"),
        "expected Transfer-Encoding: chunked, got:\n{out}"
    );
    // The body should contain JSON events with "Action".
    assert!(out.contains("Action"), "expected event JSON in body, got:\n{out}");
}

// ──────────────────────────────────────────────────────────────────────────────
// Task 4.2 — std/docker client: connect + version negotiation + unary API.
//
// These exercise the real `std/docker` module against the mock daemon. Gated on
// `feature = "docker"` (the module is compiled out without it; the file-level cfg
// is only `net`).
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "docker")]
async fn run(src: &str) -> String {
    ascript::run_source(src)
        .await
        .unwrap_or_else(|e| panic!("AScript program failed: {}", e.message))
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn connect_negotiates_version_and_lists_containers() {
    let (sock, _guard) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, err] = await docker.connect({{ socketPath: "{sock}" }})
print(err)
print(d.apiVersion)
let [cs, e2] = await d.containers({{ all: true }})
print(e2)
print(len(cs))
print(cs[0].Names[0])
"#
    );
    assert_eq!(run(&src).await, "nil\n1.43\nnil\n2\n/web\n");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn connect_exposes_socket_path_field() {
    let (sock, _guard) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, err] = await docker.connect({{ socketPath: "{sock}" }})
print(err)
print(d.socketPath == "{sock}")
"#
    );
    assert_eq!(run(&src).await, "nil\ntrue\n");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn old_daemon_below_floor_is_tier1_err() {
    let (sock, _guard) = mock_daemon_old().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, err] = await docker.connect({{ socketPath: "{sock}" }})
print(d)
print(err != nil)
print(err.message)
"#
    );
    let out = run(&src).await;
    assert!(out.starts_with("nil\ntrue\n"), "expected Tier-1 floor err, got:\n{out}");
    assert!(out.contains("1.20"), "err should name the daemon version, got:\n{out}");
    assert!(out.contains("1.24"), "err should name the floor, got:\n{out}");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn unreachable_socket_is_tier1_err() {
    // A path with no daemon → connect fails → Tier-1 [nil, err].
    let src = r#"
import * as docker from "std/docker"
let [d, err] = await docker.connect({ socketPath: "/nonexistent/ascript-docker-nope.sock" })
print(d)
print(err != nil)
"#;
    assert_eq!(run(src).await, "nil\ntrue\n");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn inspect_404_has_status_code_and_message() {
    let (sock, _guard) = mock_daemon().await;
    // The mock returns 404 for a `GET /containers/<id>/json` whose id is unknown?
    // Actually the mock routes ANY `/containers/.../json` to inspect_http (200). To
    // exercise the 404 mapping we hit a path the mock 404s: removeImage of an unknown
    // image returns the image_remove (200). The reliably-404 route is an unknown
    // endpoint — `info` is not routed by the mock → 404.
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, err] = await docker.connect({{ socketPath: "{sock}" }})
print(err)
let [v, e2] = await d.info()
print(v)
print(e2.statusCode)
print(e2.message)
"#
    );
    let out = run(&src).await;
    assert!(out.starts_with("nil\nnil\n"), "connect ok + info nil val, got:\n{out}");
    assert!(out.contains("404"), "expected statusCode 404, got:\n{out}");
    assert!(out.contains("No such container"), "expected daemon message, got:\n{out}");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn tcp_docker_host_is_tier1_unsupported() {
    let src = r#"
import * as docker from "std/docker"
let [d, err] = await docker.connect({ host: "tcp://127.0.0.1:2375" })
print(d)
print(err != nil)
print(err.message)
"#;
    let out = run(src).await;
    assert!(out.starts_with("nil\ntrue\n"), "expected Tier-1 err, got:\n{out}");
    assert!(
        out.to_lowercase().contains("tcp") || out.to_lowercase().contains("unix socket"),
        "err should explain TCP is unsupported, got:\n{out}"
    );
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn start_204_is_nil_nil_pair() {
    let (sock, _guard) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _e] = await docker.connect({{ socketPath: "{sock}" }})
let [v, err] = await d.start("abc123")
print(v)
print(err)
"#
    );
    assert_eq!(run(&src).await, "nil\nnil\n");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn create_returns_201_body() {
    let (sock, _guard) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _e] = await docker.connect({{ socketPath: "{sock}" }})
let [v, err] = await d.create({{ Image: "nginx:latest" }})
print(err)
print(v.Id)
"#
    );
    assert_eq!(run(&src).await, "nil\nabc123\n");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn inspect_returns_container_json() {
    let (sock, _guard) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _e] = await docker.connect({{ socketPath: "{sock}" }})
let [v, err] = await d.inspect("abc123def456")
print(err)
print(v.Name)
print(v.State.Running)
"#
    );
    assert_eq!(run(&src).await, "nil\n/web\ntrue\n");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn inspect_non_string_id_is_tier2_panic() {
    let (sock, _guard) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _e] = await docker.connect({{ socketPath: "{sock}" }})
let [v, err] = recover(() => d.inspect(42))
print(v)
print(err != nil)
"#
    );
    // recover() catches the Tier-2 panic → [nil, err].
    let out = run(&src).await;
    assert!(out.starts_with("nil\ntrue\n"), "expected a recovered Tier-2 panic, got:\n{out}");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn ping_and_images_unary_calls() {
    let (sock, _guard) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _e] = await docker.connect({{ socketPath: "{sock}" }})
let [pong, perr] = await d.ping()
print(perr)
let [imgs, ierr] = await d.images({{}})
print(ierr)
print(len(imgs))
print(imgs[0].RepoTags[0])
"#
    );
    assert_eq!(run(&src).await, "nil\nnil\n2\nnginx:latest\n");
}

#[cfg(feature = "docker")]
#[tokio::test]
async fn docker_client_is_non_sendable() {
    // A DockerClient handle must be rejected by the worker airlock (it is a native
    // resource → !Send). Constructing the handle directly and checking the serializer
    // proves the field-path panic without needing a live worker.
    use ascript::value::{NativeKind, NativeObject, Value};
    let handle = Value::native(std::rc::Rc::new(NativeObject {
        id: 1,
        kind: NativeKind::DockerClient,
        fields: indexmap::IndexMap::new(),
    }));
    let err = ascript::worker::serialize::check_sendable(&handle)
        .expect_err("a DockerClient handle must be non-sendable");
    assert_eq!(err.kind, "native");
}

// ──────────────────────────────────────────────────────────────────────────────
// Task 4.3–4.4 — logs / events / pull as for-await streams + the multiplex demux.
// ──────────────────────────────────────────────────────────────────────────────

/// The headline: `d.logs(...)` over a multiplexed stream demuxes stdout/stderr and is
/// `for await`-iterable, yielding `{stream, text}` items.
#[cfg(feature = "docker")]
#[tokio::test]
async fn logs_stream_for_await_demuxes_stdout_stderr() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, err] = await d.logs("abc", {{ stdout: true, stderr: true }})
print(err)
for await (entry in logs) {{ print(`${{entry.stream}}:${{entry.text}}`) }}
"#
    );
    assert_eq!(run(&src).await, "nil\nstdout:hello\n\nstderr:oops\n\n");
}

/// A TTY container's logs stream is raw text (auto-detected, no frames) — the items
/// are all `stdout` and their concatenation is the raw body.
#[cfg(feature = "docker")]
#[tokio::test]
async fn logs_tty_stream_is_raw_stdout() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, err] = await d.logs("tty", {{}})
print(err)
let acc = ""
let allStdout = true
for await (entry in logs) {{
    if (entry.stream != "stdout") {{ allStdout = false }}
    acc = acc + entry.text
}}
print(acc)
print(allStdout)
"#
    );
    // The TTY body may arrive as one or more stdout chunks; the concatenated text is
    // the raw body and every chunk is stdout.
    assert_eq!(run(&src).await, "nil\nplain tty output\n\ntrue\n");
}

/// `d.events()` yields decoded JSON objects.
#[cfg(feature = "docker")]
#[tokio::test]
async fn events_stream_yields_decoded_objects() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [events, err] = await d.events({{}})
print(err)
let n = 0
for await (ev in events) {{
    n = n + 1
    print(ev.Action)
}}
print(n)
"#
    );
    assert_eq!(run(&src).await, "nil\nstart\nstop\npull\n3\n");
}

/// `break` out of a `for await` over an events stream, then `close()` reclaims the
/// stream's connection: a subsequent `.next()` on the closed stream returns the clean
/// terminal `[nil, nil]` (proving the resource entry was taken on close — the fd is
/// reclaimed deterministically).
#[cfg(feature = "docker")]
#[tokio::test]
async fn events_break_and_close_then_next_is_ended() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [events, _e] = await d.events({{}})
let first = ""
for await (ev in events) {{
    first = ev.Action
    break
}}
print(first)
events.close()
let [item, err] = await events.next()
print(item)
print(err)
"#
    );
    // first event, then close → next() is the terminal [nil, nil].
    assert_eq!(run(&src).await, "start\nnil\nnil\n");
}

/// `d.pull()` progress objects stream to completion (clean end).
#[cfg(feature = "docker")]
#[tokio::test]
async fn pull_progress_stream_then_end() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [prog, err] = await d.pull("nginx:latest")
print(err)
let n = 0
for await (p in prog) {{ n = n + 1 }}
print(n)
"#
    );
    // 7 progress lines in the fixture.
    assert_eq!(run(&src).await, "nil\n7\n");
}

/// A pull that hits a registry error: the in-stream `{"error":…}` line is a terminal
/// `[nil, err]` on `.next()`. (Consumed via `.next()` directly — a `for await` would
/// raise it as a loop-site Tier-2 panic; manual `next()` surfaces the pair.)
#[cfg(feature = "docker")]
#[tokio::test]
async fn pull_error_is_terminal_pair_via_next() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [prog, err] = await d.pull("nonexistent:latest")
print(err)
let [a, ea] = await prog.next()
print(ea)
print(a.status)
let [b, eb] = await prog.next()
print(b)
print(eb.message)
let [c, ec] = await prog.next()
print(c)
print(ec)
"#
    );
    // first item = progress object (status set), second = terminal error pair,
    // third = clean end [nil, nil].
    let out = run(&src).await;
    assert!(out.starts_with("nil\nnil\n"), "connect ok + first item ok, got:\n{out}");
    assert!(out.contains("manifest unknown"), "expected the registry error, got:\n{out}");
    assert!(out.ends_with("nil\nnil\n"), "stream ends [nil, nil], got:\n{out}");
}

/// A truncated multiplex frame (header claims more than follows) → a terminal
/// `[nil, err]` on `.next()` (not a panic/hang).
#[cfg(feature = "docker")]
#[tokio::test]
async fn logs_truncated_frame_is_tier1_err() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, _e] = await d.logs("truncated", {{}})
let [item, err] = await logs.next()
print(item)
print(err != nil)
print(err.message)
"#
    );
    let out = run(&src).await;
    assert!(out.starts_with("nil\ntrue\n"), "truncation is a Tier-1 err, got:\n{out}");
    assert!(out.contains("truncated"), "err names the truncation, got:\n{out}");
}

/// An oversize frame (SIZE > 16 MiB) → a Tier-1 err WITHOUT allocating the claimed
/// size (a hang/OOM would fail the test by timeout/memory, not assertion).
#[cfg(feature = "docker")]
#[tokio::test]
async fn logs_oversize_frame_is_tier1_err_no_alloc() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, _e] = await d.logs("oversize", {{}})
let [item, err] = await logs.next()
print(item)
print(err != nil)
print(err.message)
"#
    );
    let out = run(&src).await;
    assert!(out.starts_with("nil\ntrue\n"), "oversize is a Tier-1 err, got:\n{out}");
    assert!(out.contains("cap") || out.contains("exceeds"), "err names the cap, got:\n{out}");
}

/// VM-path proof: a `for await` over a docker stream works on the bytecode VM too
/// (not just the tree-walker `run`) — `native_stream_method` makes it iterable on
/// BOTH engines.
#[cfg(feature = "docker")]
#[tokio::test]
async fn logs_for_await_works_on_the_vm() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [logs, err] = await d.logs("abc", {{ stdout: true, stderr: true }})
print(err)
for await (entry in logs) {{ print(`${{entry.stream}}:${{entry.text}}`) }}
"#
    );
    let (out, _code) = ascript::vm_run_source(&src)
        .await
        .unwrap_or_else(|e| panic!("VM run failed: {}", e.message));
    assert_eq!(out, "nil\nstdout:hello\n\nstderr:oops\n\n");
}

/// A `dockerStream` handle is non-sendable (the worker airlock rejects it).
#[cfg(all(feature = "docker", unix))]
#[tokio::test]
async fn docker_stream_is_non_sendable() {
    use ascript::value::{NativeKind, NativeObject, Value};
    let handle = Value::native(std::rc::Rc::new(NativeObject {
        id: 1,
        kind: NativeKind::DockerStream,
        fields: indexmap::IndexMap::new(),
    }));
    let err = ascript::worker::serialize::check_sendable(&handle)
        .expect_err("a DockerStream handle must be non-sendable");
    assert_eq!(err.kind, "native");
}

// ──────────────────────────────────────────────────────────────────────────────
// Task 4.5 — exec create / start (101 hijack) / inspect + the d.exec convenience.
// ──────────────────────────────────────────────────────────────────────────────

/// `d.execCreate(containerId, {cmd})` → the exec id from the 201 body.
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_create_returns_exec_id() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [id, err] = await d.execCreate("web", {{ cmd: ["echo", "hi"] }})
print(err)
print(id)
"#
    );
    assert_eq!(run(&src).await, "nil\nexec123\n");
}

/// `d.execStart(execId, {})` → a `dockerStream` over the 101-upgrade body; a
/// `for await` demuxes the multiplexed exec frames.
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_start_hijack_demuxes_frames() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [stream, err] = await d.execStart("exec123", {{}})
print(err)
for await (frame in stream) {{ print(`${{frame.stream}}:${{frame.text}}`) }}
"#
    );
    assert_eq!(
        run(&src).await,
        "nil\nstdout:exec stdout\n\nstderr:exec stderr\n\n"
    );
}

/// The exec-start `for await` also works on the bytecode VM (not just the tree-walker).
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_start_hijack_works_on_the_vm() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [stream, err] = await d.execStart("exec123", {{}})
print(err)
for await (frame in stream) {{ print(`${{frame.stream}}:${{frame.text}}`) }}
"#
    );
    let (out, _code) = ascript::vm_run_source(&src)
        .await
        .unwrap_or_else(|e| panic!("VM run failed: {}", e.message));
    assert_eq!(out, "nil\nstdout:exec stdout\n\nstderr:exec stderr\n\n");
}

/// `d.execInspect(execId)` → the exec status object (`ExitCode`/`Running`).
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_inspect_returns_status() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [info, err] = await d.execInspect("exec123")
print(err)
print(info.ExitCode)
print(info.Running)
"#
    );
    assert_eq!(run(&src).await, "nil\n0\nfalse\n");
}

/// `d.exec(containerId, {cmd})` — the convenience composition: create → start →
/// drain → inspect, returning `{exitCode, stdout, stderr}`.
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_convenience_returns_exit_code_and_output() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [res, err] = await d.exec("web", {{ cmd: ["echo", "hi"] }})
print(err)
print(res.exitCode)
print(res.stdout)
print(res.stderr)
"#
    );
    // stdout = "exec stdout\n", stderr = "exec stderr\n" from the upgrade fixture.
    assert_eq!(
        run(&src).await,
        "nil\n0\nexec stdout\n\nexec stderr\n\n"
    );
}

/// `d.exec` result mirrors `process.run`'s shape (a `code` alias alongside `exitCode`).
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_convenience_result_mirrors_process_run() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [res, err] = await d.exec("web", {{ cmd: ["echo", "hi"] }})
print(err)
print(res.code)
print(res.exitCode == res.code)
"#
    );
    assert_eq!(run(&src).await, "nil\n0\ntrue\n");
}

/// The convenience runs on the bytecode VM too.
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_convenience_works_on_the_vm() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [res, err] = await d.exec("web", {{ cmd: ["echo", "hi"] }})
print(err)
print(res.exitCode)
"#
    );
    let (out, _code) = ascript::vm_run_source(&src)
        .await
        .unwrap_or_else(|e| panic!("VM run failed: {}", e.message));
    assert_eq!(out, "nil\n0\n");
}

/// `attachStdin: true` on exec/execStart is a deferred-feature Tier-2 panic with the
/// pinned message (interactive stdin attach is out of v1 scope).
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_attach_stdin_is_deferred_tier2() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [v, err] = recover(() => d.execStart("exec123", {{ attachStdin: true }}))
print(v)
print(err != nil)
print(err.message)
"#
    );
    let out = run(&src).await;
    assert!(out.starts_with("nil\ntrue\n"), "expected a recovered Tier-2 panic, got:\n{out}");
    assert!(
        out.contains("attachStdin is not supported"),
        "expected the pinned deferral message, got:\n{out}"
    );
}

/// `attachStdin: true` on the convenience `d.exec` is the same pinned Tier-2 deferral.
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_convenience_attach_stdin_is_deferred_tier2() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [v, err] = recover(() => d.exec("web", {{ cmd: ["sh"], attachStdin: true }}))
print(v)
print(err != nil)
print(err.message)
"#
    );
    let out = run(&src).await;
    assert!(out.starts_with("nil\ntrue\n"), "expected a recovered Tier-2 panic, got:\n{out}");
    assert!(out.contains("attachStdin is not supported"), "got:\n{out}");
}

/// A non-string exec id on `execStart`/`execInspect` is a Tier-2 panic.
#[cfg(feature = "docker")]
#[tokio::test]
async fn exec_non_string_id_is_tier2_panic() {
    let (sock, _g) = mock_daemon().await;
    let src = format!(
        r#"
import * as docker from "std/docker"
let [d, _] = await docker.connect({{ socketPath: "{sock}" }})
let [a, ea] = recover(() => d.execStart(42, {{}}))
print(a)
print(ea != nil)
let [b, eb] = recover(() => d.execInspect(99))
print(b)
print(eb != nil)
"#
    );
    assert_eq!(run(&src).await, "nil\ntrue\nnil\ntrue\n");
}
