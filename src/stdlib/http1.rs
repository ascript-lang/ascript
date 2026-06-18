//! Minimal, hardened HTTP/1.1 **client** codec (CNTR §3.2) — internal module.
//!
//! TCP HTTP keeps using `reqwest` (unchanged). But a `{socketPath}` request (Docker's
//! `/var/run/docker.sock`) speaks HTTP/1.1 over a `UnixStream`, which reqwest cannot do
//! cleanly. This module is a SMALL, AUDITABLE HTTP/1.1 client codec, generic over the
//! transport (`AsyncRead + AsyncWrite + Unpin`) so unit tests drive it over
//! `tokio::io::duplex()` and production (Task 3.2's `std/http` `{socketPath}` path,
//! Phase 4's `std/docker`) uses `tokio::net::UnixStream`.
//!
//! **Hostile-input hardening is the headline:** every malformed response → a clean
//! Tier-1 `Result::Err(String)` (the caller wraps it as a `[nil, err]` pair), NEVER a
//! panic, hang, or unbounded allocation. Every read is bounded; a never-terminating
//! server cannot make the parser buffer without limit.
//!
//! It has NO `std/` routing entry and NO exports — it is consumed internally.

#![cfg(all(unix, feature = "net"))]
// The codec's public surface (`Http1Request`, `send_request`, `read_to_end`,
// `into_byte_stream`, the bound constants) has no non-test caller YET — its consumers are
// Task 3.2's `std/http` `{socketPath}` path and Phase 4's `std/docker`. Tests exercise the
// whole surface now; allow dead-code until those callers land in the next tasks.
#![allow(dead_code)]

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
// `bytes::Bytes` is the chunk type net_http's `ByteStream` carries; tokio-util re-exports
// the `bytes` crate, so we alias it rather than add a separate direct dependency — keeping
// `into_byte_stream` returning net_http's EXACT type.
use tokio_util::bytes;

/// Maximum size of the response head (status line + all header lines, up to the blank
/// line). A head larger than this → a clean `Err` (a hostile server cannot stream an
/// unbounded header block to exhaust memory).
pub(crate) const MAX_HEADER_BLOCK: usize = 64 * 1024;

/// Maximum number of header lines in the response head.
pub(crate) const MAX_HEADERS: usize = 256;

/// Maximum size of a single chunk in a `Transfer-Encoding: chunked` body. A chunk size
/// larger than this → a clean `Err` (the chunk-size line is attacker-controlled hex).
pub(crate) const MAX_CHUNK_SIZE: u64 = 16 * 1024 * 1024;

/// A single HTTP/1.1 request to write.
pub(crate) struct Http1Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// Caller headers, written verbatim after `Host`/`Connection`. The caller adds
    /// `Connection: Upgrade` / `Upgrade: …` here for a hijack; otherwise we default to
    /// `Connection: close`.
    pub headers: Vec<(String, String)>,
    pub body: Option<&'a [u8]>,
}

/// A parsed HTTP/1.1 response head plus a framed body.
pub(crate) struct Http1Response<T> {
    pub status: u16,
    /// Order-preserving; lookup is ASCII-case-insensitive (see [`header`]).
    pub headers: Vec<(String, String)>,
    pub body: Http1Body<T>,
}

