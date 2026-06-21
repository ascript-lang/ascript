//! `std/net/http` — modern HTTP client (feature `net`), spec §11.5.
//!
//! Verbs `get/post/put/patch/delete/head/options(url, opts?)` plus `request(opts)`
//! (where `opts.method` selects the verb). Every call is async and returns the
//! Tier-1 pair `[resp, err]`:
//!
//! - a connect / TLS / DNS / timeout failure → `[nil, err]`;
//! - otherwise `[resp, nil]` where `resp` is a `Value::native(HttpResponse)` whose
//!   `fields` carry `status` (number), `ok` (200-299), `version` ("1.1"|"2"|...),
//!   `url` (final string), `headers` (object, lowercased keys) and `cookies` (an
//!   object of name→value parsed from `Set-Cookie`).
//!
//! A non-2xx response is NOT an error — it is a normal `resp` with `ok == false`.
//!
//! The response body is read lazily via async methods on the handle:
//! `await resp.text() → [string, err]`, `await resp.bytes() → [bytes, err]`,
//! `await resp.json() → [value, err]`. `reqwest::Response::{text,bytes,json}`
//! consume the response by value, so each accessor `take_resource`s it; a second
//! body accessor on the same handle is a Tier-2 panic "response body already
//! consumed". The metadata fields above are read at response time and need no
//! consumption.
//!
//! Request body shapes (`opts.body`): a string · bytes · `{json: value}` (serialized
//! via the shared std/json converter → `application/json`) · `{form: object}`
//! (urlencoded → `application/x-www-form-urlencoded`) · `{multipart: [...]}`
//! (`reqwest::multipart::Form`; each part `{name, value}` for a text field or
//! `{name, data, filename?, contentType?}` for a file/bytes part).
//!
//! Advanced request options (Task 3) map onto a per-request `reqwest::Client`
//! (built only when an advanced opt is present; plain requests reuse the pooled
//! default client) and/or the `RequestBuilder`:
//!   - `timeout {connect, read, total}` (ms) → `connect_timeout` + total `timeout`.
//!     reqwest has no separate per-read timeout in its stable API, so `read` folds
//!     into the total timeout (when `total` is unset). A total-timeout expiry is a
//!     Tier-1 err.
//!   - `redirect {follow, max}` | `"none"` → `redirect::Policy` (default follow, max 10).
//!   - `retry {max, backoff:"exponential"|"constant", baseDelay, retryOn:[statuses]}`
//!     → a hand-rolled loop (`send_with_retry`) retrying on connection errors for
//!     idempotent methods AND on a response status ∈ `retryOn`, up to `max` (OFF by
//!     default). Non-cloneable (streaming) bodies cannot be retried.
//!   - `decompress` (default true) → `false` disables all transparent decoders.
//!   - `tls {caBundle, clientCert, minVersion, sni, insecure}` (`insecure` disables
//!     cert verification — for testing only).
//!   - `cookies: true` → a per-request-client cookie jar (persists across redirects +
//!     reuse within that request's client). A shared cross-request jar handle is a
//!     documented follow-up.
//!   - `proxy` "http://…"/"https://…"/"socks5://…" (socks feature enabled)/"system"/"none".
//!   - `httpVersion` "auto"|"1.1"|"2"|"3" — "3" requires the `http3` build feature
//!     (default-off, see §11.5 deferrals below); without it, "3" returns a clean
//!     Tier-1 error. `resp.version` reports the negotiated version.
//!   - `errorOnStatus: true` → a non-2xx response becomes a Tier-1 err.
//!   - `cancel` → a `cancelToken()` handle whose `cancel()` aborts the in-flight send
//!     via a `tokio::select!` against a shared `Notify`.
//!
//! Streaming bodies (Task 4): with `opts.stream:true` the body is NOT buffered;
//! `resp.body` is a `Value::native(HttpBody)` reader following the §11.4 idiom
//! (`await resp.body.read(n?)`→chunk|nil, `readLine()`, `readToEnd()`; chunk type
//! string|bytes per `opts.bodyMode`). The buffered accessors (text/bytes/json) are
//! then unavailable; conversely `resp.body` is only present in streaming mode.
//! Request streaming (`body:{stream:source}`): a `bytes` source is sent as a true
//! streamed body; a reader-handle (Reader/TcpStream/HttpBody) or async-generator-fn
//! source is DRAINED into a buffer and then sent (buffered-then-sent), because
//! pulling the next chunk from those sources re-enters the `!Send` single-threaded
//! interp, which reqwest's body poll cannot do — see `apply_stream_body`.
//!
//! Server-Sent Events (Task 5): `sse(url, opts?) → [stream, err]` is a first-class
//! SSE client (NOT a flag on request). It GETs with `Accept: text/event-stream` and
//! exposes a `Value::native(SseStream)` whose `await stream.next() → [event, err]`
//! parses the SSE wire format (`event:`/`data:`/`id:`/`retry:` fields, blank-line
//! event boundaries, multi-line `data:` joined with `\n`, `:`-comment lines ignored,
//! one-leading-space strip after the colon). `stream.lastEventId` is a live property
//! (the most recent `id:`); `stream.close()` ends the stream. Auto-reconnect is ON by
//! default (`opts.reconnect:false` disables): on disconnect it waits the server
//! `retry:` interval (or `opts.retryDefault`, default 3000ms), reconnects with
//! `Last-Event-ID`, and resumes; `opts.maxReconnects` caps attempts. See `SseState`.
//!
//! §11.5 deferrals (documented, owned, opt-in where applicable):
//!   - HTTP/3: feature-gated, default-OFF. The `http3` Cargo feature wires
//!     `reqwest/http3` and turns the `httpVersion:"3"` path (above) into a real
//!     http3 pin (`http3_prior_knowledge`); with the feature off it returns a clean
//!     Tier-1 error ("HTTP/3 requires the 'http3' build feature"). reqwest's http3
//!     is UNSTABLE, so enabling the feature ALSO needs `RUSTFLAGS=--cfg
//!     reqwest_unstable` — hence it is intentionally not in `default`.
//!   - Response trailers: best-effort. reqwest's high-level response API does not
//!     expose HTTP trailing headers, so `resp.trailers` is always an empty object
//!     (the §11.5 shape, kept so it is not a generic NativeMethod stub); trailing
//!     headers from chunked/h2 responses are dropped. Revisit if a low-level (hyper)
//!     client path is added.
//!   - SOCKS proxy: SUPPORTED. reqwest's `socks` feature is enabled in `[features]
//!     net`, so `proxy:"socks5://…"` works and compiles cleanly; it is listed under
//!     §11.5 only because it ships behind that reqwest feature.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, OwnedKind, Value, ValueKind};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
// `bytes::Bytes` is the chunk type of reqwest's byte stream; tokio-util re-exports
// the `bytes` crate, so we alias it rather than add a separate direct dependency.
use tokio_util::bytes;

/// Default chunk size for `resp.body.read()` with no `n` argument (mirrors
/// net/tcp + std/process readers).
const DEFAULT_CHUNK: usize = 64 * 1024;

/// How a streaming body's chunks are decoded (`opts.bodyMode`): UTF-8-lossy
/// strings (default) or raw bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BodyMode {
    Str,
    Bytes,
}

impl BodyMode {
    /// Parse `opts.bodyMode` ("string" default | "bytes").
    fn parse(opts: &Value, span: Span) -> Result<BodyMode, Control> {
        match opt_field(opts, "bodyMode") {
            None => Ok(BodyMode::Str),
            Some(other) => match other.kind() {
                ValueKind::Str(s) if s.as_ref() == "string" => Ok(BodyMode::Str),
                ValueKind::Str(s) if s.as_ref() == "bytes" => Ok(BodyMode::Bytes),
                _ => Err(AsError::at(
                    format!(
                        "net/http bodyMode must be \"string\" or \"bytes\", got {}",
                        crate::interp::type_name(&other)
                    ),
                    span,
                )
                .into()),
            },
        }
    }

    /// Wrap a finalized chunk as Str (lossy) or Bytes per the mode.
    fn wrap(self, bytes: Vec<u8>) -> Value {
        match self {
            BodyMode::Bytes => Value::bytes_rc(Rc::new(RefCell::new(bytes))),
            BodyMode::Str => Value::str(String::from_utf8_lossy(&bytes).into_owned()),
        }
    }
}

/// The `AsyncRead` we get by adapting a `reqwest` byte stream: `bytes_stream()`
/// yields `Result<Bytes, reqwest::Error>`; `StreamReader` needs `io::Error` items,
/// so the stream's errors are mapped to `io::Error` first. The reader is then a
/// `BufReader` over that, which lets the §11.4 idiom (`read`/`read_until`/
/// `read_to_end`) apply VERBATIM over the chunked stream — the leftover buffering
/// for partial `read(n)` and `readLine()` line-splitting is the BufReader's own.
pub(crate) type ByteStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>>>>;

/// A streaming HTTP response body: a `BufReader` over the response's chunked byte
/// stream, plus the decode mode. Reads are pull-driven (each awaits only the next
/// chunk), so a slow consumer applies backpressure to the transfer.
pub struct HttpBodyState {
    reader: tokio::io::BufReader<tokio_util::io::StreamReader<ByteStream, bytes::Bytes>>,
    mode: BodyMode,
}

impl HttpBodyState {
    fn new(resp: reqwest::Response, mode: BodyMode) -> Self {
        use futures_util::StreamExt;
        // bytes_stream(): Stream<Item = reqwest::Result<Bytes>>. Map the error type
        // to io::Error so StreamReader (which yields io::Result chunks) accepts it.
        let stream = resp
            .bytes_stream()
            .map(|r| r.map_err(std::io::Error::other));
        let boxed: ByteStream = Box::pin(stream);
        let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(boxed));
        HttpBodyState { reader, mode }
    }

    /// CNTR §3.2: build a streaming body over an ALREADY-BOXED `ByteStream` (the http1
    /// UDS client's `into_byte_stream()` returns this EXACT type). The resulting
    /// `HttpBodyState` is INDISTINGUISHABLE downstream from a reqwest-backed one — the
    /// same `StreamReader`+`BufReader` adaptation, so `resp.body.read()`/`readLine()`/
    /// `readToEnd()` behave byte-identically over a UDS response.
    #[cfg(unix)]
    pub(crate) fn from_byte_stream(stream: ByteStream, mode: BodyMode) -> Self {
        let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(stream));
        HttpBodyState { reader, mode }
    }

    async fn read_upto(&mut self, n: usize, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        // `read_buf` over a `take(n)` adapter appends only the bytes actually
        // available, capped at `n` — bounding the read at `n` with NO 64KB zero-fill
        // on every small frame (the old `resize(n, 0)` + `truncate` did). `reserve`
        // alone is insufficient: it can over-allocate, and `read_buf` fills to the
        // vec's full spare capacity, so a hard `take(n)` cap is required.
        buf.clear();
        buf.reserve(n);
        let got = (&mut self.reader).take(n as u64).read_buf(buf).await?;
        Ok(got)
    }

    async fn read_line_bytes(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        self.reader.read_until(b'\n', buf).await
    }

    async fn read_to_end_bytes(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        self.reader.read_to_end(buf).await
    }
}

/// A line reader over a streaming HTTP response body — the SSE wire format is
/// line-oriented, so we only need `read_until('\n')` over the chunked byte stream
/// (the same `StreamReader` + `BufReader` adaptation `HttpBodyState` uses, minus
/// the decode-mode/partial-read machinery, which SSE doesn't need).
type SseReader = tokio::io::BufReader<tokio_util::io::StreamReader<ByteStream, bytes::Bytes>>;

fn sse_reader(resp: reqwest::Response) -> SseReader {
    use futures_util::StreamExt;
    let stream = resp
        .bytes_stream()
        .map(|r| r.map_err(std::io::Error::other));
    let boxed: ByteStream = Box::pin(stream);
    tokio::io::BufReader::new(tokio_util::io::StreamReader::new(boxed))
}

/// Reconnect configuration captured at `sse()` time so a disconnect can re-issue
/// the same GET. `headers` are the caller's `opts.headers` (auth, etc.) replayed
/// verbatim; `Accept: text/event-stream` and `Last-Event-ID` are added per attempt.
pub struct SseReconnect {
    url: String,
    headers: Vec<(String, String)>,
    auth: Option<SseAuth>,
    enabled: bool,
    /// Fallback retry interval (ms) when the server hasn't sent a `retry:` field.
    retry_default_ms: u64,
    /// `opts.maxReconnects` cap (None = unbounded).
    max: Option<u32>,
    /// Reconnect attempts used so far.
    count: u32,
}

/// Captured `opts.auth` for an SSE request, applied via reqwest's `RequestBuilder`
/// (`bearer_auth`/`basic_auth`) on each connect — avoids hand-rolling base64 (which
/// isn't a dependency of the `net` feature).
#[derive(Clone)]
enum SseAuth {
    Bearer(String),
    Basic(String, Option<String>),
}

/// One parsed SSE event, dispatched on a blank-line boundary.
#[derive(Default)]
struct SseEvent {
    /// `event:` field — defaults to "message" if none was sent.
    event: Option<String>,
    /// Accumulated `data:` lines (joined with `\n` on dispatch).
    data: Vec<String>,
    /// `id:` field of this event, if any.
    id: Option<String>,
    /// `retry:` field of this event (ms), if any.
    retry: Option<u64>,
    /// Whether any field at all was seen since the last dispatch (a blank line with
    /// no preceding fields dispatches nothing).
    nonempty: bool,
}

/// The live state behind a `Value::native(SseStream)`: the current event-stream
/// body reader, the in-progress event buffer, the running `lastEventId`/`retry`,
/// and the reconnect template. `next()` reads lines, accumulates fields, and on a
/// blank line dispatches the buffered event; on EOF it either reconnects (carrying
/// `Last-Event-ID`) or ends.
pub struct SseState {
    reader: SseReader,
    pending: SseEvent,
    last_event_id: String,
    /// The current server-provided retry interval (ms), if any — overrides
    /// `reconnect.retry_default_ms` for the wait before the next reconnect.
    retry_ms: Option<u64>,
    reconnect: SseReconnect,
}

impl SseState {
    fn new(reader: SseReader, reconnect: SseReconnect) -> Self {
        SseState {
            reader,
            pending: SseEvent::default(),
            last_event_id: String::new(),
            retry_ms: None,
            reconnect,
        }
    }

    pub fn last_event_id(&self) -> &str {
        &self.last_event_id
    }

    /// Apply one SSE wire-format line to the in-progress event (no trailing newline).
    /// Returns the parsed event fields' effect; the dispatch decision (blank line)
    /// is made by the caller. Comment lines (leading `:`) are ignored.
    fn apply_line(&mut self, line: &str) {
        if line.is_empty() {
            return; // blank line handled by the caller (dispatch boundary)
        }
        if line.starts_with(':') {
            return; // comment
        }
        // field[:value]; if there is no colon, the whole line is the field name with
        // an empty value (per the SSE spec).
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => {
                // Strip a single leading space after the colon.
                let v = v.strip_prefix(' ').unwrap_or(v);
                (f, v)
            }
            None => (line, ""),
        };
        self.pending.nonempty = true;
        match field {
            "event" => self.pending.event = Some(value.to_string()),
            "data" => self.pending.data.push(value.to_string()),
            "id" => {
                // The spec ignores an id containing a NUL; the lastEventId is updated
                // immediately (so it reflects the field even before dispatch).
                if !value.contains('\0') {
                    self.pending.id = Some(value.to_string());
                    self.last_event_id = value.to_string();
                }
            }
            "retry" => {
                if let Ok(ms) = value.parse::<u64>() {
                    self.pending.retry = Some(ms);
                    self.retry_ms = Some(ms);
                }
            }
            _ => {} // unknown field: ignored per spec
        }
    }

    /// Take the buffered event (resetting the buffer) as an AScript object value, or
    /// `None` if nothing was buffered (a stray blank line dispatches nothing).
    fn take_event(&mut self) -> Option<Value> {
        if !self.pending.nonempty {
            return None;
        }
        let ev = std::mem::take(&mut self.pending);
        let mut map = IndexMap::new();
        map.insert(
            "event".to_string(),
            Value::str(ev.event.unwrap_or_else(|| "message".to_string())),
        );
        map.insert("data".to_string(), Value::str(ev.data.join("\n")));
        map.insert(
            "id".to_string(),
            match ev.id {
                Some(id) => Value::str(id),
                None => Value::nil(),
            },
        );
        map.insert(
            "retry".to_string(),
            match ev.retry {
                // NUM §4: a retry interval (ms) is an integer → `Int`.
                Some(ms) => Value::int(ms as i64),
                None => Value::nil(),
            },
        );
        Some(obj(map))
    }
}

thread_local! {
    /// A process-wide default `reqwest::Client` (connection pool + cookie store off
    /// for the core verbs). The interp is single-threaded, so a thread-local cache
    /// is sufficient; per-request configuration (timeouts/redirects/tls) arrives in
    /// Task 3 and will build dedicated clients as needed.
    static DEFAULT_CLIENT: RefCell<Option<reqwest::Client>> = const { RefCell::new(None) };
}

fn default_client() -> reqwest::Client {
    DEFAULT_CLIENT.with(|c| {
        c.borrow_mut()
            .get_or_insert_with(|| {
                reqwest::Client::builder()
                    .build()
                    .expect("default reqwest client should build")
            })
            .clone()
    })
}

/// The pooled, process-wide default `reqwest::Client`, shared with SP12
/// `std/telemetry`'s hand-rolled OTLP/Sentry/PostHog exporters AND BATT's
/// `std/jwt` JWKS fetch + `std/oauth` token calls so they reuse the same
/// connection pool (no second client, no new crate). Cloning is cheap
/// (an `Arc` internally).
#[cfg(any(feature = "telemetry", feature = "auth"))]
pub(crate) fn shared_client() -> reqwest::Client {
    default_client()
}

/// Keys whose presence forces a per-request `ClientBuilder` (the cached default
/// client cannot express any of these). If none are present we reuse the pooled
/// default client (fast path).
const ADVANCED_CLIENT_KEYS: &[&str] = &[
    "timeout",
    "redirect",
    "decompress",
    "tls",
    "cookies",
    "proxy",
    "httpVersion",
];

