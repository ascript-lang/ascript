//! `std/http/server` — a minimal HTTP/1 server whose request handlers are
//! AScript functions (feature `net`, spec §11.2).
//!
//! ## Why hand-rolled HTTP/1 (not hyper's `Service`)
//!
//! Handlers are AScript functions that need `&mut Interp` — the interpreter is
//! single-threaded (`Rc`/`RefCell`, `!Send`) on a current-thread tokio runtime.
//! hyper's `serve_connection` wants a `Service` whose returned future is owned by
//! hyper and cannot borrow `&mut Interp`. Rather than fight that ownership model,
//! the accept loop here parses the HTTP/1 request line + headers + body directly
//! off the `tokio::net::TcpStream`, dispatches through the interpreter, and writes
//! the response bytes back. This keeps `&mut self` available across the whole
//! accept→dispatch→respond cycle.
//!
//! ## Concurrent request handling (M17)
//!
//! Each accepted connection is handled on its **own `spawn_local` task** (built on
//! M17's interior-mutable interpreter: the accept loop captures `self.rc()` and the
//! handler task re-enters the interpreter via that owned `Rc<Interp>`). The accept
//! loop continues to `accept()` the next connection immediately, so a slow handler
//! does NOT block other clients (no head-of-line blocking) — total wall time for a
//! mix of fast/slow requests is ≈ max, not the sum.
//!
//! - **Bounded concurrency.** A `tokio::sync::Semaphore` caps the number of
//!   connections handled at once (default `DEFAULT_MAX_CONCURRENT` = 256,
//!   configurable via the serve opt `maxConcurrent`). The loop acquires an
//!   `OwnedSemaphorePermit` BEFORE spawning each handler task and the task holds it
//!   for its whole lifetime; this applies backpressure and bounds memory/fd usage
//!   under a flood of slow clients.
//! - **Deterministic shutdown.** `maxRequests:N` counts accepted connections; after
//!   N the loop stops accepting and **drains** the in-flight handler tasks (awaits
//!   them) before returning, so an `await serve(...)` (and tests) complete only once
//!   every accepted request's response has been written.
//! - **Per-connection limits preserved.** `requestTimeout` (408), `maxBodySize`
//!   (413), the header limit (431), and handler-panic→500 isolation all apply inside
//!   each spawned task, so one stuck/oversized/panicking connection can't affect the
//!   others or the accept loop. A task panic can't abort the process (handler
//!   panics are converted to 500 before they can escape the task).
//! - **Borrow discipline.** The handler task never holds a `RefCell` borrow across
//!   an `.await`: routes/middleware are cloned out under a short borrow, the listener
//!   is taken out of the resource table up front, and the per-request `HttpNext`
//!   continuations are swept per-dispatch (each dispatch is tagged with a unique id)
//!   so concurrent connections never clobber one another's pending `next` handles.
//!
//! ## Testable lifecycle
//!
//! `listen()` blocks, so the API is split for testability:
//! - `server.bind(host, port) → [boundPort, err]` binds a listener WITHOUT looping
//!   (so tests bind port 0, read the OS-assigned `boundPort`, then drive `serve`).
//! - `server.serve(opts?) → [nil, err]` runs the accept loop. `opts.maxRequests:N`
//!   makes it return after accepting N requests (draining in-flight handlers first,
//!   so an `await serve(...)` completes in tests). `opts.maxConcurrent:N` caps the
//!   number of connections handled at once. With no `maxRequests` it loops until the
//!   listener errors.
//! - `server.listen(host, port, opts?)` is `bind` + `serve` for the common case.
//!
//! ## Request / response shape
//!
//! Handlers receive `{method, path, query, headers, params, body}` (query/headers/
//! params are objects; body is a string). They return either a string (→ 200,
//! text/plain), an object `{status?, headers?, body?}`, or a Result `[value, err]`
//! (non-nil err → 500; else the value is converted as above).
//!
//! ## Middleware
//!
//! `server.use(mw)` registers `(req, next) => resp`. `next` is a callable that
//! advances the chain (the next middleware, or finally the matched route handler).
//! A middleware may short-circuit by returning a response without calling `next`.
//!
//! ## Robustness: panics, request limits, and timeouts
//!
//! - **Handler/middleware panics never kill the server.** A Tier-2 panic
//!   (`Control::Panic`) or a `?`-propagation (`Control::Propagate`) escaping the
//!   handler chain is caught and converted to a **500** response (body = the error
//!   message, for dev-friendliness); the accept loop keeps serving.
//! - **Request size limits** bound memory against hostile clients:
//!   `MAX_HEADER_BYTES` (64 KiB) — exceeding it before the `\r\n\r\n` terminator
//!   yields a **431**; `maxBodySize` (default 16 MiB, configurable via the serve
//!   opt `{maxBodySize}`) — a `Content-Length` over the limit yields a **413** and
//!   the body is NOT read.
//! - **Per-request read timeout** (default 30s, configurable via `{requestTimeout}`
//!   in milliseconds) wraps the whole request read. On expiry the server responds
//!   **408** and continues — so a slowloris client can't hang its connection's task.
//!
//! With concurrent handling these limits bound each connection's task independently
//! (and the `maxConcurrent` semaphore bounds how many run at once), so a hostile
//! client can stall only its own task, never the accept loop or other clients.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Max bytes of request head (request line + headers) before the `\r\n\r\n`
/// terminator. Exceeding it → 431 (headers too large). Bounds memory + slowloris.
const MAX_HEADER_BYTES: usize = 64 * 1024;
/// Default max request body size (overridable via the serve opt `maxBodySize`).
/// A larger declared `Content-Length` → 413 and the body is not read.
const DEFAULT_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Default per-request read timeout in milliseconds (overridable via
/// `requestTimeout`). On expiry the server responds 408 and continues serving.
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
/// Default cap on the number of connections handled concurrently (overridable via
/// the serve opt `maxConcurrent`). The accept loop acquires a permit from a
/// semaphore before spawning each per-connection handler task, bounding memory/fd
/// usage so a flood of slow clients can't spawn unbounded tasks.
const DEFAULT_MAX_CONCURRENT: usize = 256;

/// Why reading a request failed, so the loop can pick the right status code.
enum ReadError {
    /// Header block exceeded `MAX_HEADER_BYTES` before the terminator → 431.
    HeadersTooLarge,
    /// Declared `Content-Length` exceeded the configured limit → 413.
    BodyTooLarge,
    /// Malformed request or a mid-request I/O error → 400.
    BadRequest,
}

/// Routes + middleware + the (optionally) bound listener for one server handle.
pub struct HttpServerState {
    /// `(method_uppercase, path_pattern, handler)`. Path may contain `:name` params.
    pub routes: Vec<(String, String, Value)>,
    /// Middleware `(req, next) => resp`, run in registration order before the route.
    pub middleware: Vec<Value>,
    /// The bound listener, present after `bind`/`listen`. `serve` accepts on it.
    pub listener: Option<TcpListener>,
}

impl HttpServerState {
    fn new() -> Self {
        HttpServerState {
            routes: Vec::new(),
            middleware: Vec::new(),
            listener: None,
        }
    }
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("create", bi("http_server.create"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

fn obj(map: IndexMap<String, Value>) -> Value {
    Value::Object(Rc::new(RefCell::new(map)))
}

/// A parsed HTTP/1 request read off the socket.
struct RawRequest {
    method: String,
    /// The raw request target (path + optional `?query`).
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Read and parse a single HTTP/1 request from `stream`. Returns `Ok(None)` on a
/// clean EOF before any bytes (client closed), or a `ReadError` (→ 4xx) on a
/// limit violation / malformed request. `max_body` caps the body size (413).
async fn read_request(
    stream: &mut TcpStream,
    max_body: usize,
) -> Result<Option<RawRequest>, ReadError> {
    // Read until we have the full header block (terminated by CRLF CRLF), bounding
    // the buffer at MAX_HEADER_BYTES so a client that never sends the terminator
    // can't exhaust memory.
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return Err(ReadError::HeadersTooLarge);
        }
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|_| ReadError::BadRequest)?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None); // clean EOF, no request
            }
            return Err(ReadError::BadRequest); // closed mid-request
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let target = parts.next().unwrap_or("/").to_string();

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }

    // Reject an oversized body up front (by its declared length) WITHOUT reading it.
    if content_length > max_body {
        return Err(ReadError::BodyTooLarge);
    }

    // Body: header block is `header_end..header_end+4` (the CRLFCRLF), body follows.
    let body_start = header_end + 4;
    let mut body = if buf.len() > body_start {
        buf[body_start..].to_vec()
    } else {
        Vec::new()
    };
    while body.len() < content_length {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|_| ReadError::BadRequest)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);

    Ok(Some(RawRequest {
        method,
        target,
        headers,
        body,
    }))
}