impl<T> Http1Response<T> {
    /// ASCII-case-insensitive header lookup (first match wins).
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// How the response body is framed.
enum Framing {
    /// Exactly `n` more bytes follow.
    ContentLength(u64),
    /// `Transfer-Encoding: chunked`.
    Chunked,
    /// No length and `Connection: close` — read to EOF.
    ReadToEof,
    /// Status / method dictates no body (204/304/HEAD/Content-Length: 0).
    Empty,
}

/// The body of a parsed response.
pub(crate) enum Http1Body<T> {
    /// A streamable body. Carries the buffered reader (which may already hold bytes read
    /// past the head) plus the framing state.
    Stream(BodyReader<T>),
    /// `101 Switching Protocols`: hand the raw transport back to the caller plus the
    /// EXACT bytes already buffered past the head (Phase 4 exec/attach hijack needs this).
    Upgraded { transport: T, leftover: Vec<u8> },
}

/// The streamable-body state: a `BufReader` over the transport (carrying any bytes already
/// read past the head as leftover) + the framing.
pub(crate) struct BodyReader<T> {
    reader: BufReader<T>,
    framing: Framing,
    /// Chunked decode bookkeeping: bytes remaining in the current chunk (`None` ⇒ need to
    /// read the next size line); `done` once the terminal `0` chunk + trailers consumed.
    chunk_remaining: Option<u64>,
    done: bool,
}

impl<T: AsyncRead + AsyncWrite + Unpin> BodyReader<T> {
    /// Read the entire body into a `Vec`, bounded by `cap` (the stdlib `MAX_ALLOC_COUNT`
    /// discipline). We NEVER `reserve` an attacker-controlled length: bytes are appended as
    /// they arrive and the running total is checked against `cap`, so a lying
    /// `Content-Length`/`chunked` body cannot drive an unbounded allocation.
    pub(crate) async fn read_to_end(mut self, cap: usize) -> Result<Vec<u8>, String> {
        let mut out: Vec<u8> = Vec::new();
        loop {
            let mut chunk = [0u8; 16 * 1024];
            let got = self
                .read_frame(&mut chunk)
                .await
                .map_err(|e| format!("http1: body read error: {e}"))?;
            if got == 0 {
                break;
            }
            if out.len() + got > cap {
                return Err(format!(
                    "http1: response body exceeds {cap}-byte limit"
                ));
            }
            out.extend_from_slice(&chunk[..got]);
        }
        Ok(out)
    }

    /// Read up to `buf.len()` bytes of the *decoded* body (content-length / chunked / EOF
    /// transparently), returning the count (0 ⇒ end of body). Every read is bounded by
    /// `buf.len()` and the framing limits, so a never-terminating server cannot grow an
    /// unbounded buffer here.
    async fn read_frame(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        match self.framing {
            Framing::Empty => {
                self.done = true;
                Ok(0)
            }
            Framing::ContentLength(ref mut remaining) => {
                if *remaining == 0 {
                    self.done = true;
                    return Ok(0);
                }
                let want = (*remaining).min(buf.len() as u64) as usize;
                let got = self.reader.read(&mut buf[..want]).await?;
                if got == 0 {
                    // EOF before Content-Length satisfied.
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "http1: connection closed before Content-Length body completed",
                    ));
                }
                *remaining -= got as u64;
                if *remaining == 0 {
                    self.done = true;
                }
                Ok(got)
            }
            Framing::ReadToEof => {
                let got = self.reader.read(buf).await?;
                if got == 0 {
                    self.done = true;
                }
                Ok(got)
            }
            Framing::Chunked => self.read_chunked(buf).await,
        }
    }

    /// Chunked transfer decode. Hex sizes (incl. UPPERCASE), `;ext` extensions ignored,
    /// terminal `0\r\n\r\n`, trailer headers skipped. Bounded by `MAX_CHUNK_SIZE`.
    async fn read_chunked(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.chunk_remaining {
                None => {
                    // Read the chunk-size line: "<hex>[;ext...]\r\n".
                    let line = read_line_limited(&mut self.reader, MAX_HEADER_BLOCK).await?;
                    let size_part = line.split(';').next().unwrap_or("").trim();
                    let size = u64::from_str_radix(size_part, 16).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("http1: bad chunk size '{size_part}'"),
                        )
                    })?;
                    if size > MAX_CHUNK_SIZE {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("http1: chunk size {size} exceeds {MAX_CHUNK_SIZE}-byte cap"),
                        ));
                    }
                    if size == 0 {
                        // Terminal chunk: consume trailer headers up to the blank line.
                        loop {
                            let trailer =
                                read_line_limited(&mut self.reader, MAX_HEADER_BLOCK).await?;
                            if trailer.is_empty() {
                                break;
                            }
                        }
                        self.done = true;
                        return Ok(0);
                    }
                    self.chunk_remaining = Some(size);
                }
                Some(0) => {
                    // End of a data chunk: consume the trailing CRLF, then loop for next size.
                    let crlf = read_line_limited(&mut self.reader, 8).await?;
                    if !crlf.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "http1: missing CRLF after chunk data",
                        ));
                    }
                    self.chunk_remaining = None;
                }
                Some(remaining) => {
                    let want = remaining.min(buf.len() as u64) as usize;
                    let got = self.reader.read(&mut buf[..want]).await?;
                    if got == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "http1: connection closed mid-chunk",
                        ));
                    }
                    self.chunk_remaining = Some(remaining - got as u64);
                    return Ok(got);
                }
            }
        }
    }
}