fn has_advanced_client_opts(opts: &Value) -> bool {
    ADVANCED_CLIENT_KEYS
        .iter()
        .any(|k| opt_field(opts, k).is_some())
}

/// Parsed `retry` config (a hand-rolled retry loop wraps the send). Default OFF.
struct RetryConfig {
    max: u32,
    exponential: bool,
    base_delay_ms: u64,
    retry_on: Vec<u16>,
}

/// Read a numeric field from an object, if present and a number.
fn num_field(o: &crate::value::ObjectCell, key: &str) -> Option<f64> {
    // NUM §4: accept BOTH numeric subtypes (`Int` and `Float`).
    o.get(key).and_then(|v| v.as_f64())
}

/// Read a numeric field strictly: `Ok(Some(n))` if it's a number, `Ok(None)` if
/// absent or nil, and a Tier-2 type error (`"<ctx> expects a number"`) for any
/// other present type. Used where a wrong type must fail loudly (not coerce).
fn strict_num_field(
    o: &crate::value::ObjectCell,
    key: &str,
    ctx: &str,
    span: Span,
) -> Result<Option<f64>, Control> {
    match o.get(key) {
        // NUM §4: accept BOTH numeric subtypes (`Int` and `Float`).
        Some(v) if v.is_number() => Ok(v.as_f64()),
        None => Ok(None),
        Some(other) => match other.kind() {
            ValueKind::Nil => Ok(None),
            _ => Err(AsError::at(
                format!(
                    "net/http {} expects a number, got {}",
                    ctx,
                    crate::interp::type_name(&other)
                ),
                span,
            )
            .into()),
        },
    }
}

/// HTTP methods considered idempotent (safe to auto-retry on a server/connection
/// error even without an explicit `retryOn` match).
fn is_idempotent(method: &reqwest::Method) -> bool {
    matches!(
        *method,
        reqwest::Method::GET
            | reqwest::Method::HEAD
            | reqwest::Method::PUT
            | reqwest::Method::DELETE
            | reqwest::Method::OPTIONS
    )
}

/// Parse `opts.retry` into a `RetryConfig`. Returns `Ok(None)` when absent (retry
/// OFF by default). Shape: `{ max, backoff:"exponential"|"constant", baseDelay,
/// retryOn:[statuses] }`.
fn parse_retry(opts: &Value, span: Span) -> Result<Option<RetryConfig>, Control> {
    let r = match opt_field(opts, "retry") {
        Some(v) => v,
        None => return Ok(None),
    };
    let o = match r.kind() {
        ValueKind::Object(o) => o.clone(),
        _ => {
            return Err(AsError::at(
                format!(
                    "net/http retry expects an object, got {}",
                    crate::interp::type_name(&r)
                ),
                span,
            )
            .into())
        }
    };
    // A present-but-wrong-type field is a type error (parity with timeout/redirect);
    // absent or nil fields fall back to the documented default.
    let max = strict_num_field(&o, "max", "retry.max", span)?
        .unwrap_or(0.0)
        .max(0.0) as u32;
    let exponential = match o.get("backoff").map(|x| x.into_kind()) {
        Some(OwnedKind::Nil) | None => true, // default exponential
        Some(OwnedKind::Str(s)) if s.as_ref() == "exponential" => true,
        Some(OwnedKind::Str(s)) if s.as_ref() == "constant" => false,
        Some(_) => {
            return Err(AsError::at(
                "net/http retry.backoff expects \"exponential\" or \"constant\"",
                span,
            )
            .into())
        }
    };
    let base_delay_ms = strict_num_field(&o, "baseDelay", "retry.baseDelay", span)?
        .unwrap_or(100.0)
        .max(0.0) as u64;
    let retry_on = match o.get("retryOn") {
        None => Vec::new(),
        Some(x) => match x.kind() {
            ValueKind::Nil => Vec::new(),
            ValueKind::Array(a) => {
                let mut out = Vec::new();
                for v in a.borrow().iter() {
                    // NUM §4: a status code may be an `Int` or `Float`.
                    match v.as_f64() {
                        Some(n) => out.push(n as u16),
                        None => {
                            return Err(AsError::at(
                                format!(
                            "net/http retry.retryOn expects an array of numbers, got a {} entry",
                            crate::interp::type_name(v)
                        ),
                                span,
                            )
                            .into())
                        }
                    }
                }
                out
            }
            _ => {
                return Err(AsError::at(
                    format!(
                        "net/http retry.retryOn expects an array of numbers, got {}",
                        crate::interp::type_name(&x)
                    ),
                    span,
                )
                .into())
            }
        },
    };
    Ok(Some(RetryConfig {
        max,
        exponential,
        base_delay_ms,
        retry_on,
    }))
}

/// Sleep the backoff interval before retry `attempt` (0-based): exponential is
/// `baseDelay * 2^attempt`, constant is `baseDelay`.
async fn backoff_sleep(cfg: &RetryConfig, attempt: u32) {
    let delay = if cfg.exponential {
        cfg.base_delay_ms.saturating_mul(1u64 << attempt.min(20))
    } else {
        cfg.base_delay_ms
    };
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
}

/// Build a per-request `reqwest::Client` from the advanced client-level opts
/// (timeout/redirect/decompress/tls/cookies/proxy/httpVersion). Only called when
/// `has_advanced_client_opts` is true; plain requests reuse the pooled default.
fn build_client(opts: &Value, span: Span) -> Result<reqwest::Client, Control> {
    build_client_inner(opts, span, false)
}

/// BLOCKER 1: build a per-request client with redirects FORCED off, used when a
/// `net` carve-out is active so a redirect cannot escape the host allow-list (we
/// only validate the initial request host). All other opts are still honored.
fn build_client_no_redirect(opts: &Value, span: Span) -> Result<reqwest::Client, Control> {
    build_client_inner(opts, span, true)
}

fn build_client_inner(
    opts: &Value,
    span: Span,
    force_no_redirect: bool,
) -> Result<reqwest::Client, Control> {
    let mut b = reqwest::Client::builder();

    // timeout { connect, read, total } in ms. reqwest has no separate per-read
    // timeout in its stable API, so `read` is folded into the total timeout (if
    // `total` is not itself set); the connect timeout is applied independently.
    if let Some(t) = opt_field(opts, "timeout") {
        let o = match t.kind() {
            ValueKind::Object(o) => o.clone(),
            _ => {
                return Err(AsError::at(
                    format!(
                        "net/http timeout expects an object, got {}",
                        crate::interp::type_name(&t)
                    ),
                    span,
                )
                .into())
            }
        };
        if let Some(c) = num_field(&o, "connect") {
            b = b.connect_timeout(std::time::Duration::from_millis(c.max(0.0) as u64));
        }
        // total wins; otherwise read maps to the total timeout.
        let total = num_field(&o, "total").or_else(|| num_field(&o, "read"));
        if let Some(t) = total {
            b = b.timeout(std::time::Duration::from_millis(t.max(0.0) as u64));
        }
    }

    // redirect { follow, max } | "none". Default: follow, max 10.
    // BLOCKER 1: when a `net` carve-out is active, redirects are FORCED off so a
    // redirect to a disallowed host can't escape the allow-list (the initial host is
    // the only one validated). This overrides any `opts.redirect` request.
    if force_no_redirect {
        b = b.redirect(reqwest::redirect::Policy::none());
    } else if let Some(r) = opt_field(opts, "redirect") {
        let policy = match r.kind() {
            ValueKind::Str(s) if s.as_ref() == "none" => reqwest::redirect::Policy::none(),
            ValueKind::Object(o) => {
                let follow =
                    !matches!(o.get("follow").map(|x| x.into_kind()), Some(OwnedKind::Bool(false)));
                if !follow {
                    reqwest::redirect::Policy::none()
                } else {
                    let max = num_field(o, "max").unwrap_or(10.0).max(0.0) as usize;
                    reqwest::redirect::Policy::limited(max)
                }
            }
            _ => {
                return Err(AsError::at(
                    format!(
                        "net/http redirect expects an object or \"none\", got {}",
                        crate::interp::type_name(&r)
                    ),
                    span,
                )
                .into())
            }
        };
        b = b.redirect(policy);
    }

    // decompress (default true). false → disable all transparent decoders, which
    // also stops reqwest from advertising Accept-Encoding.
    if matches!(
        opt_field(opts, "decompress").map(|x| x.into_kind()),
        Some(OwnedKind::Bool(false))
    ) {
        b = b.no_gzip().no_brotli().no_deflate().no_zstd();
    }

    // cookies: true → a per-client cookie jar (persists/sends across redirects +
    // connection reuse within this request's client). A shared cross-request jar
    // handle is a documented follow-up.
    if matches!(
        opt_field(opts, "cookies").map(|x| x.into_kind()),
        Some(OwnedKind::Bool(true))
    ) {
        b = b.cookie_store(true);
    }

    // tls { caBundle, clientCert, minVersion, sni, insecure }.
    if let Some(tls) = opt_field(opts, "tls") {
        b = apply_tls(b, &tls, span)?;
    }

    // proxy: "http://…" | "https://…" | "socks5://…" | "system" | "none".
    if let Some(p) = opt_field(opts, "proxy") {
        let s = want_string(&p, span, "net/http proxy")?;
        match s.as_ref() {
            "system" => { /* reqwest's default honors env proxies */ }
            "none" => b = b.no_proxy(),
            url => {
                let proxy = reqwest::Proxy::all(url).map_err(|e| {
                    Control::from(AsError::at(format!("net/http proxy: {}", e), span))
                })?;
                b = b.proxy(proxy);
            }
        }
    }

    // httpVersion: "auto" (default) | "1.1" | "2" | "3".
    if let Some(v) = opt_field(opts, "httpVersion") {
        let s = want_string(&v, span, "net/http httpVersion")?;
        match s.as_ref() {
            "auto" => {}
            "1.1" => b = b.http1_only(),
            "2" => b = b.http2_prior_knowledge(),
            // HTTP/3 is a §11.5 deferral: opt-in via the `http3` build feature
            // (default-off), which additionally needs `RUSTFLAGS=--cfg
            // reqwest_unstable` because reqwest's http3 support is unstable. When
            // the feature is on, pin http3 prior-knowledge on the client; when it
            // is off, surface a clean Tier-1 error rather than silently downgrading.
            #[cfg(feature = "http3")]
            "3" => b = b.http3_prior_knowledge(),
            #[cfg(not(feature = "http3"))]
            "3" => {
                return Err(AsError::at("HTTP/3 requires the 'http3' build feature", span).into())
            }
            other => {
                return Err(AsError::at(
                    format!(
                        "net/http httpVersion must be \"auto\"|\"1.1\"|\"2\"|\"3\", got \"{}\"",
                        other
                    ),
                    span,
                )
                .into())
            }
        }
    }

    b.build()
        .map_err(|e| Control::from(AsError::at(format!("net/http client build: {}", e), span)))
}

/// Apply `opts.tls` to a `ClientBuilder`. `insecure:true` DISABLES certificate
/// verification (`danger_accept_invalid_certs`) — intended only for testing
/// against self-signed endpoints; never use it against untrusted networks.
fn apply_tls(
    mut b: reqwest::ClientBuilder,
    tls: &Value,
    span: Span,
) -> Result<reqwest::ClientBuilder, Control> {
    let o = match tls.kind() {
        ValueKind::Object(o) => o.clone(),
        _ => {
            return Err(AsError::at(
                format!(
                    "net/http tls expects an object, got {}",
                    crate::interp::type_name(tls)
                ),
                span,
            )
            .into())
        }
    };
    // caBundle: a PEM string or a path to a PEM file → an extra trusted root.
    if let Some(ca) = o.get("caBundle") {
        let ca = want_string(&ca, span, "net/http tls.caBundle")?;
        let pem = read_pem_or_inline(&ca, "tls.caBundle", span)?;
        let cert = reqwest::Certificate::from_pem(pem.as_bytes()).map_err(|e| {
            Control::from(AsError::at(format!("net/http tls.caBundle: {}", e), span))
        })?;
        b = b.add_root_certificate(cert);
    }
    // clientCert: a PEM string (cert + private key) → a client identity (mTLS).
    if let Some(cc) = o.get("clientCert") {
        let cc = want_string(&cc, span, "net/http tls.clientCert")?;
        let pem = read_pem_or_inline(&cc, "tls.clientCert", span)?;
        let id = reqwest::Identity::from_pem(pem.as_bytes()).map_err(|e| {
            Control::from(AsError::at(format!("net/http tls.clientCert: {}", e), span))
        })?;
        b = b.identity(id);
    }
    // minVersion: "1.2" | "1.3".
    if let Some(mv) = o.get("minVersion") {
        let mv = want_string(&mv, span, "net/http tls.minVersion")?;
        let v = match mv.as_ref() {
            "1.2" => reqwest::tls::Version::TLS_1_2,
            "1.3" => reqwest::tls::Version::TLS_1_3,
            other => {
                return Err(AsError::at(
                    format!(
                        "net/http tls.minVersion must be \"1.2\" or \"1.3\", got \"{}\"",
                        other
                    ),
                    span,
                )
                .into())
            }
        };
        b = b.min_tls_version(v);
    }
    // sni: toggle TLS SNI (default on).
    if let Some(OwnedKind::Bool(sni)) = o.get("sni").map(|x| x.into_kind()) {
        b = b.tls_sni(sni);
    }
    // insecure: disable certificate verification (flagged above).
    if matches!(
        o.get("insecure").map(|x| x.into_kind()),
        Some(OwnedKind::Bool(true))
    ) {
        b = b.danger_accept_invalid_certs(true);
    }
    Ok(b)
}

/// Resolve a PEM source for `ctx` (a `tls.caBundle`/`tls.clientCert` value): if `s`
/// contains a `-----BEGIN` header it's inline PEM (used verbatim); otherwise it's a
/// filesystem path and the file is read. A path that can't be read yields a clear
/// error naming the path — not a downstream "PEM parse" error from inline-treating
/// a typo'd path.
fn read_pem_or_inline(s: &str, ctx: &str, span: Span) -> Result<String, Control> {
    if s.contains("-----BEGIN") {
        Ok(s.to_string())
    } else {
        std::fs::read_to_string(s).map_err(|e| {
            Control::from(AsError::at(
                format!("net/http {}: could not read PEM file '{}': {}", ctx, s, e),
                span,
            ))
        })
    }
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("get", bi("net_http.get")),
        ("post", bi("net_http.post")),
        ("put", bi("net_http.put")),
        ("patch", bi("net_http.patch")),
        ("delete", bi("net_http.delete")),
        ("head", bi("net_http.head")),
        ("options", bi("net_http.options")),
        ("request", bi("net_http.request")),
        ("cancelToken", bi("net_http.cancelToken")),
        ("sse", bi("net_http.sse")),
    ]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg)))
}

fn bytes_value(b: Vec<u8>) -> Value {
    Value::bytes_rc(Rc::new(RefCell::new(b)))
}

fn obj(map: IndexMap<String, Value>) -> Value {
    Value::object(map)
}

/// Pull `opts.<key>` (an object) when present and non-nil.
fn opt_field(opts: &Value, key: &str) -> Option<Value> {
    match opts.kind() {
        ValueKind::Object(o) => match o.get(key) {
            None => None,
            Some(v) if matches!(v.kind(), ValueKind::Nil) => None,
            Some(v) => Some(v),
        },
        _ => None,
    }
}

/// Map an AScript value to URL-query / form string pairs. Each value is rendered
/// with its scalar string form; arrays expand to repeated keys (`k=a&k=b`).
fn value_to_query_pairs(
    v: &Value,
    span: Span,
    ctx: &str,
) -> Result<Vec<(String, String)>, Control> {
    let o = match v.kind() {
        ValueKind::Object(o) => o,
        _ => {
            return Err(AsError::at(
                format!(
                    "{} expects an object, got {}",
                    ctx,
                    crate::interp::type_name(v)
                ),
                span,
            )
            .into())
        }
    };
    let mut pairs = Vec::new();
    for (k, val) in o.entries() {
        match val.kind() {
            ValueKind::Array(a) => {
                for item in a.borrow().iter() {
                    pairs.push((k.to_string(), scalar_to_string(item, span, ctx)?));
                }
            }
            _ => pairs.push((k.to_string(), scalar_to_string(&val, span, ctx)?)),
        }
    }
    Ok(pairs)
}