/// Find the index of the start of the `\r\n\r\n` header terminator, if present.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Split a raw request target into (path, query-object). `?a=1&b=2` → `{a:"1", b:"2"}`.
fn split_target(target: &str) -> (String, Value) {
    let (path, query_str) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q),
        None => (target.to_string(), ""),
    };
    let mut query = IndexMap::new();
    for pair in query_str.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        query.insert(url_decode(k), Value::Str(url_decode(v).into()));
    }
    (path, obj(query))
}

/// Percent-decode a URL component (`%20`→space, `+`→space).
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Match `path` against a route `pattern` with `:name` params. Returns the captured
/// params on a match, or `None` if the pattern does not match.
fn match_route(pattern: &str, path: &str) -> Option<IndexMap<String, Value>> {
    let pat: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let act: Vec<&str> = path.trim_matches('/').split('/').collect();
    if pat.len() != act.len() {
        return None;
    }
    let mut params = IndexMap::new();
    for (p, a) in pat.iter().zip(act.iter()) {
        if let Some(name) = p.strip_prefix(':') {
            params.insert(name.to_string(), Value::Str(url_decode(a).into()));
        } else if p != a {
            return None;
        }
    }
    Some(params)
}

/// A converted HTTP response ready to serialize.
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Standard reason phrases for the statuses a handler is likely to set.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

/// Build a plain text/plain response with the given status + body (for the
/// server-generated error responses: 400/408/413/431/500).
fn simple_response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
        body: body.as_bytes().to_vec(),
    }
}

/// Convert a handler's return value into an `HttpResponse`.
/// - string → 200 text/plain
/// - object `{status?, headers?, body?}` → as specified (defaults 200, body "")
/// - `[value, err]` → if err non-nil → 500 with the error message; else convert value
fn value_to_response(v: &Value) -> HttpResponse {
    match v {
        Value::Str(s) => HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: s.as_bytes().to_vec(),
        },
        Value::Array(a) => {
            // A Result pair `[value, err]`.
            let a = a.borrow();
            if a.len() == 2 {
                let err = &a[1];
                if !matches!(err, Value::Nil) {
                    let msg = error_message(err);
                    return HttpResponse {
                        status: 500,
                        headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
                        body: msg.into_bytes(),
                    };
                }
                return value_to_response(&a[0]);
            }
            // A non-pair array: serialize via display.
            HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
                body: v.to_string().into_bytes(),
            }
        }
        Value::Object(o) => {
            let o = o.borrow();
            let status = match o.get("status") {
                Some(Value::Number(n)) => *n as u16,
                _ => 200,
            };
            let mut headers: Vec<(String, String)> = Vec::new();
            if let Some(Value::Object(h)) = o.get("headers") {
                for (k, val) in h.borrow().iter() {
                    headers.push((k.clone(), val.to_string()));
                }
            }
            let body = match o.get("body") {
                Some(Value::Str(s)) => s.as_bytes().to_vec(),
                Some(Value::Bytes(b)) => b.borrow().clone(),
                Some(Value::Nil) | None => Vec::new(),
                Some(other) => other.to_string().into_bytes(),
            };
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                && !body.is_empty()
            {
                headers.push(("content-type".into(), "text/plain; charset=utf-8".into()));
            }
            HttpResponse {
                status,
                headers,
                body,
            }
        }
        Value::Nil => HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: Vec::new(),
        },
        other => HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: other.to_string().into_bytes(),
        },
    }
}

/// Recover the error value carried by a `Control::Propagate`. A `?` propagation
/// carries the function's would-be return — usually a `[nil, err]` Result pair, in
/// which case the second element is the error; otherwise the value stands in.
fn propagated_error(v: &Value) -> Value {
    if let Value::Array(a) = v {
        let a = a.borrow();
        if a.len() == 2 {
            return a[1].clone();
        }
    }
    v.clone()
}

/// Pull a human-readable message out of an error value (`{message}` object or string).
fn error_message(err: &Value) -> String {
    match err {
        Value::Object(o) => match o.borrow().get("message") {
            Some(Value::Str(s)) => s.to_string(),
            _ => err.to_string(),
        },
        Value::Str(s) => s.to_string(),
        other => other.to_string(),
    }
}