impl<T> BodyReader<T>
where
    T: AsyncRead + AsyncWrite + Unpin + 'static,
{
    /// Adapt this body into net_http's EXACT `ByteStream` type
    /// (`Pin<Box<dyn Stream<Item = io::Result<Bytes>>>>`), so Task 3.2 can build a
    /// `StreamingBody`/`BodyMode` reader over a UDS response that is INDISTINGUISHABLE from
    /// a TCP one downstream. Pull-driven: each poll awaits only the next decoded frame.
    pub(crate) fn into_byte_stream(self) -> crate::stdlib::net_http::ByteStream {
        let stream = futures_util::stream::unfold(self, |mut rdr| async move {
            let mut buf = [0u8; 16 * 1024];
            match rdr.read_frame(&mut buf).await {
                Ok(0) => None,
                Ok(n) => Some((Ok(bytes::Bytes::copy_from_slice(&buf[..n])), rdr)),
                Err(e) => Some((Err(e), rdr)),
            }
        });
        Box::pin(stream)
    }
}

/// Send `req` over `io`, parse the response head, and frame the body. A malformed
/// response is a clean `Err(String)`; never a panic, hang, or unbounded allocation.
pub(crate) async fn send_request<T: AsyncRead + AsyncWrite + Unpin>(
    mut io: T,
    req: &Http1Request<'_>,
) -> Result<Http1Response<T>, String> {
    write_request(&mut io, req)
        .await
        .map_err(|e| format!("http1: write error: {e}"))?;

    let mut reader = BufReader::new(io);

    // ---- status line ----
    let status_line = read_line_limited(&mut reader, MAX_HEADER_BLOCK)
        .await
        .map_err(|e| format!("http1: reading status line: {e}"))?;
    let status = parse_status_line(&status_line)?;

    // ---- header block ----
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut head_bytes = status_line.len();
    loop {
        let line = read_line_limited(&mut reader, MAX_HEADER_BLOCK)
            .await
            .map_err(|e| format!("http1: reading headers: {e}"))?;
        if line.is_empty() {
            break; // blank line terminates the head
        }
        head_bytes = head_bytes.saturating_add(line.len()).saturating_add(2);
        if head_bytes > MAX_HEADER_BLOCK {
            return Err(format!(
                "http1: response head exceeds {MAX_HEADER_BLOCK}-byte limit"
            ));
        }
        if headers.len() >= MAX_HEADERS {
            return Err(format!("http1: response has more than {MAX_HEADERS} headers"));
        }
        let (name, value) = line.split_once(':').ok_or_else(|| {
            format!("http1: malformed header line (no colon): {line:?}")
        })?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }

    // ---- 101 Switching Protocols: hand back the transport + leftover ----
    if status == 101 {
        let (transport, leftover) = into_inner_with_leftover(reader);
        return Ok(Http1Response {
            status,
            headers,
            body: Http1Body::Upgraded {
                transport,
                leftover,
            },
        });
    }

    // ---- frame the body ----
    let is_head = req.method.eq_ignore_ascii_case("HEAD");
    let framing = decide_framing(status, is_head, &headers)?;

    Ok(Http1Response {
        status,
        headers,
        body: Http1Body::Stream(BodyReader {
            reader,
            framing,
            chunk_remaining: None,
            done: false,
        }),
    })
}

/// Write the request: line + `Host: localhost` (UDS has no host) + `Connection: close`
/// (unless the caller sets `Connection`/`Upgrade` for a hijack) + caller headers +
/// `Content-Length` + body. Then flush.
async fn write_request<T: AsyncWrite + Unpin>(
    io: &mut T,
    req: &Http1Request<'_>,
) -> io::Result<()> {
    let mut head = String::new();
    head.push_str(req.method);
    head.push(' ');
    head.push_str(req.path);
    head.push_str(" HTTP/1.1\r\n");
    head.push_str("Host: localhost\r\n");

    // Default Connection: close, unless the caller is driving an upgrade/hijack.
    let caller_sets_connection = req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("connection"));
    if !caller_sets_connection {
        head.push_str("Connection: close\r\n");
    }

    for (k, v) in &req.headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }

    if let Some(body) = req.body {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");

    io.write_all(head.as_bytes()).await?;
    if let Some(body) = req.body {
        io.write_all(body).await?;
    }
    io.flush().await?;
    Ok(())
}