/// Render a scalar (string/number/bool/nil) into its query/form string form.
fn scalar_to_string(v: &Value, span: Span, ctx: &str) -> Result<String, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(s.to_string()),
        ValueKind::Float(_) | ValueKind::Bool(_) => Ok(v.to_string()),
        ValueKind::Nil => Ok(String::new()),
        _ => Err(AsError::at(
            format!(
                "{} value must be a string/number/bool, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

/// CNTR §3.2: extract the request-target PATH (incl. `?query`) from a URL for a
/// `{socketPath}` verb-helper call. The host is ignored (a UDS has no host). A URL
/// with no parseable path falls back to `/` (and a bare path that is already a
/// request-target — e.g. `"/json"` — is returned verbatim).
#[cfg(unix)]
fn path_from_url(url: &str) -> String {
    // Find the authority terminator: after "scheme://host", the first '/'/'?'/'#'.
    let after_scheme = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => {
            // No scheme: if it already looks like a request-target, use it verbatim.
            if url.starts_with('/') {
                return url.to_string();
            }
            url
        }
    };
    match after_scheme.find(['/', '?', '#']) {
        Some(i) => {
            let rest = &after_scheme[i..];
            // Strip a trailing '#fragment' (not sent on the wire).
            match rest.find('#') {
                Some(h) => rest[..h].to_string(),
                None => rest.to_string(),
            }
        }
        None => "/".to_string(),
    }
}

/// CNTR §3.2: URL-encode query pairs (`a=1&b=2`) for a UDS request-target. The TCP
/// path uses reqwest's `.query()`; here we encode the same `Vec<(String,String)>`.
#[cfg(unix)]
fn encode_query_pairs(pairs: &[(String, String)]) -> String {
    fn enc(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for &b in s.as_bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{:02X}", b)),
            }
        }
        out
    }
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// CNTR §3.2: the whole-request timeout (ms) for the UDS path. Mirrors the TCP
/// `timeout {connect, read, total}` precedence: `total` wins, else `read`, else
/// `connect` (the UDS path has no connect/read split, so all collapse to ONE
/// whole-request bound). `None` ⇒ no timeout.
#[cfg(unix)]
fn parse_total_timeout_ms(opts: &Value, span: Span) -> Result<Option<u64>, Control> {
    let t = match opt_field(opts, "timeout") {
        None => return Ok(None),
        Some(t) => t,
    };
    let o = match t.kind() {
        ValueKind::Object(o) => o,
        _ => {
            return Err(AsError::at(
                format!(
                    "net/http timeout expects an object, got {}",
                    crate::interp::type_name(&t)
                ),
                span,
            )
            .into())
        }
    };
    let ms = num_field(o, "total")
        .or_else(|| num_field(o, "read"))
        .or_else(|| num_field(o, "connect"));
    Ok(ms.map(|m| m.max(0.0) as u64))
}

/// CNTR §3.2: build the response metadata `fields` from an http1 UDS response in the
/// SAME shape as `http_response_fields` (status/ok/version/url/headers/cookies/
/// trailers). `url` is synthesized as `unix://<socketPath>` (the TCP path's
/// `resp.url` is the request URL; a UDS request has no URL, so we surface the socket).
#[cfg(unix)]
fn uds_response_fields(
    resp: &crate::stdlib::http1::Http1Response<tokio::net::UnixStream>,
    socket_path: &str,
) -> IndexMap<String, Value> {
    let mut fields = IndexMap::new();
    // NUM §4: an HTTP status code is an `Int`.
    fields.insert("status".to_string(), Value::int(i64::from(resp.status)));
    fields.insert(
        "ok".to_string(),
        Value::bool_((200..300).contains(&resp.status)),
    );
    // The http1 codec speaks HTTP/1.1. Match the TCP path's `http_version_str` form
    // ("1.1", NOT "HTTP/1.1") so `resp.version` is byte-identical across transports.
    fields.insert("version".to_string(), Value::str("1.1"));
    fields.insert(
        "url".to_string(),
        Value::str(format!("unix://{}", socket_path)),
    );

    // headers: object of lowercased name → value (last value wins), Set-Cookie folded
    // into `cookies` — byte-identical to the TCP path's `http_response_fields`.
    let mut headers = IndexMap::new();
    let mut cookies = IndexMap::new();
    for (name, value) in resp.headers.iter() {
        let key = name.to_ascii_lowercase();
        if key == "set-cookie" {
            if let Some((k, v)) = parse_set_cookie(value) {
                cookies.insert(k, Value::str(v));
            }
        }
        headers.insert(key, Value::str(value.clone()));
    }
    fields.insert("headers".to_string(), obj(headers));
    fields.insert("cookies".to_string(), obj(cookies));
    fields.insert("trailers".to_string(), obj(IndexMap::new()));
    fields
}

impl Interp {
    /// Module-level dispatch for `std/net/http` (the verbs + `request`).
    pub(crate) async fn call_http(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "get" | "post" | "put" | "patch" | "delete" | "head" | "options" => {
                let method = func.to_ascii_uppercase();
                let url = want_string(&arg(args, 0), span, &format!("net/http.{}", func))?;
                let opts = arg(args, 1);
                // CNTR §3.2: a verb helper with `socketPath` routes to the http1 UDS
                // client. The URL's PATH is the request-target; the host is ignored (a
                // UDS endpoint has no host).
                #[cfg(unix)]
                if let Some(sp) = opt_field(&opts, "socketPath") {
                    let socket_path = want_string(&sp, span, "net/http socketPath")?.to_string();
                    let path = path_from_url(&url);
                    return self
                        .call_http_send_uds(&method, &socket_path, &path, &opts, span)
                        .await;
                }
                self.call_http_send(&method, url.to_string(), &opts, span)
                    .await
            }
            "request" => {
                let opts = arg(args, 0);
                let method = match opt_field(&opts, "method") {
                    Some(m) => {
                        want_string(&m, span, "net/http.request method")?.to_ascii_uppercase()
                    }
                    None => "GET".to_string(),
                };
                // CNTR §3.2: `request({socketPath, path, …})` routes to the http1 UDS
                // client. `socketPath` is checked BEFORE `opts.url` is required — a UDS
                // request has no URL, only a `path` request-target.
                #[cfg(unix)]
                if let Some(sp) = opt_field(&opts, "socketPath") {
                    let socket_path = want_string(&sp, span, "net/http socketPath")?.to_string();
                    let path = match opt_field(&opts, "path") {
                        Some(p) => want_string(&p, span, "net/http.request path")?.to_string(),
                        None => "/".to_string(),
                    };
                    return self
                        .call_http_send_uds(&method, &socket_path, &path, &opts, span)
                        .await;
                }
                let url = match opt_field(&opts, "url") {
                    Some(u) => want_string(&u, span, "net/http.request url")?.to_string(),
                    None => {
                        return Err(AsError::at("net/http.request requires opts.url", span).into())
                    }
                };
                self.call_http_send(&method, url, &opts, span).await
            }
            "cancelToken" => Ok(self.make_cancel_token()),
            "sse" => {
                let url = want_string(&arg(args, 0), span, "net/http.sse")?;
                let opts = arg(args, 1);
                self.call_sse(url.to_string(), &opts, span).await
            }
            _ => Err(AsError::at(format!("std/net/http has no function '{}'", func), span).into()),
        }
    }

    /// Build + send one request, returning the Tier-1 `[resp, err]` pair.
    async fn call_http_send(
        &self,
        method: &str,
        url: String,
        opts: &Value,
        span: Span,
    ) -> Result<Value, Control> {
        let m = match reqwest::Method::from_bytes(method.as_bytes()) {
            Ok(m) => m,
            Err(_) => {
                return Err(
                    AsError::at(format!("net/http: invalid method '{}'", method), span).into(),
                )
            }
        };
        // REPLAY §2.5 — refuse `{stream:true}` at OPTION PARSE during a record/replay so a
        // recorded trace never contains a stream (the HttpBody reader is not virtualized;
        // streaming is v2). Fires BEFORE any connect, so no network egress / handle is
        // created. Gate-12: `trace_active()` is the zero-cost flag — off on every normal run.
        if self.trace_active()
            && matches!(
                opt_field(opts, "stream").map(|x| x.into_kind()),
                Some(OwnedKind::Bool(true))
            )
        {
            return Err(crate::interp::Interp::trace_streaming_refused(span));
        }
        // RESIL §5.4: deadline pre-check. If a `resilience.deadline(...)` budget is
        // active and already exhausted, refuse BEFORE any connect/DNS — return the
        // deadline-exceeded pair (distinct from a plain timeout) immediately. NO
        // deadline → `deadline_remaining_ms()` is `None`, the byte-identical fast path.
        let deadline_remaining = self.deadline_remaining_ms();
        if matches!(deadline_remaining, Some(r) if r <= 0.0) {
            return Ok(crate::interp::deadline_exceeded_pair());
        }
        // FFI §4.4 stage-2 (net carve-out, BLOCKER 1): re-check the resolved target
        // host against the allow-list BEFORE issuing the request. Gate-12: no carve-out
        // → `check_net_host` returns immediately with no comparison. A URL with no
        // parseable authority is left for reqwest to reject as a Tier-1 error.
        let net_carved = self.net_carveout_active();
        if let Some(host) = crate::stdlib::caps::host_of_url(&url) {
            self.check_net_host(&host, span)?;
        }
        // Fast path: plain requests reuse the pooled default client. Any advanced
        // client-level opt (timeout/redirect/tls/cookies/proxy/decompress/httpVersion)
        // builds a dedicated per-request client. When a `net` carve-out is active a
        // redirect could escape the allow-list (we validate only the initial host),
        // so we force a dedicated client with redirects DISABLED — a redirect to a
        // disallowed host must NOT bypass the allow-list (BLOCKER 1).
        let client = if net_carved {
            build_client_no_redirect(opts, span)?
        } else if has_advanced_client_opts(opts) {
            build_client(opts, span)?
        } else {
            default_client()
        };
        let mut rb = client.request(m.clone(), &url);

        // query: object → query pairs (merged onto the URL).
        if let Some(q) = opt_field(opts, "query") {
            let pairs = value_to_query_pairs(&q, span, "net/http query")?;
            rb = rb.query(&pairs);
        }

        // headers: object of string→string. `auth:` is a sibling helper key.
        if let Some(h) = opt_field(opts, "headers") {
            let map = match h.kind() {
                ValueKind::Object(o) => o,
                _ => {
                    return Err(AsError::at(
                        format!(
                            "net/http headers expects an object, got {}",
                            crate::interp::type_name(&h)
                        ),
                        span,
                    )
                    .into())
                }
            };
            for (k, v) in map.entries() {
                let vs = scalar_to_string(&v, span, "net/http header")?;
                rb = rb.header(k.as_ref(), vs);
            }
        }

        // auth: {bearer: tok} → Authorization: Bearer; {basic: [user, pass]} → basic.
        if let Some(a) = opt_field(opts, "auth") {
            rb = self.apply_auth(rb, &a, span)?;
        }

        // body: string · bytes · {json} · {form} · {multipart} · {stream}.
        if let Some(b) = opt_field(opts, "body") {
            rb = self.apply_body(rb, &b, span).await?;
        }

        // Resolve a cancellation handle (opts.cancel → a CancelHandle's Notify), if any.
        let cancel = self.resolve_cancel(opts, span)?;

        // Parse the retry policy (default: OFF).
        let retry = parse_retry(opts, span)?;

        // RESIL §5.4: clamp the request's wall-clock to min(requested total,
        // remaining deadline budget). The effective per-request `total` timeout is
        // already wired into the reqwest client (`build_client_inner`); racing the
        // whole send (incl. the retry loop) against a `sleep(remaining)` makes the
        // EFFECTIVE bound the smaller of the two and yields `deadline-exceeded`
        // (not a plain timeout) when the budget wins. NO deadline → the `None`
        // branch awaits the send unchanged (byte-identical fast path).
        let send = self.send_with_retry(rb, &m, &url, method, retry, cancel);
        let resp = match deadline_remaining {
            Some(r) => {
                tokio::select! {
                    res = send => match res {
                        Ok(r) => r,
                        Err(pair) => return Ok(pair),
                    },
                    _ = tokio::time::sleep(std::time::Duration::from_millis(r as u64)) => {
                        return Ok(crate::interp::deadline_exceeded_pair());
                    }
                }
            }
            None => match send.await {
                Ok(r) => r,
                Err(pair) => return Ok(pair),
            },
        };

        // errorOnStatus: a non-2xx response becomes a Tier-1 err instead of a resp.
        if matches!(
            opt_field(opts, "errorOnStatus").map(|x| x.into_kind()),
            Some(OwnedKind::Bool(true))
        ) && !resp.status().is_success()
        {
            return Ok(err_pair(format!(
                "net/http {} {} returned status {}",
                method,
                url,
                resp.status().as_u16()
            )));
        }

        // stream:true → don't buffer; expose `resp.body` as an HttpBody reader and
        // do NOT store the response for the buffered text/bytes/json accessors.
        let stream = matches!(
            opt_field(opts, "stream").map(|x| x.into_kind()),
            Some(OwnedKind::Bool(true))
        );
        if stream {
            let mode = BodyMode::parse(opts, span)?;
            Ok(make_pair(
                self.http_streaming_response_value(resp, mode),
                Value::nil(),
            ))
        } else {
            Ok(make_pair(self.http_response_value(resp), Value::nil()))
        }
    }

    /// CNTR §3.2: build + send one request over a `{socketPath}` Unix-domain socket
    /// using the hardened http1 client codec (reqwest cannot speak HTTP/1.1 over a
    /// `UnixStream`). The returned Tier-1 `[resp, err]` pair's `resp` is
    /// SCRIPT-VISIBLY IDENTICAL to the TCP path: the same `status`/`ok`/`version`/
    /// `headers`/`cookies` fields and the same `text()/bytes()/json()/json(Class)`
    /// buffered accessors (or streaming `body.read()` when `opts.stream:true`).
    ///
    /// Differences from the reqwest path (documented, not silent):
    ///   - `path` is the request-target; the host is ignored (a UDS has no host), so
    ///     `opts.query` pairs are merged onto `path`.
    ///   - The timeout is a WHOLE-REQUEST `tokio::time::timeout` around the entire
    ///     connect+send+buffer (the UDS path has no reqwest connect/read split).
    ///   - A STREAMING request body (`body:{stream:…}`) is unsupported (buffered
    ///     bodies only) — a clear Tier-2 panic.
    #[cfg(unix)]
    async fn call_http_send_uds(
        &self,
        method: &str,
        socket_path: &str,
        path: &str,
        opts: &Value,
        span: Span,
    ) -> Result<Value, Control> {
        use crate::stdlib::http1;
        // §3.1 carve-out (stage-2): enforce a `net` carve-out against the resolved
        // socket path BEFORE connecting. Gate-12: no carve-out → immediate Ok.
        self.check_unix_path(socket_path, span)?;
        // REPLAY §2.5 — refuse `{stream:true}` over UDS under record/replay too (the same
        // v2 streaming refusal as the TCP path), fired before connecting.
        if self.trace_active()
            && matches!(
                opt_field(opts, "stream").map(|x| x.into_kind()),
                Some(OwnedKind::Bool(true))
            )
        {
            return Err(crate::interp::Interp::trace_streaming_refused(span));
        }

        // Build the request-target: merge `opts.query` onto `path` (the TCP path uses
        // reqwest's `.query()`; here we append the encoded pairs to the target).
        let mut target = path.to_string();
        if let Some(q) = opt_field(opts, "query") {
            let pairs = value_to_query_pairs(&q, span, "net/http query")?;
            if !pairs.is_empty() {
                let qs = encode_query_pairs(&pairs);
                let sep = if target.contains('?') { '&' } else { '?' };
                target.push(sep);
                target.push_str(&qs);
            }
        }

        // Headers: object of string→string (the same shape the TCP path accepts).
        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(h) = opt_field(opts, "headers") {
            let map = match h.kind() {
                ValueKind::Object(o) => o,
                _ => {
                    return Err(AsError::at(
                        format!(
                            "net/http headers expects an object, got {}",
                            crate::interp::type_name(&h)
                        ),
                        span,
                    )
                    .into())
                }
            };
            for (k, v) in map.entries() {
                let vs = scalar_to_string(&v, span, "net/http header")?;
                headers.push((k.as_ref().to_string(), vs));
            }
        }

        // Body: a buffered string / bytes / {json} / {form}. A STREAMING request body
        // ({stream:…}) is NOT supported over the UDS client (it buffers request
        // bodies) — a clear Tier-2 panic, never a silent drop or partial send.
        let body_bytes: Option<Vec<u8>> = self.uds_request_body(opts, &mut headers, span)?;

        // Connect (Tier-1 on failure), send + parse, all under one whole-request
        // timeout if `opts.timeout` requests one.
        let timeout_ms = parse_total_timeout_ms(opts, span)?;
        let body_ref = body_bytes.as_deref();
        let req = http1::Http1Request {
            method,
            path: &target,
            headers,
            body: body_ref,
        };

        let stream_resp = matches!(
            opt_field(opts, "stream").map(|x| x.into_kind()),
            Some(OwnedKind::Bool(true))
        );
        let mode = if stream_resp {
            BodyMode::parse(opts, span)?
        } else {
            BodyMode::Str
        };

        // errorOnStatus: a non-2xx response becomes a Tier-1 err — IDENTICAL to the TCP
        // path (`call_http_send`). Decided here (where `opts` is in scope) and applied
        // inside `uds_drive_request` after the status is parsed.
        let error_on_status = matches!(
            opt_field(opts, "errorOnStatus").map(|x| x.into_kind()),
            Some(OwnedKind::Bool(true))
        );

        let drive =
            self.uds_drive_request(socket_path, &req, stream_resp, mode, error_on_status, span);
        match timeout_ms {
            Some(ms) => match tokio::time::timeout(
                std::time::Duration::from_millis(ms),
                drive,
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Ok(err_pair(format!(
                    "net/http {} {} timed out after {}ms",
                    method, socket_path, ms
                ))),
            },
            None => drive.await,
        }
    }

    /// CNTR §3.2: connect to `socket_path`, send `req`, and adapt the http1 response
    /// to the script-visible response value (buffered or streaming).
    #[cfg(unix)]
    async fn uds_drive_request(
        &self,
        socket_path: &str,
        req: &crate::stdlib::http1::Http1Request<'_>,
        stream_resp: bool,
        mode: BodyMode,
        error_on_status: bool,
        _span: Span,
    ) -> Result<Value, Control> {
        use crate::stdlib::http1;
        let stream = match tokio::net::UnixStream::connect(socket_path).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(err_pair(format!(
                    "net/http: connect to unix socket '{}' failed: {}",
                    socket_path, e
                )))
            }
        };
        let resp = match http1::send_request(stream, req).await {
            Ok(r) => r,
            Err(e) => return Ok(err_pair(format!("net/http: {}", e))),
        };

        // errorOnStatus: a non-2xx response becomes a Tier-1 err — byte-identical in
        // SHAPE to the TCP path (`call_http_send`); `url` is the synthesized
        // `unix://<socketPath>` that `resp.url` also reports.
        if error_on_status && !(200..300).contains(&resp.status) {
            return Ok(err_pair(format!(
                "net/http {} {} returned status {}",
                req.method,
                format_args!("unix://{}", socket_path),
                resp.status
            )));
        }

        // Build the metadata fields (status/ok/version/url/headers/cookies/trailers)
        // in the SAME shape as the TCP path — see `http_response_fields`.
        let mut fields = uds_response_fields(&resp, socket_path);

        let body = match resp.body {
            http1::Http1Body::Stream(reader) => reader,
            // 101 Switching Protocols has no place in a plain http.request (that is the
            // Phase-4 exec/attach hijack path); treat the upgrade as a Tier-1 error here.
            http1::Http1Body::Upgraded { .. } => {
                return Ok(err_pair(
                    "net/http: server returned 101 Switching Protocols (upgrade not supported by http.request)".to_string(),
                ))
            }
        };

        if stream_resp {
            // Streaming: wrap the http1 body's EXACT `ByteStream` in the SAME
            // `HttpBodyState`/`HttpBody` reader the TCP stream path uses, so
            // `resp.body.read()/readLine()/readToEnd()` is byte-identical.
            let body_handle = self.register_resource(
                NativeKind::HttpBody,
                IndexMap::new(),
                ResourceState::HttpBody(HttpBodyState::from_byte_stream(
                    body.into_byte_stream(),
                    mode,
                )),
            );
            fields.insert("body".to_string(), body_handle);
            // Mint the response handle WITHOUT a live backing resource (the body owns
            // the stream) — mirrors `http_streaming_response_value`.
            let handle =
                self.register_resource(NativeKind::HttpResponse, fields, ResourceState::Closed);
            if let ValueKind::Native(n) = handle.kind() {
                self.take_resource(n.id);
            }
            Ok(make_pair(handle, Value::nil()))
        } else {
            // Buffered: read the whole body up-front (bounded by MAX_ALLOC_COUNT) and
            // store it so `text()/bytes()/json()` produce the SAME surface as a
            // reqwest `Response`.
            let buf = match body.read_to_end(super::MAX_ALLOC_COUNT as usize).await {
                Ok(b) => b,
                Err(e) => return Ok(err_pair(format!("net/http: {}", e))),
            };
            let handle = self.register_resource(
                NativeKind::HttpResponse,
                fields,
                ResourceState::HttpBufferedResponse(buf),
            );
            Ok(make_pair(handle, Value::nil()))
        }
    }

    /// CNTR §3.2: extract a buffered request body for the UDS path. Supports the same
    /// buffered shapes as the TCP path (string / bytes / `{json}` / `{form}`) and sets
    /// `Content-Type` accordingly. A STREAMING request body (`{stream:…}`) is a clear
    /// Tier-2 panic — the UDS client buffers request bodies.
    #[cfg(unix)]
    fn uds_request_body(
        &self,
        opts: &Value,
        headers: &mut Vec<(String, String)>,
        span: Span,
    ) -> Result<Option<Vec<u8>>, Control> {
        let b = match opt_field(opts, "body") {
            None => return Ok(None),
            Some(b) => b,
        };
        match b.kind() {
            ValueKind::Str(s) => Ok(Some(s.as_bytes().to_vec())),
            ValueKind::Bytes(by) => Ok(Some(by.borrow().clone())),
            ValueKind::Object(o) => {
                if o.get("stream").is_some() {
                    return Err(AsError::at(
                        "net/http: a streaming request body (body:{stream}) is not supported over a socketPath (UDS) request — the UDS client buffers request bodies; use a string/bytes/{json}/{form} body",
                        span,
                    )
                    .into());
                }
                if let Some(jv) = o.get("json") {
                    let json = crate::stdlib::json::from_ascript(&jv, &mut Vec::new())
                        .map_err(|m| {
                            Control::from(AsError::at(format!("net/http body.json: {}", m), span))
                        })?;
                    let bytes = serde_json::to_vec(&json).map_err(|e| {
                        Control::from(AsError::at(format!("net/http body.json: {}", e), span))
                    })?;
                    if !headers
                        .iter()
                        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                    {
                        headers.push(("Content-Type".to_string(), "application/json".to_string()));
                    }
                    Ok(Some(bytes))
                } else if let Some(form) = o.get("form") {
                    let pairs = value_to_query_pairs(&form, span, "net/http body.form")?;
                    let encoded = encode_query_pairs(&pairs);
                    if !headers
                        .iter()
                        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                    {
                        headers.push((
                            "Content-Type".to_string(),
                            "application/x-www-form-urlencoded".to_string(),
                        ));
                    }
                    Ok(Some(encoded.into_bytes()))
                } else {
                    Err(AsError::at(
                        "net/http body object must be {json}, {form}, or {stream}; over a socketPath request {multipart} and {stream} are unsupported",
                        span,
                    )
                    .into())
                }
            }
            _ => Err(AsError::at(
                format!(
                    "net/http body must be a string, bytes, or an object, got {}",
                    crate::interp::type_name(&b)
                ),
                span,
            )
            .into()),
        }
    }

    /// Send `rb`, applying the hand-rolled retry loop and (optional) cancellation.
    /// Returns `Ok(response)` or `Err(pair)` where `pair` is the Tier-1 `[nil, err]`.
    async fn send_with_retry(
        &self,
        rb: reqwest::RequestBuilder,
        method_obj: &reqwest::Method,
        url: &str,
        method: &str,
        retry: Option<RetryConfig>,
        cancel: Option<std::sync::Arc<tokio::sync::Notify>>,
    ) -> Result<reqwest::Response, Value> {
        // No retry budget (no `retry` opt, or max 0): send the builder directly —
        // streaming bodies that can't be cloned still work on this path.
        let cfg = match retry {
            Some(c) if c.max > 0 => c,
            _ => return self.run_send(rb.send(), &cancel, method, url).await,
        };
        let max = cfg.max;
        let idempotent = is_idempotent(method_obj);

        let mut attempt: u32 = 0;
        loop {
            // Each retryable attempt needs a fresh builder. `try_clone` returns
            // None for non-replayable bodies (streams) — then retry is impossible.
            let send_fut = match rb.try_clone() {
                Some(b) => b.send(),
                None => return self.run_send(rb.send(), &cancel, method, url).await,
            };
            let result = self.run_send(send_fut, &cancel, method, url).await;
            match result {
                Ok(resp) => {
                    let should_retry = cfg.retry_on.contains(&resp.status().as_u16());
                    if should_retry && attempt < max {
                        backoff_sleep(&cfg, attempt).await;
                        attempt += 1;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(pair) => {
                    // A connection-level error: retry on idempotent methods if budget left.
                    if idempotent && attempt < max {
                        backoff_sleep(&cfg, attempt).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(pair);
                }
            }
        }
    }

    /// Await a single send future, racing it against an optional cancellation token.
    /// Maps a reqwest error or a cancellation into the Tier-1 `[nil, err]` pair.
    async fn run_send(
        &self,
        fut: impl std::future::Future<Output = reqwest::Result<reqwest::Response>>,
        cancel: &Option<std::sync::Arc<tokio::sync::Notify>>,
        method: &str,
        url: &str,
    ) -> Result<reqwest::Response, Value> {
        match cancel {
            Some(token) => {
                tokio::select! {
                    biased;
                    _ = token.notified() => {
                        Err(err_pair(format!("net/http {} {} cancelled", method, url)))
                    }
                    r = fut => r.map_err(|e| err_pair(format!("net/http {} {} failed: {}", method, url, e))),
                }
            }
            None => fut
                .await
                .map_err(|e| err_pair(format!("net/http {} {} failed: {}", method, url, e))),
        }
    }

    fn apply_auth(
        &self,
        rb: reqwest::RequestBuilder,
        auth: &Value,
        span: Span,
    ) -> Result<reqwest::RequestBuilder, Control> {
        let o = match auth.kind() {
            ValueKind::Object(o) => o,
            _ => {
                return Err(AsError::at(
                    format!(
                        "net/http auth expects an object, got {}",
                        crate::interp::type_name(auth)
                    ),
                    span,
                )
                .into())
            }
        };
        if let Some(tok) = o.get("bearer") {
            let tok = want_string(&tok, span, "net/http auth.bearer")?;
            return Ok(rb.bearer_auth(tok.to_string()));
        }
        if let Some(basic) = o.get("basic") {
            let arr = super::want_array(&basic, span, "net/http auth.basic")?;
            let arr = arr.borrow();
            let user = want_string(
                arr.first().unwrap_or(&Value::nil()),
                span,
                "net/http auth.basic[0]",
            )?;
            let pass = arr.get(1).cloned();
            let pass = match pass {
                None => None,
                Some(p) if matches!(p.kind(), ValueKind::Nil) => None,
                Some(p) => Some(want_string(&p, span, "net/http auth.basic[1]")?.to_string()),
            };
            return Ok(rb.basic_auth(user.to_string(), pass));
        }
        Err(AsError::at(
            "net/http auth expects {bearer} or {basic:[user,pass]}",
            span,
        )
        .into())
    }

    #[async_recursion::async_recursion(?Send)]
    async fn apply_body(
        &self,
        rb: reqwest::RequestBuilder,
        body: &Value,
        span: Span,
    ) -> Result<reqwest::RequestBuilder, Control> {
        match body.kind() {
            ValueKind::Str(s) => Ok(rb.body(s.to_string())),
            ValueKind::Bytes(b) => Ok(rb.body(b.borrow().clone())),
            ValueKind::Object(o) => {
                // Pull out the single recognized shape upfront (accessor API returns owned
                // Values so no borrow held across the {stream} await path).
                let (jv, form, mp, stream) = (
                    o.get("json"),
                    o.get("form"),
                    o.get("multipart"),
                    o.get("stream"),
                );
                if let Some(jv) = jv {
                    let json =
                        crate::stdlib::json::from_ascript(&jv, &mut Vec::new()).map_err(|m| {
                            Control::from(AsError::at(format!("net/http body.json: {}", m), span))
                        })?;
                    let bytes = serde_json::to_vec(&json).map_err(|e| {
                        Control::from(AsError::at(format!("net/http body.json: {}", e), span))
                    })?;
                    return Ok(rb
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(bytes));
                }
                if let Some(form) = form {
                    let pairs = value_to_query_pairs(&form, span, "net/http body.form")?;
                    // `.form(&pairs)` urlencodes + sets application/x-www-form-urlencoded.
                    return Ok(rb.form(&pairs));
                }
                if let Some(mp) = mp {
                    let form = build_multipart(&mp, span)?;
                    return Ok(rb.multipart(form));
                }
                if let Some(source) = stream {
                    return self.apply_stream_body(rb, &source, span).await;
                }
                Err(AsError::at(
                    "net/http body object must be {json}, {form}, {multipart}, or {stream}",
                    span,
                )
                .into())
            }
            _ => Err(AsError::at(
                format!(
                    "net/http body must be a string, bytes, or an object, got {}",
                    crate::interp::type_name(body)
                ),
                span,
            )
            .into()),
        }
    }

    /// Apply a `body: {stream: source}` request body.
    ///
    /// `source` is one of:
    ///   (a) a `bytes` value — sent as a true streamed body (`Body::wrap_stream`
    ///       over a one-chunk stream); trivially incremental.
    ///   (b) a reader native handle (std/process Reader, net/tcp TcpStream, or an
    ///       http `HttpBody`) — DRAINED fully into a buffer here, then sent.
    ///   (c) an async-generator AScript fn `() => [bytes, err]` — called repeatedly
    ///       (each `[chunk, err]`; `[nil, *]` or `nil` ends) and DRAINED into a
    ///       buffer here, then sent.
    ///
    /// WHY buffered for (b)/(c): on the single-threaded interp, reqwest polls the
    /// request body on its executor, but pulling the next chunk for these sources
    /// means re-entering the interpreter (calling a user fn / reading a resource),
    /// which needs `&mut Interp` — not available inside reqwest's body poll, and the
    /// interp is `!Send`. Draining-then-sending sidesteps that reentrancy/!Send
    /// problem. It is correct but loses true incremental upload for (b)/(c); the
    /// bytes source (a) keeps the true streamed path.
    async fn apply_stream_body(
        &self,
        rb: reqwest::RequestBuilder,
        source: &Value,
        span: Span,
    ) -> Result<reqwest::RequestBuilder, Control> {
        match source.kind() {
            // (a) bytes → a true streamed body (single chunk).
            ValueKind::Bytes(b) => {
                let data = b.borrow().clone();
                let chunk =
                    Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(data));
                let stream = futures_util::stream::once(async move { chunk });
                Ok(rb.body(reqwest::Body::wrap_stream(stream)))
            }
            // (b) a reader native handle → drain fully (buffered-then-sent).
            ValueKind::Native(n)
                if matches!(
                    n.kind,
                    NativeKind::Reader | NativeKind::TcpStream | NativeKind::HttpBody
                ) =>
            {
                let bytes = self.drain_reader_handle(n.clone(), span).await?;
                Ok(rb.body(bytes))
            }
            // (c) an async-generator fn → call to exhaustion (buffered-then-sent).
            // `Value::closure` is the VM's compiled-function value (V4-T5 bridge).
            ValueKind::Function(_)
            | ValueKind::Closure(_)
            | ValueKind::Builtin(_)
            | ValueKind::BoundMethod(_) => {
                let bytes = self.drain_generator(source.clone(), span).await?;
                Ok(rb.body(bytes))
            }
            _ => Err(AsError::at(
                format!(
                    "net/http body.stream expects bytes, a reader handle, or a generator fn, got {}",
                    crate::interp::type_name(source)
                ),
                span,
            )
            .into()),
        }
    }

    /// Drain a reader native handle (Reader/TcpStream/HttpBody) fully into bytes by
    /// calling its `readToEnd()` method. Buffered-then-sent (see `apply_stream_body`).
    async fn drain_reader_handle(
        &self,
        n: Rc<crate::value::NativeObject>,
        span: Span,
    ) -> Result<Vec<u8>, Control> {
        let m = Rc::new(NativeMethod {
            receiver: n,
            method: "readToEnd".to_string(),
        });
        let v = self.call_native_method(m, Vec::new(), span).await?;
        match v.kind() {
            ValueKind::Bytes(b) => Ok(b.borrow().clone()),
            ValueKind::Str(s) => Ok(s.as_bytes().to_vec()),
            ValueKind::Nil => Ok(Vec::new()),
            _ => Err(AsError::at(
                format!(
                    "net/http body.stream reader yielded a non-bytes value: {}",
                    crate::interp::type_name(&v)
                ),
                span,
            )
            .into()),
        }
    }

    /// Drain an async-generator fn fully: call it repeatedly, concatenating each
    /// chunk, until it returns `[nil, _]`/`nil` (end) or an `[_, err]` (error →
    /// Tier-1 propagated as a Tier-2 here is avoided — a generator error aborts the
    /// drain with a Tier-2, matching how a malformed body fails the request build).
    async fn drain_generator(&self, gen: Value, span: Span) -> Result<Vec<u8>, Control> {
        let mut out = Vec::new();
        loop {
            let r = self.call_value(gen.clone(), Vec::new(), span).await?;
            // A generator yields `[chunk, err]` (or a bare chunk / nil to end).
            let (chunk, err) = match r.kind() {
                ValueKind::Nil => (Value::nil(), Value::nil()),
                ValueKind::Array(a) => {
                    let a = a.borrow();
                    (
                        a.first().cloned().unwrap_or(Value::nil()),
                        a.get(1).cloned().unwrap_or(Value::nil()),
                    )
                }
                _ => (r.clone(), Value::nil()),
            };
            if !matches!(err.kind(), ValueKind::Nil) {
                return Err(AsError::at(
                    format!("net/http body.stream generator returned an error: {}", err),
                    span,
                )
                .into());
            }
            match chunk.kind() {
                ValueKind::Nil => return Ok(out), // end of stream
                ValueKind::Bytes(b) => out.extend_from_slice(&b.borrow()),
                ValueKind::Str(s) => out.extend_from_slice(s.as_bytes()),
                _ => {
                    return Err(AsError::at(
                        format!(
                            "net/http body.stream generator chunk must be bytes/string, got {}",
                            crate::interp::type_name(&chunk)
                        ),
                        span,
                    )
                    .into())
                }
            }
        }
    }

    /// Build the response metadata `fields` (status/ok/version/url/headers/cookies)
    /// read off the response before its body is consumed. Shared by the buffered
    /// and streaming response constructors.
    fn http_response_fields(resp: &reqwest::Response) -> IndexMap<String, Value> {
        let status = resp.status();
        let mut fields = IndexMap::new();
        // NUM §4: an HTTP status code is an `Int`.
        fields.insert("status".to_string(), Value::int(i64::from(status.as_u16())));
        fields.insert("ok".to_string(), Value::bool_(status.is_success()));
        fields.insert(
            "version".to_string(),
            Value::str(http_version_str(resp.version())),
        );
        fields.insert("url".to_string(), Value::str(resp.url().as_str()));

        // headers: object of lowercased name → value (last value wins on repeats,
        // except Set-Cookie which we fold into `cookies` below).
        let mut headers = IndexMap::new();
        let mut cookies = IndexMap::new();
        for (name, value) in resp.headers().iter() {
            let key = name.as_str().to_ascii_lowercase();
            let val = value.to_str().unwrap_or("").to_string();
            if key == "set-cookie" {
                if let Some((k, v)) = parse_set_cookie(&val) {
                    cookies.insert(k, Value::str(v));
                }
            }
            headers.insert(key, Value::str(val));
        }
        fields.insert("headers".to_string(), obj(headers));
        fields.insert("cookies".to_string(), obj(cookies));
        // trailers (§11.5): best-effort, always an empty object. reqwest's high-level
        // response API does not surface HTTP trailing headers, so we expose the spec
        // shape as an empty Object rather than letting `resp.trailers` mint a generic
        // NativeMethod stub. (See the module-doc deferral note.)
        fields.insert("trailers".to_string(), obj(IndexMap::new()));
        fields
    }

    /// Read the response metadata into `fields` and register the live response (for
    /// the buffered body accessors) behind a `Value::native(HttpResponse)` handle.
    fn http_response_value(&self, resp: reqwest::Response) -> Value {
        let fields = Self::http_response_fields(&resp);
        self.register_resource(
            NativeKind::HttpResponse,
            fields,
            ResourceState::HttpResponse(resp),
        )
    }

    /// Build a streaming response: the body is NOT buffered. The response's chunked
    /// byte stream is registered behind a `Value::native(HttpBody)` reader handle,
    /// which is exposed as the response's `body` field. The buffered accessors
    /// (text/bytes/json) are intentionally absent — see `call_http_response_method`.
    fn http_streaming_response_value(&self, resp: reqwest::Response, mode: BodyMode) -> Value {
        let mut fields = Self::http_response_fields(&resp);
        let body = self.register_resource(
            NativeKind::HttpBody,
            IndexMap::new(),
            ResourceState::HttpBody(HttpBodyState::new(resp, mode)),
        );
        fields.insert("body".to_string(), body);
        // The streaming response handle carries only metadata + the `body` reader —
        // there is NO live `reqwest::Response` behind it (the body owns the stream).
        // Register to mint the handle, then immediately drop its table entry so it
        // doesn't linger as a phantom resource: the only live resource for a
        // streaming response is the HttpBody, which finalizes itself on EOF. A later
        // `resp.text()/bytes()/json()` finds no entry and (because `body` is present
        // in fields) reports the clear "not available on a streaming response" error.
        let handle =
            self.register_resource(NativeKind::HttpResponse, fields, ResourceState::Closed);
        if let ValueKind::Native(n) = handle.kind() {
            self.take_resource(n.id);
        }
        handle
    }

    /// Dispatch a body accessor on an HTTP response handle: `text`/`bytes`/`json`.
    /// Each consumes the response (`take_http_response`); a second body accessor on
    /// the same handle is a Tier-2 panic.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_http_response_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        let method = m.method.as_str();
        match method {
            "text" | "bytes" | "json" => {
                // CNTR §3.2: a buffered UDS response stores its body bytes directly
                // (no live `reqwest::Response`). Produce the SAME `[value, err]`
                // surface from those bytes — text (UTF-8-lossy), bytes, json/json(Class).
                #[cfg(unix)]
                if let Some(buf) = self.take_http_buffered(id) {
                    return self.uds_body_accessor(method, buf, args, span).await;
                }
                let resp = match self.take_http_response(id) {
                    Some(r) => r,
                    None => {
                        // A streaming response (stream:true) carries a `body` reader
                        // and never stored a buffered Response — the buffered accessors
                        // don't apply. Distinguish that from a second consume.
                        if m.receiver.fields.contains_key("body") {
                            return Err(AsError::at(
                                format!("resp.{}() is not available on a streaming response (request opts.stream:true); read resp.body instead", method),
                                span,
                            )
                            .into());
                        }
                        return Err(AsError::at("response body already consumed", span).into());
                    }
                };
                match method {
                    "text" => match resp.text().await {
                        Ok(s) => Ok(make_pair(Value::str(s), Value::nil())),
                        Err(e) => Ok(err_pair(format!("response.text failed: {}", e))),
                    },
                    "bytes" => match resp.bytes().await {
                        Ok(b) => Ok(make_pair(bytes_value(b.to_vec()), Value::nil())),
                        Err(e) => Ok(err_pair(format!("response.bytes failed: {}", e))),
                    },
                    "json" => match resp.bytes().await {
                        Ok(b) => match serde_json::from_slice::<serde_json::Value>(&b) {
                            Ok(jv) => {
                                let val = crate::stdlib::json::to_ascript(&jv);
                                // Typed parse — dispatch on the first argument:
                                //
                                //   resp.json(Class, strict?)  → validate_into (class path)
                                //   resp.json(schema)          → schema.parse_value (schema path)
                                //   resp.json()                → raw decoded value
                                //
                                // Disambiguation is unambiguous: Value::class vs
                                // tagged-Object (Object with __kind) vs absent.
                                if let Some(ValueKind::Class(c)) = args.first().map(|x| x.kind()) {
                                    // Class path: validate_into.
                                    let strict = matches!(
                                        args.get(1).map(|x| x.kind()),
                                        Some(ValueKind::Bool(true))
                                    );
                                    match self.validate_into(c, &val, strict, "", span).await {
                                        Ok(inst) => Ok(make_pair(inst, Value::nil())),
                                        Err(e) => Ok(err_pair(e.message)),
                                    }
                                } else if let Some(schema_val) = args.first() {
                                    // Schema path: tagged-Object with __kind.
                                    if crate::stdlib::schema::schema_kind(schema_val).is_some() {
                                        let schema_val = schema_val.clone();
                                        match self
                                            .parse_value(&schema_val, &val, "", false, span)
                                            .await
                                        {
                                            Ok(v) => Ok(make_pair(v, Value::nil())),
                                            Err(crate::stdlib::schema::ParseFail::Mismatch(e)) => {
                                                Ok(make_pair(Value::nil(), e))
                                            }
                                            Err(
                                                crate::stdlib::schema::ParseFail::InvalidSchema(
                                                    msg,
                                                ),
                                            ) => Err(crate::error::AsError::at(msg, span).into()),
                                            Err(crate::stdlib::schema::ParseFail::Control(c)) => {
                                                Err(c)
                                            }
                                        }
                                    } else {
                                        // Non-schema non-class first arg: return raw value.
                                        Ok(make_pair(val, Value::nil()))
                                    }
                                } else {
                                    Ok(make_pair(val, Value::nil()))
                                }
                            }
                            Err(e) => Ok(err_pair(format!("response.json failed: {}", e))),
                        },
                        Err(e) => Ok(err_pair(format!("response.json failed: {}", e))),
                    },
                    _ => unreachable!(),
                }
            }
            other => {
                Err(AsError::at(format!("httpResponse has no method '{}'", other), span).into())
            }
        }
    }

    /// CNTR §3.2: produce the `text()/bytes()/json()/json(Class)` `[value, err]` pair
    /// from a buffered UDS response's body bytes. The `json` arm REUSES the exact same
    /// typed-parse dispatch as the reqwest path (Class → `validate_into`; tagged-schema
    /// Object → `parse_value`; else raw) so the script-visible surface is identical.
    #[cfg(unix)]
    #[async_recursion::async_recursion(?Send)]
    async fn uds_body_accessor(
        &self,
        method: &str,
        buf: Vec<u8>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "text" => Ok(make_pair(
                Value::str(String::from_utf8_lossy(&buf).into_owned()),
                Value::nil(),
            )),
            "bytes" => Ok(make_pair(bytes_value(buf), Value::nil())),
            "json" => match serde_json::from_slice::<serde_json::Value>(&buf) {
                Ok(jv) => {
                    let val = crate::stdlib::json::to_ascript(&jv);
                    if let Some(ValueKind::Class(c)) = args.first().map(|x| x.kind()) {
                        let strict = matches!(
                            args.get(1).map(|x| x.kind()),
                            Some(ValueKind::Bool(true))
                        );
                        match self.validate_into(c, &val, strict, "", span).await {
                            Ok(inst) => Ok(make_pair(inst, Value::nil())),
                            Err(e) => Ok(err_pair(e.message)),
                        }
                    } else if let Some(schema_val) = args.first() {
                        if crate::stdlib::schema::schema_kind(schema_val).is_some() {
                            let schema_val = schema_val.clone();
                            match self.parse_value(&schema_val, &val, "", false, span).await {
                                Ok(v) => Ok(make_pair(v, Value::nil())),
                                Err(crate::stdlib::schema::ParseFail::Mismatch(e)) => {
                                    Ok(make_pair(Value::nil(), e))
                                }
                                Err(crate::stdlib::schema::ParseFail::InvalidSchema(msg)) => {
                                    Err(crate::error::AsError::at(msg, span).into())
                                }
                                Err(crate::stdlib::schema::ParseFail::Control(c)) => Err(c),
                            }
                        } else {
                            Ok(make_pair(val, Value::nil()))
                        }
                    } else {
                        Ok(make_pair(val, Value::nil()))
                    }
                }
                Err(e) => Ok(err_pair(format!("response.json failed: {}", e))),
            },
            _ => unreachable!("uds_body_accessor only handles text/bytes/json"),
        }
    }

    /// Dispatch a read method on a streaming HTTP body handle (`resp.body`):
    /// `read(n?)` / `readLine()` / `readToEnd()` — the §11.4 reader idiom, reused
    /// verbatim from net/tcp + std/process over the chunked byte stream. The body
    /// finalizes itself on EOF (`take_resource`), so a read after EOF returns nil
    /// (or empty bytes for `readToEnd`) rather than panicking, and the stream's
    /// connection drops promptly.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_http_body_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "read" => {
                let n = match args.first() {
                    None => DEFAULT_CHUNK,
                    Some(v) if matches!(v.kind(), ValueKind::Nil) => DEFAULT_CHUNK,
                    // Guard before the cast: an `Inf`/`NaN`/out-of-range `n` would cast
                    // to `usize::MAX` and abort the host via `buf.reserve(n)`.
                    Some(v) => super::want_count(v, span, "body.read", super::MAX_ALLOC_COUNT)?,
                };
                // read(0) is a no-op: return an empty chunk WITHOUT touching the
                // resource (an empty buffer yields Ok(0), which would otherwise be
                // treated as EOF and finalize a still-open body).
                if n == 0 {
                    // read(0) must not touch the resource. Inspect mode under a borrow.
                    let mode = self.with_resource(id, |r| match r {
                        Some(ResourceState::HttpBody(b)) => Some(b.mode),
                        _ => None,
                    });
                    return match mode {
                        Some(mode) => Ok(mode.wrap(Vec::new())),
                        None => Ok(Value::nil()), // gone → EOF
                    };
                }
                // Take the body OUT so no table borrow is held across the await.
                let mut body = match self.take_resource(id) {
                    Some(ResourceState::HttpBody(b)) => b,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(Value::nil()); // gone → EOF
                    }
                };
                let mode = body.mode;
                let mut buf = Vec::new();
                match body.read_upto(n, &mut buf).await {
                    Ok(0) => Ok(Value::nil()), // EOF: drop the body
                    Ok(_) => {
                        self.return_resource(id, ResourceState::HttpBody(body));
                        Ok(mode.wrap(buf))
                    }
                    Err(e) => Err(AsError::at(format!("body.read failed: {}", e), span).into()),
                }
            }
            "readLine" => {
                let mut body = match self.take_resource(id) {
                    Some(ResourceState::HttpBody(b)) => b,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(Value::nil()); // gone → EOF
                    }
                };
                let mode = body.mode;
                let mut buf = Vec::new();
                match body.read_line_bytes(&mut buf).await {
                    Ok(0) => Ok(Value::nil()), // EOF: drop the body
                    Ok(_) => {
                        // Strip a single trailing '\n' and an optional preceding '\r'.
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                            if buf.last() == Some(&b'\r') {
                                buf.pop();
                            }
                        }
                        self.return_resource(id, ResourceState::HttpBody(body));
                        Ok(mode.wrap(buf))
                    }
                    Err(e) => Err(AsError::at(format!("body.readLine failed: {}", e), span).into()),
                }
            }
            "readToEnd" => {
                // readToEnd is type-stable: it ALWAYS returns a value in the body's
                // mode (empty if already drained / finalized).
                let mut body = match self.take_resource(id) {
                    Some(ResourceState::HttpBody(b)) => b,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(BodyMode::Str.wrap(Vec::new()));
                    }
                };
                let mode = body.mode;
                let mut buf = Vec::new();
                // readToEnd consumes the whole body; we drop it either way.
                match body.read_to_end_bytes(&mut buf).await {
                    Ok(_) => Ok(mode.wrap(buf)),
                    Err(e) => {
                        Err(AsError::at(format!("body.readToEnd failed: {}", e), span).into())
                    }
                }
            }
            other => Err(AsError::at(format!("httpBody has no method '{}'", other), span).into()),
        }
    }

    /// `http.cancelToken()` → a `CancelHandle` native handle. Its `cancel()` method
    /// aborts any in-flight request that was passed this handle via `opts.cancel`.
    fn make_cancel_token(&self) -> Value {
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        self.register_resource(
            NativeKind::CancelHandle,
            IndexMap::new(),
            ResourceState::CancelToken(notify),
        )
    }

    /// Dispatch a method on a `CancelHandle`: only `cancel()` (notifies waiters).
    pub(crate) async fn call_cancel_method(
        &self,
        m: &Rc<NativeMethod>,
        _args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match m.method.as_str() {
            "cancel" => {
                self.with_resource(m.receiver.id, |r| {
                    if let Some(ResourceState::CancelToken(n)) = r {
                        // `notify_one` stores a permit, so a cancel that lands *before*
                        // the request's `notified()` is registered still aborts the next
                        // (and only the next) send — important on the single-threaded
                        // interp where `cancel()` and the request run sequentially.
                        n.notify_one();
                    }
                });
                Ok(Value::nil())
            }
            other => {
                Err(AsError::at(format!("cancelHandle has no method '{}'", other), span).into())
            }
        }
    }

    /// Resolve `opts.cancel` (a `CancelHandle`) into its shared `Notify`, if present.
    fn resolve_cancel(
        &self,
        opts: &Value,
        span: Span,
    ) -> Result<Option<std::sync::Arc<tokio::sync::Notify>>, Control> {
        let c = match opt_field(opts, "cancel") {
            Some(v) => v,
            None => return Ok(None),
        };
        match c.kind() {
            ValueKind::Native(n) if n.kind == NativeKind::CancelHandle => {
                Ok(self.with_resource(n.id, |r| match r {
                    Some(ResourceState::CancelToken(notify)) => Some(notify.clone()),
                    _ => None,
                }))
            }
            _ => Err(AsError::at(
                format!(
                    "net/http cancel expects a cancelToken() handle, got {}",
                    crate::interp::type_name(&c)
                ),
                span,
            )
            .into()),
        }
    }

    /// `http.sse(url, opts?)` → `[stream, err]`. Issues a GET with
    /// `Accept: text/event-stream`, then registers the response body behind a
    /// `Value::native(SseStream)` whose `next()` parses the event stream. A
    /// connect-time failure is the usual Tier-1 `[nil, err]`.
    async fn call_sse(&self, url: String, opts: &Value, span: Span) -> Result<Value, Control> {
        // FFI §4.4 stage-2 (net carve-out, BLOCKER 1): re-check the host before the
        // SSE GET (an auto-reconnecting long-lived stream). Gate-12: no carve-out → no-op.
        if let Some(host) = crate::stdlib::caps::host_of_url(&url) {
            self.check_net_host(&host, span)?;
        }
        // Replayable headers + auth — captured so reconnect re-issues an identical GET.
        let headers = sse_headers(opts, span)?;
        let auth = sse_auth(opts, span)?;
        let reconnect = SseReconnect {
            url: url.clone(),
            headers,
            auth,
            // Auto-reconnect default ON; `reconnect:false` disables.
            enabled: !matches!(
                opt_field(opts, "reconnect").map(|x| x.into_kind()),
                Some(OwnedKind::Bool(false))
            ),
            retry_default_ms: strict_num_field_v(opts, "retryDefault", "sse.retryDefault", span)?
                .unwrap_or(3000.0)
                .max(0.0) as u64,
            max: strict_num_field_v(opts, "maxReconnects", "sse.maxReconnects", span)?
                .map(|n| n.max(0.0) as u32),
            count: 0,
        };
        let resp = match self
            .sse_connect(
                &reconnect.url,
                &reconnect.headers,
                &reconnect.auth,
                None,
                span,
            )
            .await
        {
            Ok(r) => r,
            Err(pair) => return Ok(pair),
        };
        let state = SseState::new(sse_reader(resp), reconnect);
        let handle = self.register_resource(
            NativeKind::SseStream,
            IndexMap::new(),
            ResourceState::SseStream(Box::new(state)),
        );
        Ok(make_pair(handle, Value::nil()))
    }

    /// Issue one SSE GET (initial or reconnect). `last_event_id` (when Some/non-empty)
    /// is sent as the `Last-Event-ID` header. Returns the response or a Tier-1 pair.
    async fn sse_connect(
        &self,
        url: &str,
        headers: &[(String, String)],
        auth: &Option<SseAuth>,
        last_event_id: Option<&str>,
        _span: Span,
    ) -> Result<reqwest::Response, Value> {
        let mut rb = default_client()
            .get(url)
            .header(reqwest::header::ACCEPT, "text/event-stream");
        for (k, v) in headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        match auth {
            Some(SseAuth::Bearer(tok)) => rb = rb.bearer_auth(tok),
            Some(SseAuth::Basic(user, pass)) => rb = rb.basic_auth(user, pass.clone()),
            None => {}
        }
        if let Some(id) = last_event_id {
            if !id.is_empty() {
                rb = rb.header("last-event-id", id);
            }
        }
        rb.send()
            .await
            .map_err(|e| err_pair(format!("net/http sse GET {} failed: {}", url, e)))
    }

    /// Dispatch a method on an `SseStream` handle: `next()` / `close()`.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_sse_method(
        &self,
        m: &Rc<NativeMethod>,
        _args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "next" => self.sse_next(id, span).await,
            "close" => {
                // Drop the resource (and its underlying connection); subsequent
                // next() finds no resource and returns nil.
                self.take_resource(id);
                Ok(Value::nil())
            }
            other => Err(AsError::at(format!("sseStream has no method '{}'", other), span).into()),
        }
    }

    /// `stream.next()` → `[event, nil]` for the next parsed event, or `nil` when the
    /// stream ends (EOF + no/exhausted reconnect). On a blank-line boundary the
    /// buffered event is dispatched; on EOF either reconnect (after the retry delay,
    /// carrying Last-Event-ID) and continue, or end.
    async fn sse_next(&self, id: u64, span: Span) -> Result<Value, Control> {
        loop {
            // Read the next line from the current body. Take the state OUT so no
            // table borrow is held across the `read_until` await, then put it back.
            let mut state = match self.take_resource(id) {
                Some(ResourceState::SseStream(s)) => s,
                other => {
                    if let Some(o) = other {
                        self.return_resource(id, o);
                    }
                    return Ok(Value::nil()); // closed/gone → ended
                }
            };
            let line = {
                let mut buf = Vec::new();
                match state.reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => None, // EOF on the current connection
                    Ok(_) => {
                        // Strip the trailing '\n' and an optional preceding '\r'.
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                            if buf.last() == Some(&b'\r') {
                                buf.pop();
                            }
                        }
                        Some(String::from_utf8_lossy(&buf).into_owned())
                    }
                    Err(e) => {
                        return Err(
                            AsError::at(format!("net/http sse read failed: {}", e), span).into(),
                        )
                    }
                }
            };

            match line {
                Some(line) if line.is_empty() => {
                    // Blank line: dispatch the buffered event, if any.
                    let ev = state.take_event();
                    self.return_resource(id, ResourceState::SseStream(state));
                    if let Some(ev) = ev {
                        return Ok(make_pair(ev, Value::nil()));
                    }
                    // else: stray blank line, keep reading.
                }
                Some(line) => {
                    state.apply_line(&line);
                    self.return_resource(id, ResourceState::SseStream(state));
                }
                None => {
                    // EOF on the current connection. Dispatch any event buffered
                    // without a trailing blank line, then attempt reconnect.
                    let ev = state.take_event();
                    // Put the state back so `sse_reconnect` can read its plan.
                    self.return_resource(id, ResourceState::SseStream(state));
                    if let Some(ev) = ev {
                        return Ok(make_pair(ev, Value::nil()));
                    }
                    if !self.sse_reconnect(id, span).await? {
                        // No reconnect (off / cap reached / connect failed) → ended.
                        self.take_resource(id);
                        return Ok(Value::nil());
                    }
                    // Reconnected: loop to read from the fresh body.
                }
            }
        }
    }

    /// Attempt to reconnect a disconnected SSE stream. Returns `Ok(true)` if a fresh
    /// body is now in place (caller resumes reading), `Ok(false)` if reconnect is
    /// off, the `maxReconnects` cap is reached, or the reconnect GET itself failed.
    async fn sse_reconnect(&self, id: u64, span: Span) -> Result<bool, Control> {
        // Pull the reconnect plan (url/headers/delay/last id) without holding the
        // borrow across the await.
        let plan = self.with_resource_mut(id, |r| {
            let state = match r {
                Some(ResourceState::SseStream(s)) => s,
                _ => return None,
            };
            if !state.reconnect.enabled {
                return None;
            }
            if let Some(max) = state.reconnect.max {
                if state.reconnect.count >= max {
                    return None;
                }
            }
            state.reconnect.count += 1;
            let delay = state.retry_ms.unwrap_or(state.reconnect.retry_default_ms);
            Some((
                state.reconnect.url.clone(),
                state.reconnect.headers.clone(),
                state.reconnect.auth.clone(),
                delay,
                state.last_event_id.clone(),
            ))
        });
        let (url, headers, auth, delay, last_id) = match plan {
            Some(p) => p,
            None => return Ok(false),
        };
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        let resp = match self
            .sse_connect(&url, &headers, &auth, Some(&last_id), span)
            .await
        {
            Ok(r) => r,
            Err(_pair) => return Ok(false), // a failed reconnect ends the stream
        };
        self.with_resource_mut(id, |r| {
            if let Some(ResourceState::SseStream(state)) = r {
                state.reader = sse_reader(resp);
                Ok(true)
            } else {
                Ok(false) // closed during the await
            }
        })
    }
}