/// Serialize an `HttpResponse` into HTTP/1.1 wire bytes. The connection is always
/// closed after one request (v1 serves one request per connection), so a
/// `connection: close` header is emitted unless the handler set one explicitly.
fn serialize_response(resp: &HttpResponse) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, reason(resp.status)).into_bytes();
    let mut wrote_cl = false;
    let mut wrote_conn = false;
    for (k, v) in &resp.headers {
        if k.eq_ignore_ascii_case("content-length") {
            wrote_cl = true;
        }
        if k.eq_ignore_ascii_case("connection") {
            wrote_conn = true;
        }
        out.extend_from_slice(format!("{}: {}\r\n", k, v).as_bytes());
    }
    if !wrote_cl {
        out.extend_from_slice(format!("content-length: {}\r\n", resp.body.len()).as_bytes());
    }
    if !wrote_conn {
        out.extend_from_slice(b"connection: close\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&resp.body);
    out
}

impl Interp {
    /// Module-level dispatch for `std/http/server` (`create`).
    pub(crate) async fn call_http_server(
        &self,
        func: &str,
        _args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "create" => {
                let handle = self.register_resource(
                    NativeKind::HttpServer,
                    IndexMap::new(),
                    ResourceState::HttpServer(HttpServerState::new()),
                );
                Ok(handle)
            }
            // Internal terminal "handler" used when no route matched: returns a 404.
            // (Runs after any middleware, so middleware still sees unmatched requests.)
            "__not_found" => Ok(Value::Object(Rc::new(RefCell::new({
                let mut m = IndexMap::new();
                m.insert("status".to_string(), Value::Number(404.0));
                m.insert("body".to_string(), Value::Str("not found".into()));
                m
            })))),
            _ => {
                Err(AsError::at(format!("std/http/server has no function '{}'", func), span).into())
            }
        }
    }

    /// Register a route with a fixed HTTP method string. Shared by `route` and the
    /// seven verb convenience methods (`get`, `post`, `put`, `patch`, `delete`,
    /// `head`, `options`) so each verb is a thin wrapper with no duplicated logic.
    fn register_route(
        &self,
        id: u64,
        server: Value,
        method: String,
        path: String,
        handler: Value,
        span: Span,
    ) -> Result<Value, Control> {
        if !is_callable(&handler) {
            return Err(AsError::at(
                format!("server.{} handler must be a function", method.to_lowercase()),
                span,
            )
            .into());
        }
        match self.http_server_mut(id) {
            Some(mut s) => s.routes.push((method, path, handler)),
            None => {
                return Err(AsError::at("server route: server is closed", span).into());
            }
        }
        Ok(server)
    }

    /// Dispatch a method on an HTTP server handle (`route`/`use`/`bind`/`serve`/`listen`
    /// and the seven verb shortcuts `get`/`post`/`put`/`patch`/`delete`/`head`/`options`).
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_http_server_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        let server = Value::Native(m.receiver.clone());
        match m.method.as_str() {
            "route" => {
                let method =
                    want_string(&arg(&args, 0), span, "server.route method")?.to_uppercase();
                let path = want_string(&arg(&args, 1), span, "server.route path")?.to_string();
                let handler = arg(&args, 2);
                self.register_route(id, server, method, path, handler, span)
            }
            // Verb shortcuts — each is a thin wrapper over register_route.
            "get" | "post" | "put" | "patch" | "delete" | "head" | "options" => {
                let verb = m.method.to_uppercase();
                let path = want_string(&arg(&args, 0), span, &format!("server.{} path", m.method))?.to_string();
                let handler = arg(&args, 1);
                self.register_route(id, server, verb, path, handler, span)
            }
            "use" => {
                let mw = arg(&args, 0);
                if !is_callable(&mw) {
                    return Err(
                        AsError::at("server.use middleware must be a function", span).into(),
                    );
                }
                match self.http_server_mut(id) {
                    Some(mut s) => s.middleware.push(mw),
                    None => return Err(AsError::at("server.use: server is closed", span).into()),
                }
                Ok(server)
            }
            "bind" => {
                let host = want_string(&arg(&args, 0), span, "server.bind host")?;
                let port = super::want_number(&arg(&args, 1), span, "server.bind port")?;
                if !(0.0..=65535.0).contains(&port) || port.fract() != 0.0 {
                    return Err(
                        AsError::at("server.bind port must be an integer 0..=65535", span).into(),
                    );
                }
                let addr = format!("{}:{}", host, port as u16);
                match TcpListener::bind(&addr).await {
                    Ok(listener) => {
                        let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                        match self.http_server_mut(id) {
                            Some(mut s) => s.listener = Some(listener),
                            None => return Ok(err_pair("server.bind: server is closed".into())),
                        }
                        Ok(make_pair(Value::Number(bound as f64), Value::Nil))
                    }
                    Err(e) => Ok(err_pair(format!("server.bind on {} failed: {}", addr, e))),
                }
            }
            "serve" => self.http_server_serve(id, &args, span).await,
            "listen" => {
                // bind + serve convenience.
                let bind_args = vec![arg(&args, 0), arg(&args, 1)];
                let bound = self
                    .call_http_server_method(
                        &Rc::new(NativeMethod {
                            receiver: m.receiver.clone(),
                            method: "bind".into(),
                        }),
                        bind_args,
                        span,
                    )
                    .await?;
                // If bind returned an error pair, propagate it.
                if let Value::Array(a) = &bound {
                    if !matches!(a.borrow().get(1), Some(Value::Nil)) {
                        return Ok(bound);
                    }
                }
                let serve_args = vec![arg(&args, 2)];
                self.http_server_serve(id, &serve_args, span).await
            }
            "close" => {
                self.take_resource(id);
                Ok(Value::Nil)
            }
            other => Err(AsError::at(format!("httpServer has no method '{}'", other), span).into()),
        }
    }

    /// Run the accept loop on the bound listener, handling each connection on its
    /// own `spawn_local` task so a slow handler can't block other clients (no
    /// head-of-line blocking). A bounded semaphore (`maxConcurrent`) caps in-flight
    /// tasks; per-request `requestTimeout`/`maxBodySize`/4xx behavior is preserved
    /// inside each task. With `maxRequests:N` the loop stops after accepting N
    /// connections and DRAINS the in-flight handler tasks before returning, so an
    /// `await serve(...)` (and tests) complete deterministically.
    async fn http_server_serve(
        &self,
        id: u64,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        // Optional serve opts: `maxRequests` (test/shutdown stop), `maxBodySize`
        // (413 limit), `requestTimeout` (ms, 408 on expiry), `maxConcurrent` (cap on
        // concurrently-handled connections).
        let mut max_requests: Option<usize> = None;
        let mut max_body = DEFAULT_MAX_BODY_BYTES;
        let mut timeout_ms = DEFAULT_REQUEST_TIMEOUT_MS;
        let mut max_concurrent = DEFAULT_MAX_CONCURRENT;
        if let Value::Object(o) = arg(args, 0) {
            let o = o.borrow();
            if let Some(Value::Number(n)) = o.get("maxRequests") {
                if *n >= 0.0 {
                    max_requests = Some(*n as usize);
                }
            }
            if let Some(Value::Number(n)) = o.get("maxBodySize") {
                if *n >= 0.0 {
                    max_body = *n as usize;
                }
            }
            if let Some(Value::Number(n)) = o.get("requestTimeout") {
                if *n > 0.0 {
                    timeout_ms = *n as u64;
                }
            }
            if let Some(Value::Number(n)) = o.get("maxConcurrent") {
                if *n >= 1.0 {
                    max_concurrent = *n as usize;
                }
            }
        }

        // Take the listener out of the resource so we own it across awaits (the
        // resource table can't lend `&mut TcpListener` across a `call_value`).
        let listener = match self.http_server_mut(id) {
            Some(mut s) => match s.listener.take() {
                Some(l) => l,
                None => {
                    return Ok(err_pair(
                        "server.serve: not bound (call bind/listen first)".into(),
                    ))
                }
            },
            None => return Ok(err_pair("server.serve: server is closed".into())),
        };

        // Bounds the number of connections handled at once. Each spawned handler
        // task holds an `OwnedSemaphorePermit` for its lifetime; the permit is
        // released (returned to the semaphore) when the task finishes. This caps
        // memory/fd usage even under a flood of slow clients.
        // `Arc` (not `Rc`): `Semaphore::acquire_owned` requires `Arc<Semaphore>` so
        // the resulting `OwnedSemaphorePermit` is `'static` and can move into the
        // spawned handler task. Arc is fine in this `!Send` single-threaded runtime —
        // the permit never crosses a thread (every task stays on the LocalSet).
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        // In-flight handler tasks, retained ONLY when `maxRequests` is set so the
        // shutdown path can DRAIN them (await completion) before returning —
        // otherwise an accepted-but-not-yet-finished slow handler's response could be
        // lost. Without `maxRequests` (an unbounded `serve`) tasks are detached; the
        // semaphore alone bounds concurrency and finished tasks free themselves, so
        // we don't accumulate join handles forever.
        let mut inflight: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        let mut served = 0usize;
        loop {
            if let Some(max) = max_requests {
                if served >= max {
                    break;
                }
            }
            let (stream, _peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(e) => return Ok(err_pair(format!("server.serve accept failed: {}", e))),
            };
            // Acquire a permit BEFORE spawning so we never spawn more than
            // `max_concurrent` handler tasks at once. (Bounded by the semaphore; the
            // accept loop parks here when the cap is reached, applying backpressure.)
            let permit = match sem.clone().acquire_owned().await {
                Ok(p) => p,
                // The semaphore is never closed while we own it, so this is
                // unreachable in practice; bail cleanly if it ever happens.
                Err(_) => break,
            };
            let vm = self.rc();
            // Each connection is handled on its own `'static` task. A panicking
            // handler is already converted to a 500 inside `handle_connection` (and a
            // genuine internal `Control` is swallowed there too) so a task can't
            // abort the process or the accept loop. The permit is moved in and held
            // for the task's whole lifetime, then dropped (released) on completion.
            let handle = tokio::task::spawn_local(async move {
                let _permit = permit;
                vm.handle_connection(id, stream, max_body, timeout_ms, span)
                    .await;
            });
            served += 1;
            if max_requests.is_some() {
                inflight.push(handle);
            }
            // (Unbounded serve: the handle is dropped — the task is detached and runs
            // to completion on the LocalSet; the semaphore bounds concurrency.)
        }

        // Deterministic shutdown: drain every in-flight handler task so all accepted
        // connections have had their responses written before `serve` returns.
        for handle in inflight {
            let _ = handle.await;
        }

        Ok(make_pair(Value::Nil, Value::Nil))
    }

    /// Handle one accepted connection end-to-end on a spawned task: read the request
    /// (bounded by `timeout_ms`/`max_body`), dispatch it through the interpreter
    /// (handler panics/propagation → 500), then write + close. Never panics out of
    /// the task: a genuine internal `Control` escaping dispatch is swallowed (logged
    /// as a 500) so one connection can't take down the accept loop or the process.
    async fn handle_connection(
        &self,
        id: u64,
        mut stream: TcpStream,
        max_body: usize,
        timeout_ms: u64,
        span: Span,
    ) {
        // The whole request read is bounded by `requestTimeout` so a slow/stalled
        // client can't hang this connection's task — on expiry we answer 408.
        let read = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            read_request(&mut stream, max_body),
        )
        .await;
        let resp: Option<HttpResponse> = match read {
            Ok(Ok(Some(req))) => {
                // `dispatch_request` is infallible w.r.t. handler errors (it converts
                // panics/propagation to 500). A genuine internal `Control` should not
                // occur here, but if it does, convert it to a 500 rather than letting
                // the task panic (which would otherwise be silently dropped).
                match self.dispatch_request(id, req, span).await {
                    Ok(r) => Some(r),
                    Err(Control::Panic(e)) => Some(simple_response(500, &e.message)),
                    Err(Control::Propagate(v)) => {
                        Some(simple_response(500, &error_message(&propagated_error(&v))))
                    }
                    // exit() inside a request handler: the connection task cannot
                    // propagate a Control::Exit to the entry-point LocalSet (it runs
                    // in a fire-and-forget spawn_local). Treat it as a 500 here; the
                    // same exit() call will also surface at the program's top level
                    // through run_file/run_source if the server is invoked from script.
                    Err(Control::Exit(code)) => Some(simple_response(
                        500,
                        &format!("exit({code}) called in handler"),
                    )),
                }
            }
            // Clean EOF before any bytes: client closed; nothing to write.
            Ok(Ok(None)) => None,
            Ok(Err(ReadError::HeadersTooLarge)) => {
                Some(simple_response(431, "request header fields too large"))
            }
            Ok(Err(ReadError::BodyTooLarge)) => Some(simple_response(413, "payload too large")),
            Ok(Err(ReadError::BadRequest)) => Some(simple_response(400, "bad request")),
            // Timer elapsed: the read didn't complete in time.
            Err(_) => Some(simple_response(408, "request timeout")),
        };
        if let Some(resp) = resp {
            // One request per connection then close, so the response always
            // advertises `connection: close`.
            let bytes = serialize_response(&resp);
            let _ = stream.write_all(&bytes).await;
            let _ = stream.flush().await;
            // Half-close our side so the client's read terminates promptly.
            let _ = stream.shutdown().await;
        }
    }

    /// Build the request object, run the middleware chain → matched handler, and
    /// convert the result into an `HttpResponse`.
    async fn dispatch_request(
        &self,
        id: u64,
        req: RawRequest,
        span: Span,
    ) -> Result<HttpResponse, Control> {
        let (path, query) = split_target(&req.target);

        // Match a route to extract params (and find the handler).
        let (handler, params) = {
            let routes = match self.http_server_mut(id) {
                Some(s) => s.routes.clone(),
                None => Vec::new(),
            };
            let mut found: Option<(Value, IndexMap<String, Value>)> = None;
            for (rmethod, rpath, rhandler) in &routes {
                if !rmethod.eq_ignore_ascii_case(&req.method) {
                    continue;
                }
                if let Some(params) = match_route(rpath, &path) {
                    found = Some((rhandler.clone(), params));
                    break;
                }
            }
            match found {
                Some((h, p)) => (Some(h), p),
                None => (None, IndexMap::new()),
            }
        };

        // Build the request object passed to handlers/middleware.
        let mut headers_obj = IndexMap::new();
        for (k, v) in &req.headers {
            headers_obj.insert(k.to_ascii_lowercase(), Value::Str(v.clone().into()));
        }
        let mut req_obj = IndexMap::new();
        req_obj.insert("method".to_string(), Value::Str(req.method.clone().into()));
        req_obj.insert("path".to_string(), Value::Str(path.clone().into()));
        req_obj.insert("query".to_string(), query);
        req_obj.insert("headers".to_string(), obj(headers_obj));
        req_obj.insert("params".to_string(), obj(params));
        req_obj.insert(
            "body".to_string(),
            Value::Str(String::from_utf8_lossy(&req.body).into_owned().into()),
        );
        let request = obj(req_obj);

        // The terminal handler: the matched route, or a built-in 404.
        let handler = match handler {
            Some(h) => h,
            None => {
                // No middleware should run for the 404? Spec runs middleware before
                // the matched handler; with no match we still let middleware run so
                // it can e.g. authenticate, then fall through to 404.
                bi("http_server.__not_found")
            }
        };

        let middleware = match self.http_server_mut(id) {
            Some(s) => s.middleware.clone(),
            None => Vec::new(),
        };

        // A fresh id tags every `HttpNext` continuation created for this dispatch so
        // the post-chain sweep only drops THIS request's leftovers — concurrent
        // connections (each on its own task) must not clobber one another's pending
        // continuations.
        let dispatch_id = self.next_http_dispatch_id();
        // An `async` handler/middleware returns a `Value::Future` (eagerly spawned on
        // the LocalSet); settle it before converting to a response so the client sees
        // the resolved body, not the future itself. A plain (sync) handler returns a
        // non-future, so this is the identity for sequential handlers (mirrors how the
        // `await` expression drives a future, spec: `await 5 == 5`). Errors inside the
        // future surface as `Control` and become a 500 below.
        let result = match self
            .run_chain(middleware, 0, handler, request, dispatch_id, span)
            .await
        {
            Ok(Value::Future(f)) => f.get().await,
            other => other,
        };
        // A short-circuiting middleware (one that returns without calling `next`)
        // leaves its un-consumed `HttpNext` continuation in the resource table;
        // sweep this dispatch's leftovers so per-request handles don't accumulate.
        self.drop_pending_http_next(dispatch_id);
        // A handler/middleware panic (`Control::Panic`) or `?`-propagation
        // (`Control::Propagate`) must NOT kill the server: convert it to a 500 so
        // the accept loop keeps serving. The message is included for dev-friendliness.
        // (Modeled on how the `recover` builtin catches `Control::Panic`.)
        let resp = match result {
            Ok(v) => value_to_response(&v),
            Err(Control::Panic(e)) => simple_response(500, &e.message),
            Err(Control::Propagate(v)) => {
                // An escaped `?` carries the err pair's value; surface its message.
                simple_response(500, &error_message(&propagated_error(&v)))
            }
            // exit() inside a handler: re-propagate so the server task unwinds.
            Err(Control::Exit(code)) => return Err(Control::Exit(code)),
        };
        Ok(resp)
    }

    /// Run middleware `[index..]` then the terminal `handler`. Each middleware is
    /// called as `mw(req, next)` where `next` is a callable that advances the chain.
    /// A middleware that returns without calling `next` short-circuits the chain.
    /// Returns the response value.
    #[async_recursion::async_recursion(?Send)]
    async fn run_chain(
        &self,
        middleware: Vec<Value>,
        index: usize,
        handler: Value,
        request: Value,
        dispatch_id: u64,
        span: Span,
    ) -> Result<Value, Control> {
        if index >= middleware.len() {
            // Terminal: the matched route handler (or the 404 builtin).
            return self.call_value(handler, vec![request], span).await;
        }
        let mw = middleware[index].clone();
        // `next` carries the continuation (resume at index+1) in an HttpNext
        // resource; the middleware invokes it as `next(req?)` to advance the chain.
        let next_state = NextState {
            middleware: middleware.clone(),
            index: index + 1,
            handler,
            request: request.clone(),
            dispatch_id,
        };
        let next_handle = self.register_resource(
            NativeKind::HttpNext,
            IndexMap::new(),
            ResourceState::HttpNext(Box::new(next_state)),
        );
        let next = match &next_handle {
            Value::Native(n) => Value::NativeMethod(Rc::new(NativeMethod {
                receiver: n.clone(),
                method: "call".into(),
            })),
            _ => unreachable!("register_resource returns a Native handle"),
        };
        self.call_value(mw, vec![request, next], span).await
    }

    /// Dispatch a call to a `next` callable (an `HttpNext` handle). Resumes the
    /// middleware chain at the saved index. An optional argument lets the middleware
    /// pass a (possibly replaced) request object onward (`next(req)`); with no
    /// argument the original request is forwarded.
    pub(crate) async fn call_http_next(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        // Take the continuation out (a `next` is single-use, like Express's).
        let state = match self.take_resource(id) {
            Some(ResourceState::HttpNext(s)) => *s,
            _ => {
                return Err(AsError::at("next() called twice or on an invalid handle", span).into())
            }
        };
        let request = match args.first() {
            Some(v) if !matches!(v, Value::Nil) => v.clone(),
            _ => state.request,
        };
        self.run_chain(
            state.middleware,
            state.index,
            state.handler,
            request,
            state.dispatch_id,
            span,
        )
        .await
    }
}