/// Parse `HTTP/1.x <code> <reason>` → the numeric status. Rejects garbage and a status
/// outside `[100, 599]`.
fn parse_status_line(line: &str) -> Result<u16, String> {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err(format!("http1: malformed status line: {line:?}"));
    }
    let code_str = parts
        .next()
        .ok_or_else(|| format!("http1: status line has no code: {line:?}"))?;
    let code: u16 = code_str
        .parse()
        .map_err(|_| format!("http1: non-numeric status code: {code_str:?}"))?;
    if !(100..=599).contains(&code) {
        return Err(format!("http1: status code out of range: {code}"));
    }
    Ok(code)
}

/// Decide how to frame the body from the status, method, and headers.
fn decide_framing(
    status: u16,
    is_head: bool,
    headers: &[(String, String)],
) -> Result<Framing, String> {
    // No-body statuses / HEAD: empty, regardless of any length header.
    if is_head || status == 204 || status == 304 || (100..200).contains(&status) {
        return Ok(Framing::Empty);
    }

    let te = header_ci(headers, "transfer-encoding");
    if let Some(te) = te {
        if te.to_ascii_lowercase().contains("chunked") {
            return Ok(Framing::Chunked);
        }
    }

    if let Some(cl) = header_ci(headers, "content-length") {
        let cl = cl.trim();
        let n: u64 = cl
            .parse()
            .map_err(|_| format!("http1: invalid Content-Length: {cl:?}"))?;
        return Ok(if n == 0 {
            Framing::Empty
        } else {
            Framing::ContentLength(n)
        });
    }

    // No framing header. If Connection: close, read to EOF; otherwise (HTTP/1.1 default
    // keep-alive with no length) there is no body to frame — treat as empty so we don't
    // block forever waiting on a body that will never come.
    if let Some(conn) = header_ci(headers, "connection") {
        if conn.eq_ignore_ascii_case("close") {
            return Ok(Framing::ReadToEof);
        }
    }
    Ok(Framing::Empty)
}