/// Build the replayable header list for an SSE request from `opts.headers` (the
/// same shape as the regular client). `auth:` is also honored via `opts.auth`.
fn sse_headers(opts: &Value, span: Span) -> Result<Vec<(String, String)>, Control> {
    let mut out = Vec::new();
    if let Some(h) = opt_field(opts, "headers") {
        let map = match h.kind() {
            ValueKind::Object(o) => o,
            _ => {
                return Err(AsError::at(
                    format!(
                        "net/http sse headers expects an object, got {}",
                        crate::interp::type_name(&h)
                    ),
                    span,
                )
                .into())
            }
        };
        for (k, v) in map.entries() {
            out.push((k.to_string(), scalar_to_string(&v, span, "net/http sse header")?));
        }
    }
    Ok(out)
}

/// Capture `opts.auth` ({bearer} | {basic:[user,pass]}) for replay on each connect
/// via reqwest's `RequestBuilder` (so no base64 dependency is needed here).
fn sse_auth(opts: &Value, span: Span) -> Result<Option<SseAuth>, Control> {
    let a = match opt_field(opts, "auth") {
        Some(v) => v,
        None => return Ok(None),
    };
    let o = match a.kind() {
        ValueKind::Object(o) => o.clone(),
        _ => {
            return Err(AsError::at(
                format!(
                    "net/http sse auth expects an object, got {}",
                    crate::interp::type_name(&a)
                ),
                span,
            )
            .into())
        }
    };
    if let Some(tok) = o.get("bearer") {
        let tok = want_string(&tok, span, "net/http sse auth.bearer")?;
        return Ok(Some(SseAuth::Bearer(tok.to_string())));
    }
    if let Some(basic) = o.get("basic") {
        let arr = super::want_array(&basic, span, "net/http sse auth.basic")?;
        let arr = arr.borrow();
        let user = want_string(
            arr.first().unwrap_or(&Value::nil()),
            span,
            "net/http sse auth.basic[0]",
        )?;
        let pass = match arr.get(1) {
            None => None,
            Some(p) if matches!(p.kind(), ValueKind::Nil) => None,
            Some(p) => Some(want_string(p, span, "net/http sse auth.basic[1]")?.to_string()),
        };
        return Ok(Some(SseAuth::Basic(user.to_string(), pass)));
    }
    Err(AsError::at(
        "net/http sse auth expects {bearer} or {basic:[user,pass]}",
        span,
    )
    .into())
}

