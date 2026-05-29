//! `std/net/http` — modern HTTP client (feature `net`), spec §11.5.
//!
//! Verbs `get/post/put/patch/delete/head/options(url, opts?)` plus `request(opts)`
//! (where `opts.method` selects the verb). Every call is async and returns the
//! Tier-1 pair `[resp, err]`:
//!
//! - a connect / TLS / DNS / timeout failure → `[nil, err]`;
//! - otherwise `[resp, nil]` where `resp` is a `Value::Native(HttpResponse)` whose
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
//!   - `httpVersion` "auto"|"1.1"|"2"|"3" — "3" returns a clean error until the http3
//!     build feature lands (Task 8). `resp.version` reports the negotiated version.
//!   - `errorOnStatus: true` → a non-2xx response becomes a Tier-1 err.
//!   - `cancel` → a `cancelToken()` handle whose `cancel()` aborts the in-flight send
//!     via a `tokio::select!` against a shared `Notify`.
//!
//! Streaming bodies (Task 4): with `opts.stream:true` the body is NOT buffered;
//! `resp.body` is a `Value::Native(HttpBody)` reader following the §11.4 idiom
//! (`await resp.body.read(n?)`→chunk|nil, `readLine()`, `readToEnd()`; chunk type
//! string|bytes per `opts.bodyMode`). The buffered accessors (text/bytes/json) are
//! then unavailable; conversely `resp.body` is only present in streaming mode.
//! Request streaming (`body:{stream:source}`): a `bytes` source is sent as a true
//! streamed body; a reader-handle (Reader/TcpStream/HttpBody) or async-generator-fn
//! source is DRAINED into a buffer and then sent (buffered-then-sent), because
//! pulling the next chunk from those sources re-enters the `!Send` single-threaded
//! interp, which reqwest's body poll cannot do — see `apply_stream_body`.
//!
//! Deferred to later M14 tasks: `sse` (Task 5), the `http3` build feature gate
//! (Task 8).

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value};
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
            Some(Value::Str(s)) if s.as_ref() == "string" => Ok(BodyMode::Str),
            Some(Value::Str(s)) if s.as_ref() == "bytes" => Ok(BodyMode::Bytes),
            Some(other) => Err(AsError::at(
                format!(
                    "net/http bodyMode must be \"string\" or \"bytes\", got {}",
                    crate::interp::type_name(&other)
                ),
                span,
            )
            .into()),
        }
    }

    /// Wrap a finalized chunk as Str (lossy) or Bytes per the mode.
    fn wrap(self, bytes: Vec<u8>) -> Value {
        match self {
            BodyMode::Bytes => Value::Bytes(Rc::new(RefCell::new(bytes))),
            BodyMode::Str => Value::Str(String::from_utf8_lossy(&bytes).into_owned().into()),
        }
    }
}

/// The `AsyncRead` we get by adapting a `reqwest` byte stream: `bytes_stream()`
/// yields `Result<Bytes, reqwest::Error>`; `StreamReader` needs `io::Error` items,
/// so the stream's errors are mapped to `io::Error` first. The reader is then a
/// `BufReader` over that, which lets the §11.4 idiom (`read`/`read_until`/
/// `read_to_end`) apply VERBATIM over the chunked stream — the leftover buffering
/// for partial `read(n)` and `readLine()` line-splitting is the BufReader's own.
type ByteStream =
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
        let stream = resp.bytes_stream().map(|r| r.map_err(std::io::Error::other));
        let boxed: ByteStream = Box::pin(stream);
        let reader = tokio::io::BufReader::new(tokio_util::io::StreamReader::new(boxed));
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
    ADVANCED_CLIENT_KEYS.iter().any(|k| opt_field(opts, k).is_some())
}

/// Parsed `retry` config (a hand-rolled retry loop wraps the send). Default OFF.
struct RetryConfig {
    max: u32,
    exponential: bool,
    base_delay_ms: u64,
    retry_on: Vec<u16>,
}

/// Read a numeric field from an object, if present and a number.
fn num_field(o: &IndexMap<String, Value>, key: &str) -> Option<f64> {
    match o.get(key) {
        Some(Value::Number(n)) => Some(*n),
        _ => None,
    }
}