/// The continuation state behind a `next` callable: the remaining middleware
/// chain, the index to resume at, the terminal route handler, and the request.
pub struct NextState {
    middleware: Vec<Value>,
    index: usize,
    handler: Value,
    request: Value,
    /// Identifies the owning `dispatch_request` so a short-circuit sweep only
    /// drops THIS dispatch's leftover continuations (concurrent connections each
    /// have their own dispatch id — see `drop_pending_http_next`).
    pub dispatch_id: u64,
}

/// Is `v` something `call_value` can invoke?
fn is_callable(v: &Value) -> bool {
    matches!(
        v,
        Value::Function(_)
            | Value::Builtin(_)
            | Value::Class(_)
            | Value::BoundMethod(_)
            | Value::NativeMethod(_)
    )
}

#[cfg(test)]
mod tests {
    use crate::interp::Interp;
    use std::rc::Rc;

    /// Build a fresh interpreter as an `Rc` with its self-reference installed (so
    /// `serve`'s per-connection `self.rc()` / `spawn_local` works — see M17).
    fn new_interp() -> Rc<Interp> {
        let interp = Rc::new(Interp::new());
        interp.install_self();
        interp
    }

    /// Run an AScript program on a caller-held interp (so we can drive `serve` and
    /// inspect output) INSIDE a `LocalSet`, the shape `run_file`/`run_source` use:
    /// the server's per-connection handler tasks are `spawn_local`'d, which requires
    /// an active `LocalSet`; we `run_until` the program then drain remaining tasks.
    async fn run_on(interp: &Rc<Interp>, src: &str) -> Result<(), String> {
        let tokens = crate::lexer::lex(src).map_err(|e| e.message)?;
        let program = crate::parser::parse(&tokens).map_err(|e| e.message)?;
        let env = crate::interp::global_env().child();
        let local = tokio::task::LocalSet::new();
        let r = local
            .run_until(async { interp.exec(&program, &env).await })
            .await
            .map_err(|c| format!("{:?}", c))
            .map(|_| ());
        // Drain any still-running spawned tasks (handler tasks for unbounded serve;
        // for tests `serve` already drained its in-flight tasks before returning).
        local.await;
        r
    }