/// `strict_num_field` over an `opts` value directly (it may not be an object): a
/// present-but-wrong-type field is a Tier-2 type error; absent/nil → `Ok(None)`.
fn strict_num_field_v(
    opts: &Value,
    key: &str,
    ctx: &str,
    span: Span,
) -> Result<Option<f64>, Control> {
    match opt_field(opts, key) {
        None => Ok(None),
        // NUM §4: accept BOTH numeric subtypes (`Int` and `Float`).
        Some(ref v) if v.is_number() => Ok(v.as_f64()),
        Some(other) => Err(AsError::at(
            format!(
                "net/http {} expects a number, got {}",
                ctx,
                crate::interp::type_name(&other)
            ),
            span,
        )
        .into()),
    }
}

/// reqwest's HTTP `Version` → the spec's short string ("1.1" | "2" | "3" | ...).
fn http_version_str(v: reqwest::Version) -> &'static str {
    match v {
        reqwest::Version::HTTP_09 => "0.9",
        reqwest::Version::HTTP_10 => "1.0",
        reqwest::Version::HTTP_11 => "1.1",
        reqwest::Version::HTTP_2 => "2",
        reqwest::Version::HTTP_3 => "3",
        _ => "1.1",
    }
}

/// Parse the `name=value` prefix of a single `Set-Cookie` header (attributes after
/// the first `;` are ignored — a deliberately simple name→value model).
fn parse_set_cookie(header: &str) -> Option<(String, String)> {
    let first = header.split(';').next()?.trim();
    let (name, value) = first.split_once('=')?;
    Some((name.trim().to_string(), value.trim().to_string()))
}