/// ASCII-case-insensitive header lookup over a slice (first match wins).
fn header_ci<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Read one CRLF-terminated line, stripping the trailing `\r\n` (or bare `\n`), bounded by
/// `max` bytes. A line longer than `max` (no terminator) → an `Err` so a hostile server
/// cannot stream an unbounded line. EOF with no data → an `Err` (truncated head/body).
async fn read_line_limited<T: AsyncRead + Unpin>(
    reader: &mut BufReader<T>,
    max: usize,
) -> io::Result<String> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        let got = reader.read(&mut byte).await?;
        if got == 0 {
            if line.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "http1: connection closed mid-line",
                ));
            }
            // EOF without trailing newline — return what we have (tolerant of a final line).
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        if line.len() >= max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("http1: line exceeds {max}-byte limit"),
            ));
        }
        line.push(byte[0]);
    }
    // Strip a trailing CR.
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Extract the underlying transport from a `BufReader` along with whatever bytes it has
/// already buffered past the consumed head (the upgrade leftover).
fn into_inner_with_leftover<T: AsyncRead>(reader: BufReader<T>) -> (T, Vec<u8>) {
    let leftover = reader.buffer().to_vec();
    (reader.into_inner(), leftover)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::time::timeout;

    /// Run `send_request` against a duplex whose server end is pre-loaded with `canned`
    /// bytes (then closed). Returns the parsed response. Wrapped in a 2s timeout so a hang
    /// is a test failure, never an indefinite block.
    async fn run(canned: &[u8]) -> Result<Http1Response<tokio::io::DuplexStream>, String> {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let canned = canned.to_vec();
        // Drain the request the client writes, then feed the canned response and close.
        let server_task = tokio::spawn(async move {
            let mut scratch = [0u8; 4096];
            // Read whatever the request is (best-effort; small) so the client's write_all
            // doesn't backpressure-deadlock on a tiny duplex.
            let _ = timeout(Duration::from_millis(200), server.read(&mut scratch)).await;
            let _ = server.write_all(&canned).await;
            let _ = server.shutdown().await;
            // keep `server` alive until written; dropping closes the read side (EOF).
        });
        let req = Http1Request {
            method: "GET",
            path: "/",
            headers: vec![],
            body: None,
        };
        let res = timeout(Duration::from_secs(2), send_request(client, &req)).await;
        let _ = server_task.await;
        match res {
            Ok(r) => r,
            Err(_) => Err("TEST-TIMEOUT: send_request hung".to_string()),
        }
    }

    // ---- happy paths ----

    #[tokio::test]
    async fn simple_content_length_read_to_end() {
        let resp = run(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.header("content-length"), Some("5"));
        let body = match resp.body {
            Http1Body::Stream(r) => r.read_to_end(1 << 20).await.unwrap(),
            _ => panic!("expected stream body"),
        };
        assert_eq!(body, b"hello");
    }

    #[tokio::test]
    async fn simple_content_length_via_byte_stream() {
        use futures_util::StreamExt;
        let resp = run(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world")
            .await
            .unwrap();
        let mut stream = match resp.body {
            Http1Body::Stream(r) => r.into_byte_stream(),
            _ => panic!("expected stream body"),
        };
        let mut got = Vec::new();
        while let Some(chunk) = stream.next().await {
            got.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(got, b"hello world");
    }

    #[tokio::test]
    async fn header_lookup_is_case_insensitive_and_order_preserving() {
        let resp = run(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-A: 1\r\nX-B: 2\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        assert_eq!(resp.header("CONTENT-TYPE"), Some("text/plain"));
        // order preserved
        assert_eq!(resp.headers[1].0, "X-A");
        assert_eq!(resp.headers[2].0, "X-B");
    }

    #[tokio::test]
    async fn chunked_multichunk_uppercase_hex_extension_and_trailer() {
        // "A" = 10 bytes, uppercase hex; ";ext=1" extension ignored; second chunk "5";
        // terminal "0" + a trailer header skipped.
        let canned = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
A;ext=1\r\n0123456789\r\n\
5\r\nhello\r\n\
0\r\nX-Trailer: ok\r\n\r\n";
        let resp = run(canned).await.unwrap();
        let body = match resp.body {
            Http1Body::Stream(r) => r.read_to_end(1 << 20).await.unwrap(),
            _ => panic!("expected stream body"),
        };
        assert_eq!(body, b"0123456789hello");
    }

    #[tokio::test]
    async fn connection_close_read_to_eof() {
        let resp = run(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nbody-to-eof")
            .await
            .unwrap();
        let body = match resp.body {
            Http1Body::Stream(r) => r.read_to_end(1 << 20).await.unwrap(),
            _ => panic!("expected stream body"),
        };
        assert_eq!(body, b"body-to-eof");
    }

    #[tokio::test]
    async fn status_204_empty_body() {
        let resp = run(b"HTTP/1.1 204 No Content\r\n\r\n").await.unwrap();
        assert_eq!(resp.status, 204);
        let body = match resp.body {
            Http1Body::Stream(r) => r.read_to_end(1 << 20).await.unwrap(),
            _ => panic!("expected stream body"),
        };
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn content_length_zero_no_body() {
        let resp = run(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let body = match resp.body {
            Http1Body::Stream(r) => r.read_to_end(1 << 20).await.unwrap(),
            _ => panic!("expected stream body"),
        };
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn head_method_no_body() {
        // HEAD response carries Content-Length but NO body; we must not block reading one.
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            let mut scratch = [0u8; 4096];
            let _ = timeout(Duration::from_millis(200), server.read(&mut scratch)).await;
            let _ = server
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\n")
                .await;
            let _ = server.shutdown().await;
        });
        let req = Http1Request {
            method: "HEAD",
            path: "/",
            headers: vec![],
            body: None,
        };
        let resp = timeout(Duration::from_secs(2), send_request(client, &req))
            .await
            .expect("no hang")
            .unwrap();
        let _ = server_task.await;
        let body = match resp.body {
            Http1Body::Stream(r) => timeout(Duration::from_secs(2), r.read_to_end(1 << 20))
                .await
                .expect("HEAD body read must not hang")
                .unwrap(),
            _ => panic!("expected stream body"),
        };
        assert!(body.is_empty());
    }

    // ---- upgrade ----

    #[tokio::test]
    async fn upgrade_101_returns_transport_and_exact_leftover() {
        let resp = run(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: tcp\r\n\r\nRAW-HIJACK-BYTES")
            .await
            .unwrap();
        assert_eq!(resp.status, 101);
        match resp.body {
            Http1Body::Upgraded { leftover, .. } => {
                assert_eq!(leftover, b"RAW-HIJACK-BYTES");
            }
            _ => panic!("expected Upgraded body"),
        }
    }

    // ---- hostile set: every case MUST return Err (within the 2s timeout), never hang/panic ----

    async fn expect_err(canned: &[u8]) -> String {
        match timeout(Duration::from_secs(3), run(canned)).await {
            Ok(Err(e)) => e,
            Ok(Ok(resp)) => {
                // A head that parses might still have a hostile body; drain it and require Err.
                match resp.body {
                    Http1Body::Stream(r) => {
                        match timeout(Duration::from_secs(3), r.read_to_end(64 * 1024 * 1024)).await
                        {
                            Ok(Err(e)) => e,
                            Ok(Ok(_)) => panic!("expected Err, got Ok body"),
                            Err(_) => panic!("TIMEOUT/HANG reading hostile body"),
                        }
                    }
                    _ => panic!("expected Err, got non-stream body"),
                }
            }
            Err(_) => panic!("TIMEOUT/HANG: hostile input hung the parser"),
        }
    }

    #[tokio::test]
    async fn hostile_header_block_too_large() {
        let mut canned = b"HTTP/1.1 200 OK\r\n".to_vec();
        // A single enormous header value, no blank line.
        canned.extend_from_slice(b"X-Big: ");
        canned.extend(std::iter::repeat_n(b'a', 128 * 1024));
        canned.extend_from_slice(b"\r\n\r\n");
        let e = expect_err(&canned).await;
        assert!(e.contains("limit") || e.contains("exceeds"), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_too_many_headers() {
        let mut canned = b"HTTP/1.1 200 OK\r\n".to_vec();
        for i in 0..300 {
            canned.extend_from_slice(format!("X-H{i}: v\r\n").as_bytes());
        }
        canned.extend_from_slice(b"\r\n");
        let e = expect_err(&canned).await;
        assert!(e.contains("headers") || e.contains("limit"), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_non_numeric_content_length() {
        let e = expect_err(b"HTTP/1.1 200 OK\r\nContent-Length: abc\r\n\r\nx").await;
        assert!(e.contains("Content-Length"), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_overflowing_content_length() {
        let e =
            expect_err(b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999\r\n\r\nx").await;
        assert!(e.contains("Content-Length"), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_chunk_size_too_large() {
        // 0x2000000 = 32 MiB > 16 MiB cap.
        let e = expect_err(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2000000\r\n")
            .await;
        assert!(e.contains("chunk size") || e.contains("cap"), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_bad_hex_chunk_size() {
        let e = expect_err(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\n").await;
        assert!(e.contains("chunk size"), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_missing_crlf_after_chunk() {
        // chunk size 5, data "hello", but NO CRLF after — then EOF.
        let e =
            expect_err(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX").await;
        assert!(
            e.contains("CRLF") || e.contains("body read error"),
            "got: {e}"
        );
    }

    #[tokio::test]
    async fn hostile_truncated_mid_head() {
        // Status line then EOF (no header terminator).
        let e = expect_err(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain").await;
        assert!(!e.is_empty(), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_truncated_mid_body() {
        // Content-Length 10 but only 3 bytes then EOF.
        let e = expect_err(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc").await;
        assert!(!e.is_empty(), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_status_line_garbage() {
        let e = expect_err(b"NOT-HTTP garbage here\r\n\r\n").await;
        assert!(e.contains("status line") || e.contains("malformed"), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_status_below_100() {
        let e = expect_err(b"HTTP/1.1 099 Weird\r\n\r\n").await;
        assert!(e.contains("range") || e.contains("status"), "got: {e}");
    }

    #[tokio::test]
    async fn hostile_status_above_599() {
        let e = expect_err(b"HTTP/1.1 600 Weird\r\n\r\n").await;
        assert!(e.contains("range") || e.contains("status"), "got: {e}");
    }
}