    /// Reserve an ephemeral port (bind+drop) so the AScript server can bind it and
    /// a raw client can retry-connect until it's up.
    async fn reserve_port() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    }

    /// Run an AScript server program inline on the test runtime (the interp is
    /// `!Send` so it can't be spawned), while `client` runs in a spawned task and
    /// hits the server. The server `src` should `serve` with a `maxRequests` so it
    /// returns once the client's request(s) are handled.
    async fn with_server<F, Fut, T>(src: &str, client: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let client_task = tokio::spawn(client());
        let interp = new_interp();
        run_on(&interp, src)
            .await
            .unwrap_or_else(|e| panic!("server: {e}"));
        client_task.await.unwrap()
    }

    #[tokio::test]
    async fn get_route_returns_body() {
        let port = reserve_port().await;
        let base = format!("http://127.0.0.1:{port}");
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("GET", "/hello", (req) => "world")
let [bound, berr] = await s.bind("127.0.0.1", {port})
print(bound == {port})
print(berr)
await s.serve({{ maxRequests: 1 }})
"#
        );
        let _ = base;
        let url = format!("http://127.0.0.1:{port}/hello");
        let (status, body) = with_server(&src, move || async move {
            client_request("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "world");
    }

    #[tokio::test]
    async fn path_param_is_extracted() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("GET", "/users/:id", (req) => "user " + req.params.id)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/users/42");
        let (status, body) = with_server(&src, move || async move {
            client_request("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "user 42");
    }

    #[tokio::test]
    async fn post_body_is_echoed() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("POST", "/echo", (req) => "got:" + req.body)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/echo");
        let (status, body) = with_server(&src, move || async move {
            client_request("POST", &url, Some("hello body".to_string())).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "got:hello body");
    }

    #[tokio::test]
    async fn object_response_sets_status_headers_body() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("POST", "/make", (req) => ({{ status: 201, headers: {{ "X-Made": "yes" }}, body: "created" }}))
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/make");
        // Read the full raw response (head + body) so we can assert on the header.
        let (status, raw) = with_server(&src, move || async move {
            client_request_raw("POST", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 201 Created");
        assert!(
            raw.to_lowercase().contains("x-made: yes"),
            "missing header in:\n{raw}"
        );
        assert!(raw.ends_with("created"), "body wrong in:\n{raw}");
    }

    #[tokio::test]
    async fn unmatched_route_is_404() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("GET", "/known", (req) => "ok")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/unknown");
        let (status, body) = with_server(&src, move || async move {
            client_request("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 404 Not Found");
        assert_eq!(body, "not found");
    }

    #[tokio::test]
    async fn middleware_short_circuits_with_401() {
        let port = reserve_port().await;
        // Middleware returns a 401 WITHOUT calling next → the route never runs.
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.use((req, next) => ({{ status: 401, body: "denied" }}))
s.route("GET", "/secret", (req) => "treasure")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/secret");
        let (status, body) = with_server(&src, move || async move {
            client_request("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 401 Unauthorized");
        assert_eq!(body, "denied");
    }

    #[tokio::test]
    async fn middleware_calls_next_and_handler_runs() {
        let port = reserve_port().await;
        // Middleware calls next() → the matched handler runs and its response flows back.
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.use((req, next) => {{
  let resp = next(req)
  return resp
}})
s.route("GET", "/ok", (req) => "handled")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/ok");
        let (status, body) = with_server(&src, move || async move {
            client_request("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "handled");
    }

    #[tokio::test]
    async fn middleware_adds_header_to_response() {
        let port = reserve_port().await;
        // Middleware calls next(), then augments the response with an extra header.
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.use((req, next) => {{
  let resp = next(req)
  return ({{ status: 200, headers: {{ "X-Powered-By": "ascript" }}, body: resp }})
}})
s.route("GET", "/page", (req) => "hi")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/page");
        let (status, raw) = with_server(&src, move || async move {
            client_request_raw("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert!(
            raw.to_lowercase().contains("x-powered-by: ascript"),
            "missing header:\n{raw}"
        );
        assert!(raw.ends_with("hi"), "body wrong:\n{raw}");
    }

    #[tokio::test]
    async fn query_params_are_parsed() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("GET", "/search", (req) => req.query.q + "/" + req.query.page)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/search?q=cats&page=2");
        let (status, body) = with_server(&src, move || async move {
            client_request("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "cats/2");
    }

    #[tokio::test]
    async fn ascript_http_client_hits_ascript_server() {
        // End-to-end through BOTH the std/net/http client (Tasks 2-3) AND the
        // std/http/server: the AScript server runs inline; a SECOND AScript program
        // (the client) runs on its own current-thread runtime in a spawned OS thread
        // (the interp is `!Send`, so it can't share this runtime/task).
        let port = reserve_port().await;
        let server_src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("GET", "/api", (req) => "from-server")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let client_src = format!(
            r#"
import {{ get }} from "std/net/http"
fn attempt() {{
  let [resp, err] = await get("http://127.0.0.1:{port}/api")
  if (err != nil) {{ return nil }}
  let [body, _be] = await resp.text()
  return body
}}
let out = nil
while (out == nil) {{
  out = await attempt()
}}
print(out)
"#
        );
        let client = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let interp = new_interp();
                run_on(&interp, &client_src).await.expect("client ran");
                interp.output()
            })
        });
        let interp = new_interp();
        run_on(&interp, &server_src).await.expect("server ran");
        let client_out = client.join().unwrap();
        assert_eq!(client_out, "from-server\n");
    }

    #[tokio::test]
    async fn short_circuit_middleware_leaves_no_leaked_handles() {
        // After serving a request whose middleware short-circuits (never calls next),
        // the un-consumed HttpNext continuation must be swept (no per-request leak).
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.use((req, next) => ({{ status: 401, body: "no" }}))
s.route("GET", "/x", (req) => "yes")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/x");
        let client = tokio::spawn(async move { client_request("GET", &url, None).await });
        let interp = new_interp();
        let baseline = interp.resource_count();
        run_on(&interp, &src).await.expect("server ran");
        client.await.unwrap();
        // The server handle itself was closed implicitly? No — `create()`'s handle
        // outlives the program, but the transient next-continuation must be gone.
        // Resource count returns to (baseline + 1 server handle), with NO next handle.
        assert_eq!(
            interp.resource_count(),
            baseline + 1,
            "only the server handle should remain"
        );
    }

    #[tokio::test]
    async fn handler_panic_becomes_500_and_loop_survives() {
        // A handler that panics (`nil.field`, a Tier-2 panic) must NOT kill the
        // server: the client gets a 500, AND a SECOND request afterward still works
        // (proves the accept loop survived the panic). Server serves 2 requests.
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("GET", "/boom", (req) => nil.field)
s.route("GET", "/ok", (req) => "alive")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 2 }})
"#
        );
        let boom_url = format!("http://127.0.0.1:{port}/boom");
        let ok_url = format!("http://127.0.0.1:{port}/ok");
        // One client task issues both requests sequentially.
        let results = with_server(&src, move || async move {
            let first = client_request("GET", &boom_url, None).await;
            let second = client_request("GET", &ok_url, None).await;
            (first, second)
        })
        .await;
        let ((boom_status, _boom_body), (ok_status, ok_body)) = results;
        assert_eq!(
            boom_status, "HTTP/1.1 500 Internal Server Error",
            "panic must yield 500"
        );
        assert_eq!(
            ok_status, "HTTP/1.1 200 OK",
            "server must survive the panic"
        );
        assert_eq!(ok_body, "alive");
    }

    #[tokio::test]
    async fn oversized_body_is_413_and_server_survives() {
        // A request declaring a Content-Length over `maxBodySize` → 413 WITHOUT the
        // body being read, and the server keeps serving (a follow-up request works).
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("POST", "/upload", (req) => "stored")
s.route("GET", "/ping", (req) => "pong")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 2, maxBodySize: 10 }})
"#
        );
        let big = "x".repeat(100); // Content-Length: 100 > maxBodySize 10
        let upload_url = format!("http://127.0.0.1:{port}/upload");
        let ping_url = format!("http://127.0.0.1:{port}/ping");
        let results = with_server(&src, move || async move {
            let first = client_request("POST", &upload_url, Some(big)).await;
            let second = client_request("GET", &ping_url, None).await;
            (first, second)
        })
        .await;
        let ((up_status, _up_body), (ping_status, ping_body)) = results;
        assert_eq!(
            up_status, "HTTP/1.1 413 Payload Too Large",
            "oversized body must be 413"
        );
        assert_eq!(
            ping_status, "HTTP/1.1 200 OK",
            "server must survive the rejected body"
        );
        assert_eq!(ping_body, "pong");
    }

    #[tokio::test]
    async fn slow_handler_does_not_block_fast_handler() {
        // THE concurrency proof: a SLOW route (sleeps 400ms) and a FAST route. Two
        // clients hit them *at the same time*. If handling is concurrent, the fast
        // response returns long before the slow one (their handling overlaps), so
        // total wall time ≈ max(slow, fast) ≈ 400ms, NOT the sum (~800ms). Under the
        // old sequential server, the slow handler (accepted first) would block the
        // fast one and this would take ~800ms / the fast one would stall.
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import {{ sleep }} from "std/time"
let s = create()
s.route("GET", "/slow", async (req) => {{ await sleep(400); return "slow" }})
s.route("GET", "/fast", (req) => "fast")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 2 }})
"#
        );
        let slow_url = format!("http://127.0.0.1:{port}/slow");
        let fast_url = format!("http://127.0.0.1:{port}/fast");
        let (slow_elapsed_ms, fast_elapsed_ms, slow_body, fast_body) =
            with_server(&src, move || async move {
                let start = std::time::Instant::now();
                // Fire the slow request first so it is (likely) accepted first; then the
                // fast one. Concurrency means the fast one still returns quickly.
                let slow = tokio::spawn({
                    let url = slow_url.clone();
                    async move {
                        let (_s, b) = client_request("GET", &url, None).await;
                        (start.elapsed().as_millis(), b)
                    }
                });
                // Tiny stagger so /slow is accepted first, exposing head-of-line blocking
                // if the server were sequential.
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                let fast = tokio::spawn({
                    let url = fast_url.clone();
                    async move {
                        let (_s, b) = client_request("GET", &url, None).await;
                        (start.elapsed().as_millis(), b)
                    }
                });
                let (slow_ms, slow_b) = slow.await.unwrap();
                let (fast_ms, fast_b) = fast.await.unwrap();
                (slow_ms, fast_ms, slow_b, fast_b)
            })
            .await;
        assert_eq!(slow_body, "slow");
        assert_eq!(fast_body, "fast");
        // The fast response must come back well before the slow handler finishes
        // (which can't be earlier than ~400ms). Lenient bound to avoid CI flakiness:
        // if handling were sequential the fast one would wait behind the slow one
        // (~400ms+); concurrent handling returns it in tens of ms.
        assert!(
            fast_elapsed_ms < 300,
            "fast response should overlap the slow handler (got {fast_elapsed_ms}ms; slow took {slow_elapsed_ms}ms)"
        );
        // Sanity: the slow handler really did take ~400ms (it slept).
        assert!(
            slow_elapsed_ms >= 350,
            "slow handler should have slept ~400ms (got {slow_elapsed_ms}ms)"
        );
    }

    #[tokio::test]
    async fn max_requests_drains_inflight_slow_handler() {
        // maxRequests-based shutdown must DRAIN in-flight handler tasks: a slow
        // handler accepted as the Nth request must still complete and deliver its
        // response before `serve` returns. Here maxRequests:1 with a single slow
        // request — `serve` must not return until the slow handler's response is
        // written, or the client would see a truncated/empty body.
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import {{ sleep }} from "std/time"
let s = create()
s.route("GET", "/slow", async (req) => {{ await sleep(200); return "drained-ok" }})
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/slow");
        let (status, body) = with_server(&src, move || async move {
            client_request("GET", &url, None).await
        })
        .await;
        assert_eq!(
            status, "HTTP/1.1 200 OK",
            "in-flight slow handler must be drained before serve returns"
        );
        assert_eq!(body, "drained-ok");
    }

    #[tokio::test]
    async fn many_concurrent_requests_all_succeed_under_cap() {
        // Stress: more concurrent clients than a small `maxConcurrent` cap. The
        // bounded semaphore must serialize admission WITHOUT dropping anyone — every
        // request still gets a correct response (the cap throttles, never fails).
        let port = reserve_port().await;
        let n = 8usize;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import {{ sleep }} from "std/time"
let s = create()
s.route("GET", "/work", async (req) => {{ await sleep(20); return "done" }})
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: {n}, maxConcurrent: 2 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/work");
        let bodies = with_server(&src, move || async move {
            let mut tasks = Vec::new();
            for _ in 0..n {
                let u = url.clone();
                tasks.push(tokio::spawn(async move {
                    client_request("GET", &u, None).await
                }));
            }
            let mut out = Vec::new();
            for t in tasks {
                out.push(t.await.unwrap());
            }
            out
        })
        .await;
        assert_eq!(bodies.len(), n);
        for (status, body) in &bodies {
            assert_eq!(
                status, "HTTP/1.1 200 OK",
                "every request must succeed under the cap"
            );
            assert_eq!(body, "done");
        }
    }

    // ── Verb-method tests (sub-phase 7a) ──────────────────────────────────────

    /// `s.get(path, handler)` is equivalent to `s.route("GET", path, handler)`.
    #[tokio::test]
    async fn verb_get_method_dispatches_correctly() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.get("/hello", (req) => "verb-world")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/hello");
        let (status, body) = with_server(&src, move || async move {
            client_request("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "verb-world");
    }

    /// `s.post(path, handler)` is equivalent to `s.route("POST", path, handler)`.
    #[tokio::test]
    async fn verb_post_method_echoes_body() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.post("/echo", (req) => "got:" + req.body)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/echo");
        let (status, body) = with_server(&src, move || async move {
            client_request("POST", &url, Some("hello".to_string())).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "got:hello");
    }

    /// `s.delete(path, handler)` is reachable from AScript (delete is not a keyword).
    #[tokio::test]
    async fn verb_delete_method_dispatches_correctly() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.delete("/item", (req) => "deleted")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/item");
        let (status, body) = with_server(&src, move || async move {
            client_request("DELETE", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "deleted");
    }

    /// Remaining verbs: put, patch, head, options — each thin-wraps route.
    #[tokio::test]
    async fn verb_put_patch_head_options_dispatch_correctly() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.put("/item", (req) => "put-ok")
s.patch("/item", (req) => "patch-ok")
s.head("/ping", (req) => "head-ok")
s.options("/ping", (req) => "options-ok")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 4 }})
"#
        );
        let put_url = format!("http://127.0.0.1:{port}/item");
        let patch_url = format!("http://127.0.0.1:{port}/item");
        let head_url = format!("http://127.0.0.1:{port}/ping");
        let options_url = format!("http://127.0.0.1:{port}/ping");
        let results = with_server(&src, move || async move {
            let r1 = client_request("PUT", &put_url, None).await;
            let r2 = client_request("PATCH", &patch_url, None).await;
            let r3 = client_request("HEAD", &head_url, None).await;
            let r4 = client_request("OPTIONS", &options_url, None).await;
            (r1, r2, r3, r4)
        })
        .await;
        let ((s1, b1), (s2, b2), (s3, b3), (s4, b4)) = results;
        assert_eq!((s1.as_str(), b1.as_str()), ("HTTP/1.1 200 OK", "put-ok"));
        assert_eq!((s2.as_str(), b2.as_str()), ("HTTP/1.1 200 OK", "patch-ok"));
        assert_eq!((s3.as_str(), b3.as_str()), ("HTTP/1.1 200 OK", "head-ok"));
        assert_eq!((s4.as_str(), b4.as_str()), ("HTTP/1.1 200 OK", "options-ok"));
    }

    // ── End verb-method tests ──────────────────────────────────────────────────

    /// Like `client_request` but returns the FULL raw response text (head + body)
    /// so tests can assert on headers.
    async fn client_request_raw(method: &str, url: &str, body: Option<String>) -> (String, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rest = url.strip_prefix("http://").unwrap();
        let (hostport, path) = rest
            .split_once('/')
            .map(|(h, p)| (h, format!("/{p}")))
            .unwrap();
        let mut req =
            format!("{method} {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n");
        if let Some(b) = &body {
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        req.push_str("\r\n");
        if let Some(b) = &body {
            req.push_str(b);
        }
        loop {
            match tokio::net::TcpStream::connect(hostport).await {
                Ok(mut s) => {
                    s.write_all(req.as_bytes()).await.unwrap();
                    s.flush().await.unwrap();
                    let mut resp = Vec::new();
                    s.read_to_end(&mut resp).await.unwrap();
                    let text = String::from_utf8_lossy(&resp).into_owned();
                    let status = text.lines().next().unwrap_or("").to_string();
                    return (status, text);
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            }
        }
    }

    /// Issue a request; returns (status_line, body). Owned args so the future is
    /// `'static` + `Send` (it runs in a spawned task).
    async fn client_request(method: &str, url: &str, body: Option<String>) -> (String, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rest = url.strip_prefix("http://").unwrap();
        let (hostport, path) = rest
            .split_once('/')
            .map(|(h, p)| (h, format!("/{p}")))
            .unwrap();
        let mut req =
            format!("{method} {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n");
        if let Some(b) = &body {
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        req.push_str("\r\n");
        if let Some(b) = &body {
            req.push_str(b);
        }
        loop {
            match tokio::net::TcpStream::connect(hostport).await {
                Ok(mut s) => {
                    s.write_all(req.as_bytes()).await.unwrap();
                    s.flush().await.unwrap();
                    let mut resp = Vec::new();
                    s.read_to_end(&mut resp).await.unwrap();
                    let text = String::from_utf8_lossy(&resp).into_owned();
                    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
                    let status = head.lines().next().unwrap_or("").to_string();
                    return (status, body.to_string());
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            }
        }
    }
}