/// Build a `reqwest::multipart::Form` from a `{multipart:[...]}` array. Each entry is
/// `{name, value}` (a text field) or `{name, data, filename?, contentType?}` (a file
/// / bytes part, where `data` is a string or bytes).
fn build_multipart(mp: &Value, span: Span) -> Result<reqwest::multipart::Form, Control> {
    let arr = super::want_array(mp, span, "net/http body.multipart")?;
    let mut form = reqwest::multipart::Form::new();
    for entry in arr.borrow().iter() {
        let o = match entry.kind() {
            ValueKind::Object(o) => o,
            _ => {
                return Err(AsError::at(
                    format!(
                        "net/http multipart part must be an object, got {}",
                        crate::interp::type_name(entry)
                    ),
                    span,
                )
                .into())
            }
        };
        let name = match o.get("name") {
            Some(n) => want_string(&n, span, "net/http multipart part.name")?.to_string(),
            None => return Err(AsError::at("net/http multipart part requires a name", span).into()),
        };
        if let Some(data) = o.get("data") {
            let bytes = match data.kind() {
                ValueKind::Str(s) => s.as_bytes().to_vec(),
                ValueKind::Bytes(b) => b.borrow().clone(),
                _ => {
                    return Err(AsError::at(
                        format!(
                            "net/http multipart data must be string/bytes, got {}",
                            crate::interp::type_name(&data)
                        ),
                        span,
                    )
                    .into())
                }
            };
            let mut part = reqwest::multipart::Part::bytes(bytes);
            if let Some(fname) = o.get("filename") {
                let fname = want_string(&fname, span, "net/http multipart part.filename")?;
                part = part.file_name(fname.to_string());
            }
            if let Some(ct) = o.get("contentType") {
                let ct = want_string(&ct, span, "net/http multipart part.contentType")?;
                part = part.mime_str(&ct).map_err(|e| {
                    Control::from(AsError::at(
                        format!("net/http multipart contentType: {}", e),
                        span,
                    ))
                })?;
            }
            form = form.part(name, part);
        } else if let Some(value) = o.get("value") {
            let value = scalar_to_string(&value, span, "net/http multipart part.value")?;
            form = form.text(name, value);
        } else {
            return Err(
                AsError::at("net/http multipart part requires `value` or `data`", span).into(),
            );
        }
    }
    Ok(form)
}

#[cfg(test)]
mod tests {
    use crate::interp::Interp;

    /// Run an AScript program and return its captured output.
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    /// Run on a caller-held interp (so resource state can be inspected after).
    async fn run_on(interp: &Interp, src: &str) -> Result<(), crate::interp::Control> {
        let tokens = crate::lexer::lex(src).expect("lex");
        let program = crate::parser::parse(&tokens).expect("parse");
        let env = crate::interp::global_env().child();
        interp.exec(&program, &env).await.map(|_| ())
    }

    // ---- in-process HTTP/1 test fixture (hyper 1.x) -------------------------
    //
    // Starts a hyper HTTP/1 server on 127.0.0.1:0 in a spawned tokio task, returns
    // the base URL `http://127.0.0.1:{port}`. Dispatches on path:
    //   /text          → 200 "hello"
    //   /json          → 200 {"x":1,"items":[1,2,3]} (application/json)
    //   /echo          → 200 JSON {method, headers:{...}, body:"..."} reflecting the request
    //   /status/404    → 404
    //   /redirect      → 302 Location: /text
    // Reused by Tasks 3-5.
    mod fixture {
        use http_body_util::combinators::BoxBody;
        use http_body_util::{BodyExt, Full};
        use hyper::body::{Bytes, Incoming};
        use hyper::service::service_fn;
        use hyper::{Request, Response, StatusCode};
        use hyper_util::rt::TokioIo;
        use std::convert::Infallible;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::net::TcpListener;

        /// Per-server hit counter for the `/flaky` endpoint (fail N times then 200).
        type Flaky = Arc<AtomicUsize>;

        async fn handle(
            req: Request<Incoming>,
            flaky: Flaky,
        ) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
            let method = req.method().to_string();
            let path = req.uri().path().to_string();
            let query = req.uri().query().unwrap_or("").to_string();
            // Tiny query parser: first value for `key` in `a=1&b=2`.
            let qval = |key: &str| -> Option<String> {
                query.split('&').find_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    if k == key {
                        Some(v.to_string())
                    } else {
                        None
                    }
                })
            };
            // Collect headers before consuming the body.
            let mut headers = serde_json::Map::new();
            for (name, value) in req.headers().iter() {
                headers.insert(
                    name.as_str().to_ascii_lowercase(),
                    serde_json::Value::String(value.to_str().unwrap_or("").to_string()),
                );
            }
            let body_bytes = req
                .into_body()
                .collect()
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            let resp = match path.as_str() {
                "/text" => Response::new(Full::new(Bytes::from_static(b"hello"))),
                "/json" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(
                        b"{\"x\":1,\"items\":[1,2,3]}",
                    )));
                    r.headers_mut().insert(
                        hyper::header::CONTENT_TYPE,
                        "application/json".parse().unwrap(),
                    );
                    r
                }
                "/echo" => {
                    let echo = serde_json::json!({
                        "method": method,
                        "headers": serde_json::Value::Object(headers),
                        "body": body_str,
                    });
                    let mut r = Response::new(Full::new(Bytes::from(echo.to_string())));
                    r.headers_mut().insert(
                        hyper::header::CONTENT_TYPE,
                        "application/json".parse().unwrap(),
                    );
                    r
                }
                "/status/404" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(b"not found")));
                    *r.status_mut() = StatusCode::NOT_FOUND;
                    r
                }
                "/redirect" => {
                    let mut r = Response::new(Full::new(Bytes::new()));
                    *r.status_mut() = StatusCode::FOUND;
                    r.headers_mut()
                        .insert(hyper::header::LOCATION, "/text".parse().unwrap());
                    r
                }
                // Sleep `?ms=N` (default 0) then 200 "slow". For timeout/cancel tests.
                "/slow" => {
                    let ms: u64 = qval("ms").and_then(|s| s.parse().ok()).unwrap_or(0);
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    Response::new(Full::new(Bytes::from_static(b"slow")))
                }
                // Fail with 503 the first `?fail=N` (default 2) hits, then 200 "ok".
                // The counter is per-server (per fixture::start), so each test gets a
                // fresh sequence. Used by the retry tests.
                "/flaky" => {
                    let fail: usize = qval("fail").and_then(|s| s.parse().ok()).unwrap_or(2);
                    let n = flaky.fetch_add(1, Ordering::SeqCst);
                    if n < fail {
                        let mut r = Response::new(Full::new(Bytes::from_static(b"try again")));
                        *r.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                        r
                    } else {
                        Response::new(Full::new(Bytes::from(format!("ok after {}", n))))
                    }
                }
                "/status/500" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(b"boom")));
                    *r.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                    r
                }
                "/status/503" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(b"unavailable")));
                    *r.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                    r
                }
                // Sets a cookie, then redirects to /checkcookie. A client with a jar
                // (cookies:true) carries the cookie across the redirect within the
                // same request's client; /checkcookie echoes whether it arrived.
                "/setcookie" => {
                    let mut r = Response::new(Full::new(Bytes::new()));
                    *r.status_mut() = StatusCode::FOUND;
                    r.headers_mut().insert(
                        hyper::header::SET_COOKIE,
                        "sid=abc123; Path=/".parse().unwrap(),
                    );
                    r.headers_mut()
                        .insert(hyper::header::LOCATION, "/checkcookie".parse().unwrap());
                    r
                }
                "/checkcookie" => {
                    let cookie = headers
                        .get("cookie")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Response::new(Full::new(Bytes::from(format!("cookie={}", cookie))))
                }
                // A chunked/multi-frame body for streaming-response tests: emits
                // "chunk1\nchunk2\nchunk3\n" across SEPARATE body frames (via a
                // `StreamBody` of `Frame`s) so the client must pull multiple chunks.
                // Uses a boxed body, hence the `BoxBody` return type below.
                "/stream" => {
                    use http_body_util::StreamBody;
                    use hyper::body::Frame;
                    let frames = vec![
                        Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"chunk1\n"))),
                        Ok(Frame::data(Bytes::from_static(b"chunk2\n"))),
                        Ok(Frame::data(Bytes::from_static(b"chunk3\n"))),
                    ];
                    let stream = futures_util::stream::iter(frames);
                    return Ok(Response::new(BoxBody::new(StreamBody::new(stream))));
                }
                // A canned `text/event-stream` body with three events: an
                // event+data frame, a multi-line data + id + retry frame, and a
                // default-event data frame. A leading `:` comment line is ignored.
                // The body then ends (connection closes) — no reconnect data.
                "/sse" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(
                        b": this is a comment\nevent: greeting\ndata: hello\n\ndata: line1\ndata: line2\nid: 42\nretry: 1000\n\ndata: bye\n\n",
                    )));
                    r.headers_mut().insert(
                        hyper::header::CONTENT_TYPE,
                        "text/event-stream".parse().unwrap(),
                    );
                    r
                }
                // Auto-reconnect endpoint: the FIRST connection (no Last-Event-ID
                // header) emits one event (id:1) then closes; the SECOND connection
                // — detected by the presence of a `Last-Event-ID` header — emits a
                // second event (id:2) then closes. Lets the reconnect test assert
                // both the resumed event AND that Last-Event-ID was sent.
                "/sse-reconnect" => {
                    let has_last_id = headers.contains_key("last-event-id");
                    let payload: &[u8] = if has_last_id {
                        b"data: second\nid: 2\n\n"
                    } else {
                        b"data: first\nid: 1\n\n"
                    };
                    let mut r = Response::new(Full::new(Bytes::from(payload.to_vec())));
                    r.headers_mut().insert(
                        hyper::header::CONTENT_TYPE,
                        "text/event-stream".parse().unwrap(),
                    );
                    r
                }
                _ => {
                    let mut r = Response::new(Full::new(Bytes::from_static(b"nope")));
                    *r.status_mut() = StatusCode::NOT_FOUND;
                    r
                }
            };
            // The non-stream arms build a `Full`-bodied response; box it so this
            // handler's return type is uniform with the `/stream` arm's `BoxBody`.
            let (parts, body) = resp.into_parts();
            Ok(Response::from_parts(parts, BoxBody::new(body)))
        }

        /// Start the fixture; returns `http://127.0.0.1:{port}`.
        pub async fn start() -> String {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().unwrap();
            let flaky: Flaky = Arc::new(AtomicUsize::new(0));
            tokio::spawn(async move {
                loop {
                    let (stream, _) = match listener.accept().await {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let io = TokioIo::new(stream);
                    let flaky = flaky.clone();
                    tokio::spawn(async move {
                        let svc = service_fn(move |req| handle(req, flaky.clone()));
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, svc)
                            .await;
                    });
                }
            });
            format!("http://127.0.0.1:{}", addr.port())
        }
    }

    #[tokio::test]
    async fn get_text_ok_status_and_body() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/text")
print(err)
print(resp.ok)
print(resp.status)
let [body, berr] = await resp.text()
print(berr)
print(body)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\ntrue\n200\nnil\nhello\n");
    }

    #[tokio::test]
    async fn trailers_is_an_empty_object() {
        // §11.5 shape: resp.trailers is best-effort, exposed as an empty object
        // (not a generic NativeMethod stub).
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/text")
print(type(resp.trailers))
print(len(resp.trailers))
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "object\n0\n");
    }

    #[tokio::test]
    async fn get_json_parses_to_object() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/json")
let [data, jerr] = await resp.json()
print(jerr)
print(data.x)
print(data.items[2])
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\n1\n3\n");
    }

    #[tokio::test]
    async fn non_2xx_is_not_an_error() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/status/404")
print(err)
print(resp.ok)
print(resp.status)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\nfalse\n404\n");
    }

    #[tokio::test]
    async fn post_json_body_reflected_with_content_type() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ post }} from "std/net/http"