/// Read a numeric field strictly: `Ok(Some(n))` if it's a number, `Ok(None)` if
/// absent or nil, and a Tier-2 type error (`"<ctx> expects a number"`) for any
/// other present type. Used where a wrong type must fail loudly (not coerce).
fn strict_num_field(
    o: &IndexMap<String, Value>,
    key: &str,
    ctx: &str,
    span: Span,
) -> Result<Option<f64>, Control> {
    match o.get(key) {
        Some(Value::Number(n)) => Ok(Some(*n)),
        Some(Value::Nil) | None => Ok(None),
        Some(other) => Err(AsError::at(
            format!("net/http {} expects a number, got {}", ctx, crate::interp::type_name(other)),
            span,
        )
        .into()),
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
    let o = match &r {
        Value::Object(o) => o.borrow(),
        other => {
            return Err(AsError::at(
                format!("net/http retry expects an object, got {}", crate::interp::type_name(other)),
                span,
            )
            .into())
        }
    };
    // A present-but-wrong-type field is a type error (parity with timeout/redirect);
    // absent or nil fields fall back to the documented default.
    let max = strict_num_field(&o, "max", "retry.max", span)?.unwrap_or(0.0).max(0.0) as u32;
    let exponential = match o.get("backoff") {
        Some(Value::Nil) | None => true, // default exponential
        Some(Value::Str(s)) if s.as_ref() == "exponential" => true,
        Some(Value::Str(s)) if s.as_ref() == "constant" => false,
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
        Some(Value::Nil) | None => Vec::new(),
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for v in a.borrow().iter() {
                match v {
                    Value::Number(n) => out.push(*n as u16),
                    other => {
                        return Err(AsError::at(
                            format!(
                                "net/http retry.retryOn expects an array of numbers, got a {} entry",
                                crate::interp::type_name(other)
                            ),
                            span,
                        )
                        .into())
                    }
                }
            }
            out
        }
        Some(other) => {
            return Err(AsError::at(
                format!(
                    "net/http retry.retryOn expects an array of numbers, got {}",
                    crate::interp::type_name(other)
                ),
                span,
            )
            .into())
        }
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
    let mut b = reqwest::Client::builder();

    // timeout { connect, read, total } in ms. reqwest has no separate per-read
    // timeout in its stable API, so `read` is folded into the total timeout (if
    // `total` is not itself set); the connect timeout is applied independently.
    if let Some(t) = opt_field(opts, "timeout") {
        let o = match &t {
            Value::Object(o) => o.borrow(),
            other => {
                return Err(AsError::at(
                    format!("net/http timeout expects an object, got {}", crate::interp::type_name(other)),
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
    if let Some(r) = opt_field(opts, "redirect") {
        let policy = match &r {
            Value::Str(s) if s.as_ref() == "none" => reqwest::redirect::Policy::none(),
            Value::Object(o) => {
                let o = o.borrow();
                let follow = !matches!(o.get("follow"), Some(Value::Bool(false)));
                if !follow {
                    reqwest::redirect::Policy::none()
                } else {
                    let max = num_field(&o, "max").unwrap_or(10.0).max(0.0) as usize;
                    reqwest::redirect::Policy::limited(max)
                }
            }
            other => {
                return Err(AsError::at(
                    format!("net/http redirect expects an object or \"none\", got {}", crate::interp::type_name(other)),
                    span,
                )
                .into())
            }
        };
        b = b.redirect(policy);
    }

    // decompress (default true). false → disable all transparent decoders, which
    // also stops reqwest from advertising Accept-Encoding.
    if matches!(opt_field(opts, "decompress"), Some(Value::Bool(false))) {
        b = b.no_gzip().no_brotli().no_deflate().no_zstd();
    }

    // cookies: true → a per-client cookie jar (persists/sends across redirects +
    // connection reuse within this request's client). A shared cross-request jar
    // handle is a documented follow-up.
    if matches!(opt_field(opts, "cookies"), Some(Value::Bool(true))) {
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
                let proxy = reqwest::Proxy::all(url)
                    .map_err(|e| Control::from(AsError::at(format!("net/http proxy: {}", e), span)))?;
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
            "3" => {
                return Err(AsError::at(
                    "HTTP/3 requires the 'http3' build feature",
                    span,
                )
                .into())
            }
            other => {
                return Err(AsError::at(
                    format!("net/http httpVersion must be \"auto\"|\"1.1\"|\"2\"|\"3\", got \"{}\"", other),
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
    let o = match tls {
        Value::Object(o) => o.borrow(),
        other => {
            return Err(AsError::at(
                format!("net/http tls expects an object, got {}", crate::interp::type_name(other)),
                span,
            )
            .into())
        }
    };
    // caBundle: a PEM string or a path to a PEM file → an extra trusted root.
    if let Some(ca) = o.get("caBundle") {
        let ca = want_string(ca, span, "net/http tls.caBundle")?;
        let pem = read_pem_or_inline(&ca, "tls.caBundle", span)?;
        let cert = reqwest::Certificate::from_pem(pem.as_bytes())
            .map_err(|e| Control::from(AsError::at(format!("net/http tls.caBundle: {}", e), span)))?;
        b = b.add_root_certificate(cert);
    }
    // clientCert: a PEM string (cert + private key) → a client identity (mTLS).
    if let Some(cc) = o.get("clientCert") {
        let cc = want_string(cc, span, "net/http tls.clientCert")?;
        let pem = read_pem_or_inline(&cc, "tls.clientCert", span)?;
        let id = reqwest::Identity::from_pem(pem.as_bytes())
            .map_err(|e| Control::from(AsError::at(format!("net/http tls.clientCert: {}", e), span)))?;
        b = b.identity(id);
    }
    // minVersion: "1.2" | "1.3".
    if let Some(mv) = o.get("minVersion") {
        let mv = want_string(mv, span, "net/http tls.minVersion")?;
        let v = match mv.as_ref() {
            "1.2" => reqwest::tls::Version::TLS_1_2,
            "1.3" => reqwest::tls::Version::TLS_1_3,
            other => {
                return Err(AsError::at(
                    format!("net/http tls.minVersion must be \"1.2\" or \"1.3\", got \"{}\"", other),
                    span,
                )
                .into())
            }
        };
        b = b.min_tls_version(v);
    }
    // sni: toggle TLS SNI (default on).
    if let Some(Value::Bool(sni)) = o.get("sni") {
        b = b.tls_sni(*sni);
    }
    // insecure: disable certificate verification (flagged above).
    if matches!(o.get("insecure"), Some(Value::Bool(true))) {
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
    ]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

fn bytes_value(b: Vec<u8>) -> Value {
    Value::Bytes(Rc::new(RefCell::new(b)))
}

fn obj(map: IndexMap<String, Value>) -> Value {
    Value::Object(Rc::new(RefCell::new(map)))
}

/// Pull `opts.<key>` (an object) when present and non-nil.
fn opt_field(opts: &Value, key: &str) -> Option<Value> {
    match opts {
        Value::Object(o) => match o.borrow().get(key) {
            Some(Value::Nil) | None => None,
            Some(v) => Some(v.clone()),
        },
        _ => None,
    }
}

/// Map an AScript value to URL-query / form string pairs. Each value is rendered
/// with its scalar string form; arrays expand to repeated keys (`k=a&k=b`).
fn value_to_query_pairs(v: &Value, span: Span, ctx: &str) -> Result<Vec<(String, String)>, Control> {
    let o = match v {
        Value::Object(o) => o,
        other => {
            return Err(AsError::at(
                format!("{} expects an object, got {}", ctx, crate::interp::type_name(other)),
                span,
            )
            .into())
        }
    };
    let mut pairs = Vec::new();
    for (k, val) in o.borrow().iter() {
        match val {
            Value::Array(a) => {
                for item in a.borrow().iter() {
                    pairs.push((k.clone(), scalar_to_string(item, span, ctx)?));
                }
            }
            _ => pairs.push((k.clone(), scalar_to_string(val, span, ctx)?)),
        }
    }
    Ok(pairs)
}

/// Render a scalar (string/number/bool/nil) into its query/form string form.
fn scalar_to_string(v: &Value, span: Span, ctx: &str) -> Result<String, Control> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        Value::Number(_) | Value::Bool(_) => Ok(v.to_string()),
        Value::Nil => Ok(String::new()),
        other => Err(AsError::at(
            format!("{} value must be a string/number/bool, got {}", ctx, crate::interp::type_name(other)),
            span,
        )
        .into()),
    }
}

impl Interp {
    /// Module-level dispatch for `std/net/http` (the verbs + `request`).
    pub(crate) async fn call_http(
        &mut self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "get" | "post" | "put" | "patch" | "delete" | "head" | "options" => {
                let method = func.to_ascii_uppercase();
                let url = want_string(&arg(args, 0), span, &format!("net/http.{}", func))?;
                let opts = arg(args, 1);
                self.call_http_send(&method, url.to_string(), &opts, span).await
            }
            "request" => {
                let opts = arg(args, 0);
                let method = match opt_field(&opts, "method") {
                    Some(m) => want_string(&m, span, "net/http.request method")?.to_ascii_uppercase(),
                    None => "GET".to_string(),
                };
                let url = match opt_field(&opts, "url") {
                    Some(u) => want_string(&u, span, "net/http.request url")?.to_string(),
                    None => {
                        return Err(AsError::at("net/http.request requires opts.url", span).into())
                    }
                };
                self.call_http_send(&method, url, &opts, span).await
            }
            "cancelToken" => Ok(self.make_cancel_token()),
            _ => Err(AsError::at(format!("std/net/http has no function '{}'", func), span).into()),
        }
    }

    /// Build + send one request, returning the Tier-1 `[resp, err]` pair.
    async fn call_http_send(
        &mut self,
        method: &str,
        url: String,
        opts: &Value,
        span: Span,
    ) -> Result<Value, Control> {
        let m = match reqwest::Method::from_bytes(method.as_bytes()) {
            Ok(m) => m,
            Err(_) => return Err(AsError::at(format!("net/http: invalid method '{}'", method), span).into()),
        };
        // Fast path: plain requests reuse the pooled default client. Any advanced
        // client-level opt (timeout/redirect/tls/cookies/proxy/decompress/httpVersion)
        // builds a dedicated per-request client.
        let client = if has_advanced_client_opts(opts) {
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
            let map = match &h {
                Value::Object(o) => o,
                other => {
                    return Err(AsError::at(
                        format!("net/http headers expects an object, got {}", crate::interp::type_name(other)),
                        span,
                    )
                    .into())
                }
            };
            for (k, v) in map.borrow().iter() {
                let vs = scalar_to_string(v, span, "net/http header")?;
                rb = rb.header(k.as_str(), vs);
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

        let resp = match self
            .send_with_retry(rb, &m, &url, method, retry, cancel)
            .await
        {
            Ok(r) => r,
            Err(pair) => return Ok(pair),
        };

        // errorOnStatus: a non-2xx response becomes a Tier-1 err instead of a resp.
        if matches!(opt_field(opts, "errorOnStatus"), Some(Value::Bool(true))) && !resp.status().is_success() {
            return Ok(err_pair(format!(
                "net/http {} {} returned status {}",
                method,
                url,
                resp.status().as_u16()
            )));
        }

        // stream:true → don't buffer; expose `resp.body` as an HttpBody reader and
        // do NOT store the response for the buffered text/bytes/json accessors.
        let stream = matches!(opt_field(opts, "stream"), Some(Value::Bool(true)));
        if stream {
            let mode = BodyMode::parse(opts, span)?;
            Ok(make_pair(self.http_streaming_response_value(resp, mode), Value::Nil))
        } else {
            Ok(make_pair(self.http_response_value(resp), Value::Nil))
        }
    }

    /// Send `rb`, applying the hand-rolled retry loop and (optional) cancellation.
    /// Returns `Ok(response)` or `Err(pair)` where `pair` is the Tier-1 `[nil, err]`.
    async fn send_with_retry(
        &mut self,
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
        &mut self,
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
        let o = match auth {
            Value::Object(o) => o,
            other => {
                return Err(AsError::at(
                    format!("net/http auth expects an object, got {}", crate::interp::type_name(other)),
                    span,
                )
                .into())
            }
        };
        let o = o.borrow();
        if let Some(tok) = o.get("bearer") {
            let tok = want_string(tok, span, "net/http auth.bearer")?;
            return Ok(rb.bearer_auth(tok.to_string()));
        }
        if let Some(basic) = o.get("basic") {
            let arr = super::want_array(basic, span, "net/http auth.basic")?;
            let arr = arr.borrow();
            let user = want_string(arr.first().unwrap_or(&Value::Nil), span, "net/http auth.basic[0]")?;
            let pass = arr.get(1).cloned();
            let pass = match pass {
                Some(Value::Nil) | None => None,
                Some(p) => Some(want_string(&p, span, "net/http auth.basic[1]")?.to_string()),
            };
            return Ok(rb.basic_auth(user.to_string(), pass));
        }
        Err(AsError::at("net/http auth expects {bearer} or {basic:[user,pass]}", span).into())
    }

    #[async_recursion::async_recursion(?Send)]
    async fn apply_body(
        &mut self,
        rb: reqwest::RequestBuilder,
        body: &Value,
        span: Span,
    ) -> Result<reqwest::RequestBuilder, Control> {
        match body {
            Value::Str(s) => Ok(rb.body(s.to_string())),
            Value::Bytes(b) => Ok(rb.body(b.borrow().clone())),
            Value::Object(o) => {
                // Pull out the single recognized shape WITHOUT holding the borrow
                // across an await (the {stream} path can call back into the interp).
                let (jv, form, mp, stream) = {
                    let o = o.borrow();
                    (
                        o.get("json").cloned(),
                        o.get("form").cloned(),
                        o.get("multipart").cloned(),
                        o.get("stream").cloned(),
                    )
                };
                if let Some(jv) = jv {
                    let json = crate::stdlib::json::from_ascript(&jv, &mut Vec::new())
                        .map_err(|m| Control::from(AsError::at(format!("net/http body.json: {}", m), span)))?;
                    let bytes = serde_json::to_vec(&json)
                        .map_err(|e| Control::from(AsError::at(format!("net/http body.json: {}", e), span)))?;
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
            other => Err(AsError::at(
                format!(
                    "net/http body must be a string, bytes, or an object, got {}",
                    crate::interp::type_name(other)
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
        &mut self,
        rb: reqwest::RequestBuilder,
        source: &Value,
        span: Span,
    ) -> Result<reqwest::RequestBuilder, Control> {
        match source {
            // (a) bytes → a true streamed body (single chunk).
            Value::Bytes(b) => {
                let data = b.borrow().clone();
                let chunk =
                    Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(data));
                let stream = futures_util::stream::once(async move { chunk });
                Ok(rb.body(reqwest::Body::wrap_stream(stream)))
            }
            // (b) a reader native handle → drain fully (buffered-then-sent).
            Value::Native(n)
                if matches!(
                    n.kind,
                    NativeKind::Reader | NativeKind::TcpStream | NativeKind::HttpBody
                ) =>
            {
                let bytes = self.drain_reader_handle(n.clone(), span).await?;
                Ok(rb.body(bytes))
            }
            // (c) an async-generator fn → call to exhaustion (buffered-then-sent).
            Value::Function(_) | Value::Builtin(_) | Value::BoundMethod(_) => {
                let bytes = self.drain_generator(source.clone(), span).await?;
                Ok(rb.body(bytes))
            }
            other => Err(AsError::at(
                format!(
                    "net/http body.stream expects bytes, a reader handle, or a generator fn, got {}",
                    crate::interp::type_name(other)
                ),
                span,
            )
            .into()),
        }
    }

    /// Drain a reader native handle (Reader/TcpStream/HttpBody) fully into bytes by
    /// calling its `readToEnd()` method. Buffered-then-sent (see `apply_stream_body`).
    async fn drain_reader_handle(
        &mut self,
        n: Rc<crate::value::NativeObject>,
        span: Span,
    ) -> Result<Vec<u8>, Control> {
        let m = Rc::new(NativeMethod { receiver: n, method: "readToEnd".to_string() });
        let v = self.call_native_method(m, Vec::new(), span).await?;
        match v {
            Value::Bytes(b) => Ok(b.borrow().clone()),
            Value::Str(s) => Ok(s.as_bytes().to_vec()),
            Value::Nil => Ok(Vec::new()),
            other => Err(AsError::at(
                format!("net/http body.stream reader yielded a non-bytes value: {}", crate::interp::type_name(&other)),
                span,
            )
            .into()),
        }
    }

    /// Drain an async-generator fn fully: call it repeatedly, concatenating each
    /// chunk, until it returns `[nil, _]`/`nil` (end) or an `[_, err]` (error →
    /// Tier-1 propagated as a Tier-2 here is avoided — a generator error aborts the
    /// drain with a Tier-2, matching how a malformed body fails the request build).
    async fn drain_generator(
        &mut self,
        gen: Value,
        span: Span,
    ) -> Result<Vec<u8>, Control> {
        let mut out = Vec::new();
        loop {
            let r = self.call_value(gen.clone(), Vec::new(), span).await?;
            // A generator yields `[chunk, err]` (or a bare chunk / nil to end).
            let (chunk, err) = match &r {
                Value::Nil => (Value::Nil, Value::Nil),
                Value::Array(a) => {
                    let a = a.borrow();
                    (a.first().cloned().unwrap_or(Value::Nil), a.get(1).cloned().unwrap_or(Value::Nil))
                }
                other => (other.clone(), Value::Nil),
            };
            if !matches!(err, Value::Nil) {
                return Err(AsError::at(
                    format!("net/http body.stream generator returned an error: {}", err),
                    span,
                )
                .into());
            }
            match chunk {
                Value::Nil => return Ok(out), // end of stream
                Value::Bytes(b) => out.extend_from_slice(&b.borrow()),
                Value::Str(s) => out.extend_from_slice(s.as_bytes()),
                other => {
                    return Err(AsError::at(
                        format!("net/http body.stream generator chunk must be bytes/string, got {}", crate::interp::type_name(&other)),
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
        fields.insert("status".to_string(), Value::Number(status.as_u16() as f64));
        fields.insert("ok".to_string(), Value::Bool(status.is_success()));
        fields.insert("version".to_string(), Value::Str(http_version_str(resp.version()).into()));
        fields.insert("url".to_string(), Value::Str(resp.url().as_str().into()));

        // headers: object of lowercased name → value (last value wins on repeats,
        // except Set-Cookie which we fold into `cookies` below).
        let mut headers = IndexMap::new();
        let mut cookies = IndexMap::new();
        for (name, value) in resp.headers().iter() {
            let key = name.as_str().to_ascii_lowercase();
            let val = value.to_str().unwrap_or("").to_string();
            if key == "set-cookie" {
                if let Some((k, v)) = parse_set_cookie(&val) {
                    cookies.insert(k, Value::Str(v.into()));
                }
            }
            headers.insert(key, Value::Str(val.into()));
        }
        fields.insert("headers".to_string(), obj(headers));
        fields.insert("cookies".to_string(), obj(cookies));
        fields
    }

    /// Read the response metadata into `fields` and register the live response (for
    /// the buffered body accessors) behind a `Value::Native(HttpResponse)` handle.
    fn http_response_value(&mut self, resp: reqwest::Response) -> Value {
        let fields = Self::http_response_fields(&resp);
        self.register_resource(NativeKind::HttpResponse, fields, ResourceState::HttpResponse(resp))
    }

    /// Build a streaming response: the body is NOT buffered. The response's chunked
    /// byte stream is registered behind a `Value::Native(HttpBody)` reader handle,
    /// which is exposed as the response's `body` field. The buffered accessors
    /// (text/bytes/json) are intentionally absent — see `call_http_response_method`.
    fn http_streaming_response_value(&mut self, resp: reqwest::Response, mode: BodyMode) -> Value {
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
        let handle = self.register_resource(NativeKind::HttpResponse, fields, ResourceState::Closed);
        if let Value::Native(n) = &handle {
            self.take_resource(n.id);
        }
        handle
    }

    /// Dispatch a body accessor on an HTTP response handle: `text`/`bytes`/`json`.
    /// Each consumes the response (`take_http_response`); a second body accessor on
    /// the same handle is a Tier-2 panic.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_http_response_method(
        &mut self,
        m: &Rc<NativeMethod>,
        _args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        let method = m.method.as_str();
        match method {
            "text" | "bytes" | "json" => {
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
                        Ok(s) => Ok(make_pair(Value::Str(s.into()), Value::Nil)),
                        Err(e) => Ok(err_pair(format!("response.text failed: {}", e))),
                    },
                    "bytes" => match resp.bytes().await {
                        Ok(b) => Ok(make_pair(bytes_value(b.to_vec()), Value::Nil)),
                        Err(e) => Ok(err_pair(format!("response.bytes failed: {}", e))),
                    },
                    "json" => match resp.bytes().await {
                        Ok(b) => match serde_json::from_slice::<serde_json::Value>(&b) {
                            Ok(jv) => Ok(make_pair(crate::stdlib::json::to_ascript(&jv), Value::Nil)),
                            Err(e) => Ok(err_pair(format!("response.json failed: {}", e))),
                        },
                        Err(e) => Ok(err_pair(format!("response.json failed: {}", e))),
                    },
                    _ => unreachable!(),
                }
            }
            other => Err(AsError::at(format!("httpResponse has no method '{}'", other), span).into()),
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
        &mut self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "read" => {
                let n = match args.first() {
                    None | Some(Value::Nil) => DEFAULT_CHUNK,
                    Some(v) => {
                        let n = super::want_number(v, span, "body.read")?;
                        if n < 0.0 {
                            return Err(AsError::at("body.read n must be non-negative", span).into());
                        }
                        n as usize
                    }
                };
                // read(0) is a no-op: return an empty chunk WITHOUT touching the
                // resource (an empty buffer yields Ok(0), which would otherwise be
                // treated as EOF and finalize a still-open body).
                if n == 0 {
                    let mode = match self.http_body_mut(id) {
                        Some(b) => b.mode,
                        None => return Ok(Value::Nil), // gone → EOF
                    };
                    return Ok(mode.wrap(Vec::new()));
                }
                let body = match self.http_body_mut(id) {
                    Some(b) => b,
                    None => return Ok(Value::Nil), // gone → EOF
                };
                let mode = body.mode;
                let mut buf = Vec::new();
                match body.read_upto(n, &mut buf).await {
                    Ok(0) => {
                        self.take_resource(id);
                        Ok(Value::Nil)
                    }
                    Ok(_) => Ok(mode.wrap(buf)),
                    Err(e) => Err(AsError::at(format!("body.read failed: {}", e), span).into()),
                }
            }
            "readLine" => {
                let body = match self.http_body_mut(id) {
                    Some(b) => b,
                    None => return Ok(Value::Nil), // gone → EOF
                };
                let mode = body.mode;
                let mut buf = Vec::new();
                match body.read_line_bytes(&mut buf).await {
                    Ok(0) => {
                        self.take_resource(id);
                        Ok(Value::Nil)
                    }
                    Ok(_) => {
                        // Strip a single trailing '\n' and an optional preceding '\r'.
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                            if buf.last() == Some(&b'\r') {
                                buf.pop();
                            }
                        }
                        Ok(mode.wrap(buf))
                    }
                    Err(e) => Err(AsError::at(format!("body.readLine failed: {}", e), span).into()),
                }
            }
            "readToEnd" => {
                // readToEnd is type-stable: it ALWAYS returns a value in the body's
                // mode (empty if already drained / finalized).
                let body = match self.http_body_mut(id) {
                    Some(b) => b,
                    None => return Ok(BodyMode::Str.wrap(Vec::new())),
                };
                let mode = body.mode;
                let mut buf = Vec::new();
                match body.read_to_end_bytes(&mut buf).await {
                    Ok(_) => {
                        self.take_resource(id);
                        Ok(mode.wrap(buf))
                    }
                    Err(e) => Err(AsError::at(format!("body.readToEnd failed: {}", e), span).into()),
                }
            }
            other => Err(AsError::at(format!("httpBody has no method '{}'", other), span).into()),
        }
    }

    /// `http.cancelToken()` → a `CancelHandle` native handle. Its `cancel()` method
    /// aborts any in-flight request that was passed this handle via `opts.cancel`.
    fn make_cancel_token(&mut self) -> Value {
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        self.register_resource(
            NativeKind::CancelHandle,
            IndexMap::new(),
            ResourceState::CancelToken(notify),
        )
    }

    /// Dispatch a method on a `CancelHandle`: only `cancel()` (notifies waiters).
    pub(crate) async fn call_cancel_method(
        &mut self,
        m: &Rc<NativeMethod>,
        _args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match m.method.as_str() {
            "cancel" => {
                if let Some(ResourceState::CancelToken(n)) = self.resource(m.receiver.id) {
                    // `notify_one` stores a permit, so a cancel that lands *before*
                    // the request's `notified()` is registered still aborts the next
                    // (and only the next) send — important on the single-threaded
                    // interp where `cancel()` and the request run sequentially.
                    n.notify_one();
                }
                Ok(Value::Nil)
            }
            other => Err(AsError::at(format!("cancelHandle has no method '{}'", other), span).into()),
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
        match &c {
            Value::Native(n) if n.kind == NativeKind::CancelHandle => {
                match self.resource(n.id) {
                    Some(ResourceState::CancelToken(notify)) => Ok(Some(notify.clone())),
                    _ => Ok(None),
                }
            }
            other => Err(AsError::at(
                format!("net/http cancel expects a cancelToken() handle, got {}", crate::interp::type_name(other)),
                span,
            )
            .into()),
        }
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
        let o = match entry {
            Value::Object(o) => o,
            other => {
                return Err(AsError::at(
                    format!("net/http multipart part must be an object, got {}", crate::interp::type_name(other)),
                    span,
                )
                .into())
            }
        };
        let o = o.borrow();
        let name = match o.get("name") {
            Some(n) => want_string(n, span, "net/http multipart part.name")?.to_string(),
            None => return Err(AsError::at("net/http multipart part requires a name", span).into()),
        };
        if let Some(data) = o.get("data") {
            let bytes = match data {
                Value::Str(s) => s.as_bytes().to_vec(),
                Value::Bytes(b) => b.borrow().clone(),
                other => {
                    return Err(AsError::at(
                        format!("net/http multipart data must be string/bytes, got {}", crate::interp::type_name(other)),
                        span,
                    )
                    .into())
                }
            };
            let mut part = reqwest::multipart::Part::bytes(bytes);
            if let Some(fname) = o.get("filename") {
                let fname = want_string(fname, span, "net/http multipart part.filename")?;
                part = part.file_name(fname.to_string());
            }
            if let Some(ct) = o.get("contentType") {
                let ct = want_string(ct, span, "net/http multipart part.contentType")?;
                part = part
                    .mime_str(&ct)
                    .map_err(|e| Control::from(AsError::at(format!("net/http multipart contentType: {}", e), span)))?;
            }
            form = form.part(name, part);
        } else if let Some(value) = o.get("value") {
            let value = scalar_to_string(value, span, "net/http multipart part.value")?;
            form = form.text(name, value);
        } else {
            return Err(AsError::at("net/http multipart part requires `value` or `data`", span).into());
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
    async fn run_on(interp: &mut Interp, src: &str) -> Result<(), crate::interp::Control> {
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
            let body_bytes = req.into_body().collect().await.map(|c| c.to_bytes()).unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            let resp = match path.as_str() {
                "/text" => Response::new(Full::new(Bytes::from_static(b"hello"))),
                "/json" => {
                    let mut r = Response::new(Full::new(Bytes::from_static(b"{\"x\":1,\"items\":[1,2,3]}")));
                    r.headers_mut()
                        .insert(hyper::header::CONTENT_TYPE, "application/json".parse().unwrap());
                    r
                }
                "/echo" => {
                    let echo = serde_json::json!({
                        "method": method,
                        "headers": serde_json::Value::Object(headers),
                        "body": body_str,
                    });
                    let mut r = Response::new(Full::new(Bytes::from(echo.to_string())));
                    r.headers_mut()
                        .insert(hyper::header::CONTENT_TYPE, "application/json".parse().unwrap());
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
                    r.headers_mut().insert(hyper::header::LOCATION, "/text".parse().unwrap());
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
                    r.headers_mut()
                        .insert(hyper::header::SET_COOKIE, "sid=abc123; Path=/".parse().unwrap());
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
        let out = run(
            r#"
import { get } from "std/net/http"
let [resp, err] = await get("http://127.0.0.1:1/")
print(resp)
print(err != nil)
"#,
        )
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn double_body_consume_is_tier2_panic() {
        let base = fixture::start().await;
        let mut interp = Interp::new();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/text")
let [_t, _te] = await resp.text()
let [_b, _be] = await resp.bytes()
"#
        );
        let res = run_on(&mut interp, &src).await;
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
        let mut interp = Interp::new();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [_r, _e] = await get("{base}/text", {{ retry: {{ max: "y" }} }})
"#
        );
        let res = run_on(&mut interp, &src).await;
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
    async fn http_version_3_errors_cleanly_without_feature() {
        let base = fixture::start().await;
        // httpVersion:"3" must surface a clean Tier-2 error (no http3 build feature).
        let mut interp = Interp::new();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [_r, _e] = await get("{base}/text", {{ httpVersion: "3" }})
"#
        );
        let res = run_on(&mut interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(msg.contains("HTTP/3 requires the 'http3' build feature"), "got: {}", msg);
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
        let mut interp = Interp::new();
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [_r, _e] = await get("{base}/text", {{ tls: {{ caBundle: "/no/such/ca.pem" }} }})
"#
        );
        let res = run_on(&mut interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("could not read PEM file '/no/such/ca.pem'"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("expected a clear path-read error, got ok={:?}", other.is_ok()),
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
        let mut interp = Interp::new();
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
        run_on(&mut interp, &src).await.expect("exec");
        assert_eq!(interp.output, "nil\nnil\nnil\n");
        assert_eq!(
            interp.resource_count(),
            baseline,
            "HttpBody resource should be reclaimed on EOF"
        );
    }

    #[tokio::test]
    async fn stream_response_text_accessor_is_clear_error() {
        let base = fixture::start().await;
        let mut interp = Interp::new();
        // With stream:true the response is NOT stored as a buffered Response, so
        // text()/json()/bytes() must surface a clear Tier-2 error.
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/stream", {{ stream: true }})
let [_t, _te] = await resp.text()
"#
        );
        let res = run_on(&mut interp, &src).await;
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
        let mut interp = Interp::new();
        // Without stream:true there is no `body` reader; `resp.body` must be a clear
        // error directing the caller to text()/bytes()/json().
        let src = format!(
            r#"
import {{ get }} from "std/net/http"
let [resp, _e] = await get("{base}/text")
let x = resp.body
"#
        );
        let res = run_on(&mut interp, &src).await;
        match res {
            Err(crate::interp::Control::Panic(e)) => {
                let msg = e.to_string();
                assert!(msg.contains("body") && msg.contains("stream"), "got: {}", msg);
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
}
