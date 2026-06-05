//! A tiny in-process HTTP/1.1 mock server for the SP11 `std/ai` fixture-replay
//! tests. NO real network, NO secrets: it binds `127.0.0.1:0` (an ephemeral
//! loopback port), reads-and-discards each request, and writes back a recorded
//! response body (JSON for non-streaming, an SSE event stream for streaming). A
//! genai `ServiceTargetResolver` points every request at this server's base URL,
//! so genai's real reqwest path is exercised end-to-end against recorded bytes.
//!
//! It runs on a dedicated `std::thread` with a blocking `TcpListener` so it works
//! identically whether the test drives a current-thread runtime + `LocalSet` (the
//! spike) or `ascript::run_source` (the .as integration tests). `#![allow(dead_code)]`
//! because each test file pulls in only the parts it needs.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// A recorded response a `MockServer` serves. Each request consumes the NEXT
/// fixture in order (so a multi-turn tool exchange records turn-1 then turn-2).
#[derive(Clone)]
pub struct Fixture {
    body: Vec<u8>,
    content_type: &'static str,
    status: u16,
    /// For SSE: send `Content-Type: text/event-stream` and stream the body as-is.
    sse: bool,
}

impl Fixture {
    /// A `200 OK application/json` fixture from a body string.
    pub fn json(body: &str) -> Self {
        Fixture {
            body: body.as_bytes().to_vec(),
            content_type: "application/json",
            status: 200,
            sse: false,
        }
    }

    /// A `200 OK text/event-stream` fixture (SSE deltas) from a body string.
    pub fn sse(body: &str) -> Self {
        Fixture {
            body: body.as_bytes().to_vec(),
            content_type: "text/event-stream",
            status: 200,
            sse: true,
        }
    }

    /// An error-status JSON fixture (e.g. a provider 400/429/500 body).
    pub fn json_status(status: u16, body: &str) -> Self {
        Fixture {
            body: body.as_bytes().to_vec(),
            content_type: "application/json",
            status,
            sse: false,
        }
    }

    fn reason(&self) -> &'static str {
        match self.status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            _ => "Status",
        }
    }
}

/// A running mock HTTP server. Drop or call [`MockServer::stop`] to shut it down.
pub struct MockServer {
    base_url: String,
    stop_flag: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockServer {
    /// Start a mock server that replays `fixtures` in order, one per request
    /// (cycling back to the first once exhausted, so a server is robust to genai
    /// retries). Returns once the listener is bound.
    pub fn start(fixtures: Vec<Fixture>) -> Self {
        assert!(!fixtures.is_empty(), "mock server needs at least one fixture");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{}", addr);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop_flag.clone();

        // A non-blocking-ish accept loop: set a short read/accept timeout so the
        // thread can observe the stop flag and exit promptly.
        listener
            .set_nonblocking(false)
            .expect("listener blocking mode");

        let handle = std::thread::spawn(move || {
            let mut next = 0usize;
            // Use accept timeouts via set_read_timeout per-stream; for accept,
            // poll with a short sleep loop driven by a cloned non-blocking listener.
            listener.set_nonblocking(true).expect("nonblocking listener");
            loop {
                if stop_for_thread.load(Ordering::Relaxed) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let fixture = fixtures[next % fixtures.len()].clone();
                        next += 1;
                        serve_one(stream, &fixture);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        MockServer {
            base_url,
            stop_flag,
            handle: Some(handle),
        }
    }

    /// Convenience: a single-fixture server.
    pub fn start_blocking(fixture: Fixture) -> Self {
        Self::start(vec![fixture])
    }

    /// The `http://127.0.0.1:<port>` base URL to point a resolver at.
    pub fn base_url(&self) -> String {
        self.base_url.clone()
    }

    /// Signal the accept loop to stop and join the thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Read the request (headers + body) far enough to be polite, then write the
/// fixture response. Blocking, set a read timeout so a hung client can't wedge us.
fn serve_one(mut stream: TcpStream, fixture: &Fixture) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(500)))
        .ok();
    stream.set_nonblocking(false).ok();

    // Read until end-of-headers; then drain Content-Length bytes if present.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                    header_end = Some(pos + 4);
                    break;
                }
                if buf.len() > 1 << 20 {
                    break; // 1 MiB header cap — defensive
                }
            }
            Err(_) => break,
        }
    }
    // Drain the declared request body so the client's write fully completes
    // before we respond (reqwest is tolerant, but this avoids RST races).
    if let Some(he) = header_end {
        let headers = String::from_utf8_lossy(&buf[..he]).to_ascii_lowercase();
        if let Some(len) = content_length(&headers) {
            let have = buf.len() - he;
            let mut remaining = len.saturating_sub(have);
            while remaining > 0 {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => remaining = remaining.saturating_sub(n),
                    Err(_) => break,
                }
            }
        }
    }

    let mut resp = Vec::new();
    let _ = write!(
        resp,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        fixture.status,
        fixture.reason(),
        fixture.content_type,
        fixture.body.len()
    );
    if fixture.sse {
        let _ = write!(resp, "Cache-Control: no-cache\r\n");
    }
    let _ = write!(resp, "\r\n");
    resp.extend_from_slice(&fixture.body);
    let _ = stream.write_all(&resp);
    let _ = stream.flush();
}

fn content_length(lower_headers: &str) -> Option<usize> {
    for line in lower_headers.lines() {
        if let Some(v) = line.strip_prefix("content-length:") {
            return v.trim().parse().ok();
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}