let [resp, _e] = await post("{base}/echo", {{ body: {{ json: {{ a: 1 }} }} }})
let [data, _je] = await resp.json()
print(data.method)
print(data.body)
print(data.headers["content-type"])
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "POST\n{\"a\":1}\napplication/json\n");
    }

    #[tokio::test]
    async fn post_form_body_urlencoded() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ post }} from "std/net/http"
let [resp, _e] = await post("{base}/echo", {{ body: {{ form: {{ k: "v" }} }} }})
let [data, _je] = await resp.json()
print(data.body)
print(data.headers["content-type"])
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "k=v\napplication/x-www-form-urlencoded\n");
    }

    #[tokio::test]
    async fn headers_and_bearer_auth_reflected() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/echo", {{ headers: {{ "x-test": "yes" }}, auth: {{ bearer: "tok" }} }})
let [data, _je] = await resp.json()
print(data.headers["x-test"])
print(data.headers["authorization"])
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "yes\nBearer tok\n");
    }

    #[tokio::test]
    async fn query_object_merged_into_url() {
        let base = fixture::start().await;
        // /echo reflects the request; assert the final URL carried the query string.
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
import {{ find }} from "std/string"
let [resp, _e] = await get("{base}/echo", {{ query: {{ a: "1", b: "two" }} }})
print(find(resp.url, "a=1") >= 0)
print(find(resp.url, "b=two") >= 0)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "true\ntrue\n");
    }

    #[tokio::test]
    async fn connect_failure_is_tier1_err_no_panic() {
        // Port 1 has nothing listening → a connect error, surfaced as a Tier-1 err.
        let out = run(r#"
import { get } from "std/net/http"
let [resp, err] = await get("http://127.0.0.1:1/")
print(resp)
print(err != nil)
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn double_body_consume_is_tier2_panic() {
        let base = fixture::start().await;
        let interp = Interp::new();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/text")
let [_t, _te] = await resp.text()
let [_b, _be] = await resp.bytes()
"#
        );
        let res = run_on(&interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(msg.contains("already consumed"), "got: {}", msg);
            }
            other => panic!("expected a Tier-2 panic, got ok={:?}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn interp_e2e_get_json_destructured() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
fn fetch() {{
  let [resp, err] = await get("{base}/json")
  if (err != nil) {{ return -1 }}
  let [data, jerr] = await resp.json()
  if (jerr != nil) {{ return -2 }}
  return data.x + data.items[0] + data.items[2]
}}
print(fetch())
"#
        );
        let out = run(&src).await;
        // x=1, items[0]=1, items[2]=3 → 5
        assert_eq!(out, "5\n");
    }

    // ---- Task 3: advanced request options -----------------------------------

    #[tokio::test]
    async fn timeout_total_expiry_is_tier1_err() {
        let base = fixture::start().await;
        // /slow sleeps 300ms; a 50ms total timeout must surface a Tier-1 err.
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/slow?ms=300", {{ timeout: {{ total: 50 }} }})
print(resp)
print(err != nil)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn redirect_follow_by_default_reaches_final_body() {
        let base = fixture::start().await;
        // /redirect → 302 Location:/text. Default policy follows it to "hello".
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/redirect")
print(resp.status)
let [body, _be] = await resp.text()
print(body)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "200\nhello\n");
    }

    #[tokio::test]
    async fn redirect_none_returns_302_not_followed() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/redirect", {{ redirect: "none" }})
print(resp.status)
print(resp.ok)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "302\nfalse\n");
    }

    #[tokio::test]
    async fn retry_eventually_succeeds_after_failures() {
        let base = fixture::start().await;
        // /flaky?fail=2 returns 503 twice then "ok after 2". retryOn 503 with max 3.
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/flaky?fail=2", {{ retry: {{ max: 3, baseDelay: 1, retryOn: [503] }} }})
print(err)
print(resp.status)
let [body, _be] = await resp.text()
print(body)
"#
        );
        let out = run(&src).await;
        // Took 2 retries to reach the 3rd hit (index 2) → "ok after 2".
        assert_eq!(out, "nil\n200\nok after 2\n");
    }

    #[tokio::test]
    async fn retry_max_wrong_type_is_tier2_panic() {
        let base = fixture::start().await;
        // A non-number retry.max is a type error (parity with timeout/redirect),
        // not a silent "retries off" — it must Tier-2 panic with a clear message.
        let interp = Interp::new();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [_r, _e] = await get("{base}/text", {{ retry: {{ max: "y" }} }})
"#
        );
        let res = run_on(&interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(msg.contains("retry.max expects a number"), "got: {}", msg);
            }
            other => panic!("expected a Tier-2 panic, got ok={:?}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn retry_disabled_by_default_returns_first_503() {
        let base = fixture::start().await;
        // Without a retry opt, the first 503 is returned as-is (no retry).
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/flaky?fail=2")
print(resp.status)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "503\n");
    }

    #[tokio::test]
    async fn error_on_status_turns_404_into_err() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/status/404", {{ errorOnStatus: true }})
print(resp)
print(err != nil)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn decompress_default_decodes_response() {
        // The h1 fixture serves plain (uncompressed) bodies; assert that the default
        // decompress:true path still returns the body intact (Accept-Encoding is
        // advertised, server ignores it). True gzip decode is covered by reqwest's
        // own tests; the fixture cannot easily gzip, so this guards the opt wiring.
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/text", {{ decompress: false }})
let [body, _be] = await resp.text()
print(body)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "hello\n");
    }

    #[tokio::test]
    async fn cookies_jar_carries_cookie_across_redirect() {
        let base = fixture::start().await;
        // /setcookie sets sid + redirects to /checkcookie. With cookies:true the
        // per-request jar stores the Set-Cookie and replays it on the redirect hop,
        // so /checkcookie sees it. Without a jar the cookie would be dropped.
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
import {{ find }} from "std/string"
let [resp, _e] = await get("{base}/setcookie", {{ cookies: true }})
let [body, _be] = await resp.text()
print(find(body, "sid=abc123") >= 0)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "true\n");
    }

    #[tokio::test]
    async fn cookies_off_drops_cookie_across_redirect() {
        let base = fixture::start().await;
        // Without cookies:true there is no jar, so the redirect hop carries no cookie.
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
import {{ find }} from "std/string"
let [resp, _e] = await get("{base}/setcookie")
let [body, _be] = await resp.text()
print(find(body, "sid=abc123") >= 0)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "false\n");
    }

    #[tokio::test]
    async fn http_version_1_1_works_against_h1_fixture() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/text", {{ httpVersion: "1.1" }})
print(err)
print(resp.version)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\n1.1\n");
    }

    #[tokio::test]
    #[cfg(not(feature = "http3"))]
    async fn http_version_3_errors_cleanly_without_feature() {
        let base = fixture::start().await;
        // httpVersion:"3" must surface a clean Tier-1 error (no http3 build feature).
        let interp = Interp::new();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [_r, _e] = await get("{base}/text", {{ httpVersion: "3" }})
"#
        );
        let res = run_on(&interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("HTTP/3 requires the 'http3' build feature"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("expected a clean error, got ok={:?}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn cancel_token_aborts_request() {
        let base = fixture::start().await;
        // The interp runs single-threaded and sequentially, so we cancel the token
        // *before* issuing the request; the stored permit makes the request's
        // select abort immediately rather than waiting out the 5s /slow. The test
        // would hang (or take ~5s) if the cancel path were not wired.
        let src = format!(
            r#"
import {{ get, cancelToken }} from "std/net/http"
let tok = cancelToken()
tok.cancel()
let [resp, err] = await get("{base}/slow?ms=5000", {{ cancel: tok }})
print(resp)
print(err != nil)
"#
        );
        let out = tokio::time::timeout(std::time::Duration::from_secs(2), run(&src))
            .await
            .expect("cancel must abort well under the 5s /slow + 2s budget");
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn cancel_token_unused_does_not_abort() {
        let base = fixture::start().await;
        // A token that is never cancelled must not interfere with a normal request.
        let src = format!(
            r#"
import {{ get, cancelToken }} from "std/net/http"
let tok = cancelToken()
let [resp, err] = await get("{base}/text", {{ cancel: tok }})
print(err)
let [body, _be] = await resp.text()
print(body)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\nhello\n");
    }

    #[tokio::test]
    async fn tls_min_version_opt_parses_against_h1_fixture() {
        // tls/proxy can't be deeply exercised against a plain-h1 in-process fixture;
        // assert the opts at least parse + build a working client (no-op over plain
        // HTTP). Real TLS/proxy behavior is reqwest's own and is documented as not
        // in-process-testable here.
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/text", {{ tls: {{ minVersion: "1.2", sni: true }}, proxy: "none" }})
print(err)
print(resp.status)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\n200\n");
    }

    #[tokio::test]
    async fn tls_cabundle_bad_path_reports_clear_read_error() {
        let base = fixture::start().await;
        // A non-PEM string with no -----BEGIN header is treated as a file path; a
        // missing file must yield a clear "could not read PEM file" error naming the
        // path — not a confusing downstream PEM-parse error.
        let interp = Interp::new();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [_r, _e] = await get("{base}/text", {{ tls: {{ caBundle: "/no/such/ca.pem" }} }})
"#
        );
        let res = run_on(&interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("could not read PEM file '/no/such/ca.pem'"),
                    "got: {}",
                    msg
                );
            }
            other => panic!(
                "expected a clear path-read error, got ok={:?}",
                other.is_ok()
            ),
        }
    }

    #[tokio::test]
    async fn timeout_connect_only_does_not_break_fast_request() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("{base}/text", {{ timeout: {{ connect: 5000 }} }})
print(err)
print(resp.status)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\n200\n");
    }

    // ---- Task 4: streaming response + request bodies ------------------------

    #[tokio::test]
    async fn stream_read_to_end_yields_full_body() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/stream", {{ stream: true }})
let body = await resp.body.readToEnd()
print(type(body))
print(body)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "string\nchunk1\nchunk2\nchunk3\n\n");
    }

    #[tokio::test]
    async fn stream_read_line_yields_lines_then_nil() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/stream", {{ stream: true }})
print(await resp.body.readLine())
print(await resp.body.readLine())
print(await resp.body.readLine())
print(await resp.body.readLine())
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "chunk1\nchunk2\nchunk3\nnil\n");
    }

    #[tokio::test]
    async fn stream_bytes_mode_read_returns_bytes() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/stream", {{ stream: true, bodyMode: "bytes" }})
let chunk = await resp.body.read()
print(type(chunk))
print(len(chunk) > 0)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "bytes\ntrue\n");
    }

    #[tokio::test]
    async fn stream_partial_read_bounds_chunk_and_concatenates() {
        let base = fixture::start().await;
        // read(4) returns at most 4 bytes; successive reads concatenate to the
        // full 21-byte body ("chunk1\nchunk2\nchunk3\n").
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/stream", {{ stream: true }})
let acc = ""
let first = await resp.body.read(4)
print(len(first) <= 4)
acc = acc + first
let part = await resp.body.read(4)
while (part != nil) {{
  acc = acc + part
  part = await resp.body.read(4)
}}
print(acc)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "true\nchunk1\nchunk2\nchunk3\n\n");
    }

    #[tokio::test]
    async fn stream_read_after_eof_is_nil_repeatedly_and_reclaims_resource() {
        let base = fixture::start().await;
        let interp = Interp::new();
        let baseline = interp.resource_count();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/stream", {{ stream: true }})
let _all = await resp.body.readToEnd()
print(await resp.body.read())
print(await resp.body.read())
print(await resp.body.readLine())
"#
        );
        run_on(&interp, &src).await.expect("exec");
        assert_eq!(interp.output(), "nil\nnil\nnil\n");
        assert_eq!(
            interp.resource_count(),
            baseline,
            "HttpBody resource should be reclaimed on EOF"
        );
    }

    #[tokio::test]
    async fn stream_response_text_accessor_is_clear_error() {
        let base = fixture::start().await;
        let interp = Interp::new();
        // With stream:true the response is NOT stored as a buffered Response, so
        // text()/json()/bytes() must surface a clear Tier-2 error.
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/stream", {{ stream: true }})
let [_t, _te] = await resp.text()
"#
        );
        let res = run_on(&interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(msg.contains("streaming"), "got: {}", msg);
            }
            other => panic!("expected a Tier-2 error, got ok={:?}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn non_stream_response_body_accessor_is_clear_error() {
        let base = fixture::start().await;
        let interp = Interp::new();
        // Without stream:true there is no `body` reader; `resp.body` must be a clear
        // error directing the caller to text()/bytes()/json().
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/text")
let x = resp.body
"#
        );
        let res = run_on(&interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("body") && msg.contains("stream"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("expected a Tier-2 error, got ok={:?}", other.is_ok()),
        }
    }

    #[tokio::test]
    async fn request_body_stream_bytes_is_reflected() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ post }} from "std/net/http"
import {{ utf8Encode }} from "std/encoding"
let [resp, _e] = await post("{base}/echo", {{ body: {{ stream: utf8Encode("streamed-bytes") }} }})
let [data, _je] = await resp.json()
print(data.body)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "streamed-bytes\n");
    }

    #[tokio::test]
    async fn request_body_stream_generator_is_buffered_and_reflected() {
        let base = fixture::start().await;
        // An async-generator source: a fn returning [bytes, err] each call, then nil
        // to end. The two chunks are buffered-then-sent; /echo reflects their concat.
        let src = format!(
            r#"
import {{ post }} from "std/net/http"
import {{ utf8Encode }} from "std/encoding"
let calls = 0
fn gen() {{
  calls = calls + 1
  if (calls == 1) {{ return [utf8Encode("part1-"), nil] }}
  if (calls == 2) {{ return [utf8Encode("part2"), nil] }}
  return [nil, nil]
}}
let [resp, _e] = await post("{base}/echo", {{ body: {{ stream: gen }} }})
let [data, _je] = await resp.json()
print(data.body)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "part1-part2\n");
    }

    // ---- Task 5: Server-Sent Events (http.sse) ------------------------------

    #[tokio::test]
    async fn sse_parses_events_in_order_then_nil() {
        let base = fixture::start().await;
        // The canned /sse body has three events: greeting+data, multi-line data +
        // id + retry, and a default-event data. The leading `:` comment is ignored.
        // With reconnect:false the stream ends (nil) after the third event.
        let src = format!(
            r#"
import {{ sse }} from "std/net/http"
let [s, err] = await sse("{base}/sse", {{ reconnect: false }})
print(err)
let e1 = await s.next()
print(e1[0].event)
print(e1[0].data)
let e2 = await s.next()
print(e2[0].event)
print(e2[0].data)
print(e2[0].id)
print(e2[0].retry)
let e3 = await s.next()
print(e3[0].event)
print(e3[0].data)
let e4 = await s.next()
print(e4)
"#
        );
        let out = run(&src).await;
        assert_eq!(
            out,
            "nil\ngreeting\nhello\nmessage\nline1\nline2\n42\n1000\nmessage\nbye\nnil\n"
        );
    }

    #[tokio::test]
    async fn sse_last_event_id_tracks_latest_id() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ sse }} from "std/net/http"
let [s, _e] = await sse("{base}/sse", {{ reconnect: false }})
let _e1 = await s.next()
print(s.lastEventId)
let _e2 = await s.next()
print(s.lastEventId)
"#
        );
        let out = run(&src).await;
        // No id on the first event, "42" after the second event.
        assert_eq!(out, "\n42\n");
    }

    #[tokio::test]
    async fn sse_close_ends_stream_and_reclaims_resource() {
        let base = fixture::start().await;
        let interp = Interp::new();
        let baseline = interp.resource_count();
        let src = format!(
            r#"
import {{ sse }} from "std/net/http"
let [s, _e] = await sse("{base}/sse", {{ reconnect: false }})
let _e1 = await s.next()
s.close()
print(await s.next())
print(await s.next())
"#
        );
        run_on(&interp, &src).await.expect("exec");
        assert_eq!(interp.output(), "nil\nnil\n");
        assert_eq!(
            interp.resource_count(),
            baseline,
            "SseStream resource should be reclaimed on close()"
        );
    }

    #[tokio::test]
    async fn sse_auto_reconnect_resumes_with_last_event_id() {
        let base = fixture::start().await;
        // /sse-reconnect: first connection (no Last-Event-ID) → {data:first,id:1}
        // then closes; reconnect (carrying Last-Event-ID) → {data:second,id:2} then
        // closes. With a tiny retryDefault + maxReconnects:1 we get both events then
        // nil. The second event proves the reconnect carried Last-Event-ID.
        let src = format!(
            r#"
import {{ sse }} from "std/net/http"
let [s, _e] = await sse("{base}/sse-reconnect", {{ retryDefault: 10, maxReconnects: 1 }})
let e1 = await s.next()
print(e1[0].data)
let e2 = await s.next()
print(e2[0].data)
print(e2[0].id)
print(await s.next())
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "first\nsecond\n2\nnil\n");
    }

    #[tokio::test]
    async fn sse_no_reconnect_ends_cleanly_after_first_connection() {
        let base = fixture::start().await;
        // reconnect:false against /sse-reconnect → only the first event, then nil.
        let src = format!(
            r#"
import {{ sse }} from "std/net/http"
let [s, _e] = await sse("{base}/sse-reconnect", {{ reconnect: false }})
let e1 = await s.next()
print(e1[0].data)
print(await s.next())
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "first\nnil\n");
    }

    #[tokio::test]
    async fn sse_interp_e2e_event_loop() {
        let base = fixture::start().await;
        let src = format!(
            r#"
import {{ sse }} from "std/net/http"
fn collect() {{
  let [s, err] = await sse("{base}/sse", {{ reconnect: false }})
  if (err != nil) {{ return "ERR" }}
  let out = ""
  let ev = await s.next()
  while (ev != nil) {{
    out = out + ev[0].event + ":" + ev[0].data + "|"
    ev = await s.next()
  }}
  return out
}}
print(collect())
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "greeting:hello|message:line1\nline2|message:bye|\n");
    }

    #[tokio::test]
    async fn request_body_stream_reader_handle_is_drained_and_reflected() {
        let base = fixture::start().await;
        // A reader-handle source (variant b): open a STREAMING response body (an
        // HttpBody reader) from /stream and pipe THAT into a POST /echo body. The
        // reader is drained (readToEnd) then sent; /echo reflects the full content.
        let src = format!(
            r#"
import {{ get, post }} from "std/net/http"
let [src, _se] = await get("{base}/stream", {{ stream: true }})
let [resp, _e] = await post("{base}/echo", {{ body: {{ stream: src.body }} }})
let [data, _je] = await resp.json()
print(data.body)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "chunk1\nchunk2\nchunk3\n\n");
    }

    // ── RESIL §5.4: deadline-aware HTTP consult site ─────────────────────────

    use crate::interp::{task_locals_scope, TaskLocals};
    use crate::span::Span;
    use crate::value::{Value, ValueKind};
    use std::rc::Rc;

    /// Assert `pair` is `[nil, {code:"deadline-exceeded"}]`.
    fn assert_deadline_pair(pair: &Value) {
        match pair.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                assert_eq!(b.len(), 2, "expected a [value, err] pair");
                assert_eq!(b[0], Value::nil(), "value slot should be nil");
                match b[1].kind() {
                    ValueKind::Object(o) => {
                        assert_eq!(
                            o.get("code"),
                            Some(Value::str("deadline-exceeded")),
                            "err code should be deadline-exceeded"
                        );
                    }
                    other => panic!("err slot should be an object, got {:?}", other),
                }
            }
            other => panic!("expected a pair, got {:?}", other),
        }
    }

    // §5.4: an ALREADY-EXPIRED deadline → call_http_send returns the
    // deadline-exceeded pair IMMEDIATELY, with NO connection attempt. The target is
    // a non-routable address (TEST-NET-1, RFC 5737); if a connect were attempted it
    // would block until the (default) connect timeout — far longer than the assert
    // budget. Returning fast proves the pre-check fired before any connect.
    #[tokio::test(flavor = "current_thread")]
    async fn http_expired_deadline_pre_check_no_connect() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let interp = std::rc::Rc::new(Interp::new());
                interp.install_self();
                // Build an already-expired deadline in the current task locals.
                let now = interp.clock_monotonic_ms(crate::stdlib::time::real_monotonic_ms());
                let locals = Rc::new(TaskLocals {
                    deadline_at_ms: Some(now - 1000.0),
                    trace_id: None,
                });
                let opts = Value::nil();
                let started = std::time::Instant::now();
                let pair = task_locals_scope(Some(locals), async {
                    interp
                        .call_http_send("GET", "http://192.0.2.1/".to_string(), &opts, Span::new(0, 0))
                        .await
                        .expect("must not panic")
                })
                .await;
                let elapsed = started.elapsed();
                assert_deadline_pair(&pair);
                assert!(
                    elapsed < std::time::Duration::from_secs(1),
                    "expired deadline must return immediately (no connect), took {:?}",
                    elapsed
                );
            })
            .await;
    }

    // §5.4: clamp — the effective total timeout is min(requested, remaining). A real
    // listener that NEVER accepts means a connect would hang ~indefinitely (or for
    // the requested 60s total). Under a 50ms deadline the call returns the
    // deadline-exceeded pair in well under that — proving min(60000, 50) clamping.
    #[cfg(feature = "resilience")]
    #[tokio::test(flavor = "current_thread")]
    async fn http_clamp_to_remaining_budget() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Bind a listener and NEVER accept → connects to it hang in the
                // backlog / time out only at the full requested total.
                let listener =
                    std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
                let addr = listener.local_addr().expect("addr");
                // Fill the accept backlog so a fresh connect actually stalls rather
                // than completing the TCP handshake (kernel may auto-accept a few).
                // Even if the handshake completes, no HTTP response ever arrives, so
                // the 60s total would otherwise apply.
                let url = format!("http://{}/", addr);
                let src = format!(
                    r#"
import {{ get }} from "std/net/http"
import * as resilience from "std/resilience"
let [resp, err] = resilience.deadline(50, async () => {{
    return await get("{url}", {{ timeout: {{ total: 60000 }} }})
}})
print(err.code)
"#
                );
                let started = std::time::Instant::now();
                let out = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    crate::run_source(&src),
                )
                .await
                .expect("must return well under the 60s requested total")
                .expect("program should run");
                let elapsed = started.elapsed();
                assert_eq!(out, "deadline-exceeded\n");
                assert!(
                    elapsed < std::time::Duration::from_secs(2),
                    "clamp to the 50ms budget should return fast, took {:?}",
                    elapsed
                );
                drop(listener);
            })
            .await;
    }

    // ---- CNTR §3.2: {socketPath} routing over the http1 UDS client ----------
    //
    // A `socketPath` request speaks HTTP/1.1 over a `UnixStream` (Docker's socket
    // shape). The script-visible surface (status/headers/text()/json()/json(Class)/
    // streaming body.read()) MUST be byte-identical to a TCP request. These tests
    // spawn an in-test UDS HTTP/1.1 server and assert that parity, plus the §3.1
    // carve-out, the streaming-request-body refusal, and the non-unix platform error.
    #[cfg(unix)]
    mod uds {
        use super::run_on;
        use crate::interp::Interp;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixListener;

        /// A throwaway UDS path under the temp dir that unlinks on drop.
        struct UdsTemp {
            path: std::path::PathBuf,
        }
        impl Drop for UdsTemp {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
        fn uds_temp_path(tag: &str) -> UdsTemp {
            let dir = std::env::temp_dir();
            let path = dir.join(format!(
                "ascript-http-uds-{}-{}-{}.sock",
                tag,
                std::process::id(),
                COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
            let _ = std::fs::remove_file(&path);
            UdsTemp { path }
        }
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        /// Spawn an in-test UDS HTTP/1.1 server. Reads one request head (best-effort),
        /// routes on the request-target path, writes a canned HTTP/1.1 response, and
        /// closes the connection. Routes:
        ///   /text     → 200 text/plain "hello" (Content-Length)
        ///   /json     → 200 application/json {"ok":true} (Content-Length)
        ///   /chunked  → 200 Transfer-Encoding: chunked, three data chunks then terminal 0
        /// Returns the socket path string (kept alive by the returned guard).
        fn spawn_uds_http_fixture() -> (String, UdsTemp) {
            let tmp = uds_temp_path("fixture");
            let path = tmp.path.to_str().unwrap().to_string();
            let listener = UnixListener::bind(&tmp.path).expect("bind uds");
            tokio::spawn(async move {
                loop {
                    let (mut stream, _) = match listener.accept().await {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    tokio::spawn(async move {
                        // Read the request head (up to the blank line) so the client's
                        // write completes; we only need the request-target path.
                        let mut buf = Vec::new();
                        let mut byte = [0u8; 1];
                        loop {
                            match stream.read(&mut byte).await {
                                Ok(0) => break,
                                Ok(_) => {
                                    buf.push(byte[0]);
                                    if buf.ends_with(b"\r\n\r\n") {
                                        break;
                                    }
                                    if buf.len() > 64 * 1024 {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let head = String::from_utf8_lossy(&buf);
                        let target = head
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();
                        let resp: Vec<u8> = match target.as_str() {
                            // NOTE: no Content-Type here — the TCP hyper fixture's
                            // /text route also omits it, so the surface-parity test
                            // (which prints resp.headers["content-type"]) sees nil on
                            // BOTH paths. /textct below is the with-content-type route.
                            "/text" => b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec(),
                            "/textct" => b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello".to_vec(),
                            "/json" => b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}".to_vec(),
                            "/chunked" => b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n7\r\nchunk1\n\r\n7\r\nchunk2\n\r\n7\r\nchunk3\n\r\n0\r\n\r\n".to_vec(),
                            // /slow accepts the request then never responds (sleeps well
                            // past any test timeout) so the whole-request timeout fires.
                            "/slow" => {
                                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec()
                            }
                            _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\nnope".to_vec(),
                        };
                        let _ = stream.write_all(&resp).await;
                        let _ = stream.shutdown().await;
                    });
                }
            });
            (path, tmp)
        }

        async fn run(src: &str) -> String {
            crate::run_source(src).await.expect("program should run")
        }

        #[tokio::test]
        async fn http_request_over_socket_path_matches_tcp_surface() {
            let (sock, _g) = spawn_uds_http_fixture();
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/json", method: "GET" }})
print(err)
print(resp.status)
let [v, jerr] = await resp.json()
print(jerr)
print(v.ok)
"#
            );
            assert_eq!(run(&src).await, "nil\n200\nnil\ntrue\n");
        }

        #[tokio::test]
        async fn socket_path_text_status_headers_surface() {
            let (sock, _g) = spawn_uds_http_fixture();
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/textct" }})
print(err)
print(resp.status)
print(resp.ok)
print(resp.headers["content-type"])
let [body, berr] = await resp.text()
print(berr)
print(body)
"#
            );
            assert_eq!(run(&src).await, "nil\n200\ntrue\ntext/plain\nnil\nhello\n");
        }

        #[tokio::test]
        async fn socket_path_streaming_body_read_yields_chunks_then_nil() {
            let (sock, _g) = spawn_uds_http_fixture();
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/chunked", stream: true }})
print(err)
print(resp.status)
let body = await resp.body.readToEnd()
print(body)
"#
            );
            assert_eq!(run(&src).await, "nil\n200\nchunk1\nchunk2\nchunk3\n\n");
        }

        #[tokio::test]
        async fn socket_path_typed_json_class_parse() {
            let (sock, _g) = spawn_uds_http_fixture();
            let src = format!(
                r#"
import * as http from "std/net/http"
class Ok {{ ok: bool }}
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/json" }})
print(err)
let [v, jerr] = await resp.json(Ok)
print(jerr)
print(v.ok)
print(v instanceof Ok)
"#
            );
            assert_eq!(run(&src).await, "nil\nnil\ntrue\ntrue\n");
        }

        #[tokio::test]
        async fn verb_helper_get_with_socket_path_extracts_url_path() {
            let (sock, _g) = spawn_uds_http_fixture();
            // The verb helper takes a URL; with socketPath the host is ignored and the
            // URL's PATH is used as the request-target.
            let src = format!(
                r#"
import {{ get }} from "std/net/http"
let [resp, err] = await get("http://docker/json", {{ socketPath: "{sock}" }})
print(err)
print(resp.status)
let [v, _je] = await resp.json()
print(v.ok)
"#
            );
            assert_eq!(run(&src).await, "nil\n200\ntrue\n");
        }

        #[tokio::test]
        async fn socket_path_streaming_request_body_is_clear_tier2() {
            let (sock, _g) = spawn_uds_http_fixture();
            // The UDS client buffers request bodies; a STREAMING request body
            // (`body:{stream:…}`) is unsupported → a clear Tier-2 panic.
            let interp = Interp::new();
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/text", method: "POST", body: {{ stream: "abc" }} }})
print(resp)
"#
            );
            let res = run_on(&interp, &src).await;
            match res {
                Err(crate::interp::Control::Panic(e)) => {
                    assert!(
                        e.message.contains("streaming request body")
                            && e.message.contains("socketPath"),
                        "got: {}",
                        e.message
                    );
                }
                other => panic!("expected a streaming-request-body Tier-2 panic, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn socket_path_connect_failure_is_tier1_pair() {
            // A socketPath that does not exist → a Tier-1 [nil, err] pair, never a panic.
            let missing = uds_temp_path("missing");
            let path = missing.path.to_str().unwrap().to_string();
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{path}", path: "/text" }})
print(resp)
print(err != nil)
"#
            );
            assert_eq!(run(&src).await, "nil\ntrue\n");
        }

        #[tokio::test]
        async fn socket_path_error_on_status_turns_404_into_err() {
            // REVIEW PROBE (CNTR Phase 3): `errorOnStatus:true` must behave IDENTICALLY
            // on the UDS path as on the TCP path — a non-2xx response becomes a Tier-1
            // err. The fixture's unknown route returns 404, so `/nope` + errorOnStatus
            // must yield [nil, err], NOT a successful [resp, nil].
            let (sock, _g) = spawn_uds_http_fixture();
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/nope", errorOnStatus: true }})
print(resp)
print(err != nil)
"#
            );
            assert_eq!(run(&src).await, "nil\ntrue\n");
        }

        #[tokio::test]
        async fn socket_path_error_on_status_passes_2xx_through() {
            // Symmetry: errorOnStatus:true on a 200 must NOT error — the resp passes through.
            let (sock, _g) = spawn_uds_http_fixture();
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/text", errorOnStatus: true }})
print(err)
print(resp.status)
"#
            );
            assert_eq!(run(&src).await, "nil\n200\n");
        }

        #[tokio::test]
        async fn socket_path_total_timeout_fires_on_slow_server() {
            // REVIEW PROBE (CNTR Phase 3, item 6): a UDS server that accepts then never
            // responds (the /slow route sleeps 30s) must surface a Tier-1 timeout err
            // under a small `timeout.total`, NOT hang the request. The whole test is
            // wrapped in a 5s tokio timeout so a real hang is a loud test failure.
            let (sock, _g) = spawn_uds_http_fixture();
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/slow", timeout: {{ total: 50 }} }})
print(resp)
print(err != nil)
"#
            );
            let out = tokio::time::timeout(std::time::Duration::from_secs(5), run(&src))
                .await
                .expect("UDS total-timeout must not hang the request");
            assert_eq!(out, "nil\ntrue\n");
        }

        #[tokio::test]
        async fn socket_path_non_200_surfaces_status_ok_and_body() {
            // REVIEW PROBE (CNTR Phase 3, item 3): a non-200 response (no errorOnStatus)
            // must still produce a normal [resp, nil] pair with `status`/`ok`/body all
            // script-visible — NOT an error, and `ok` must be false.
            let (sock, _g) = spawn_uds_http_fixture();
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/nope" }})
print(err)
print(resp.status)
print(resp.ok)
let [body, berr] = await resp.text()
print(berr)
print(body)
"#
            );
            assert_eq!(run(&src).await, "nil\n404\nfalse\nnil\nnope\n");
        }

        #[tokio::test]
        async fn socket_path_deny_net_scope_is_denied() {
            use crate::stdlib::caps::{CapSet, NetDeny, NetScope};
            let (sock, _g) = spawn_uds_http_fixture();
            // A net carve-out with NO unix allow entry → the UDS request is denied at
            // the §3.1 stage-2 carve-out (`check_unix_path`).
            let interp = Interp::new();
            let mut cs = CapSet::all_granted();
            cs.set_net_scope(NetScope {
                deny: NetDeny::All,
                allow: vec![],
            });
            interp.set_caps(cs);
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/text" }})
print(resp)
"#
            );
            match run_on(&interp, &src).await {
                Err(crate::interp::Control::Panic(e)) => {
                    assert!(
                        e.message.contains("net") && e.message.contains(&sock),
                        "got: {}",
                        e.message
                    );
                }
                other => panic!("expected a net-carve-out denial, got {other:?}"),
            }
        }

        // ---- explicit surface-parity battery: UDS == TCP shape -------------
        //
        // Run the SAME script body against the TCP hyper fixture and the UDS fixture
        // and diff the printed output. The fixtures serve the SAME /text content
        // ("hello", text/plain), so every script-visible field EXCEPT `resp.url`
        // (intentionally `unix://…` vs `http://…` — documented) must match
        // byte-for-byte.
        #[tokio::test]
        async fn surface_parity_text_uds_equals_tcp() {
            let tcp_base = super::fixture::start().await;
            let (sock, _g) = spawn_uds_http_fixture();
            // A script that prints every comparable surface field (NOT url).
            let probe = |req: &str| {
                format!(
                    r#"
import * as http from "std/net/http"
let [resp, err] = await {req}
print(err)
print(resp.status)
print(resp.ok)
print(resp.version)
print(resp.headers["content-type"])
print(type(resp.headers))
let [body, berr] = await resp.text()
print(berr)
print(body)
print(len(body))
"#
                )
            };
            let tcp_src = probe(&format!(r#"http.request({{ url: "{tcp_base}/text" }})"#));
            let uds_src = probe(&format!(
                r#"http.request({{ socketPath: "{sock}", path: "/text" }})"#
            ));
            let tcp_out = run(&tcp_src).await;
            let uds_out = run(&uds_src).await;
            assert_eq!(
                tcp_out, uds_out,
                "UDS surface must match TCP surface (except url); TCP=\n{tcp_out}\nUDS=\n{uds_out}"
            );
            // And the concrete value (status 200, ok true, version HTTP/1.1, no
            // content-type → nil, body hello, len 5).
            assert_eq!(
                uds_out,
                "nil\n200\ntrue\n1.1\nnil\nobject\nnil\nhello\n5\n"
            );
        }

        // Streaming-body surface parity: the same `body.readToEnd()` reader idiom
        // over a chunked response yields the SAME string both over TCP (/stream) and
        // UDS (/chunked) — both emit "chunk1\nchunk2\nchunk3\n".
        #[tokio::test]
        async fn surface_parity_stream_uds_equals_tcp() {
            let tcp_base = super::fixture::start().await;
            let (sock, _g) = spawn_uds_http_fixture();
            let probe = |req: &str| {
                format!(
                    r#"
import * as http from "std/net/http"
let [resp, _e] = await {req}
print(resp.status)
let body = await resp.body.readToEnd()
print(type(body))
print(body)
"#
                )
            };
            let tcp_src = probe(&format!(
                r#"http.request({{ url: "{tcp_base}/stream", stream: true }})"#
            ));
            let uds_src = probe(&format!(
                r#"http.request({{ socketPath: "{sock}", path: "/chunked", stream: true }})"#
            ));
            assert_eq!(run(&tcp_src).await, run(&uds_src).await);
        }

        #[tokio::test]
        async fn socket_path_allow_carveout_admits_request() {
            use crate::stdlib::caps::{CapSet, NetDeny, NetScope};
            let (sock, _g) = spawn_uds_http_fixture();
            let interp = Interp::new();
            let canon = interp.unix_scope_key(&sock);
            let mut cs = CapSet::all_granted();
            cs.set_net_scope(NetScope {
                deny: NetDeny::All,
                allow: vec![canon],
            });
            interp.set_caps(cs);
            let src = format!(
                r#"
import * as http from "std/net/http"
let [resp, err] = await http.request({{ socketPath: "{sock}", path: "/json" }})
print(err)
print(resp.status)
"#
            );
            run_on(&interp, &src).await.expect("exec");
            assert_eq!(interp.output(), "nil\n200\n");
        }
    }
}
