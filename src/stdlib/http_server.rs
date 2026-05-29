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
//! ## Sequential request handling (v1 limitation)
//!
//! Requests are handled **strictly sequentially**: the loop accepts a connection,
//! serves exactly one request on it, awaits the handler fully, writes the
//! response, then loops. Concurrent connection handling is a documented v1
//! limitation (deferred per the M14 plan) — it is the correct behaviour under a
//! single `&mut Interp`.
//!
//! ## Testable lifecycle
//!
//! `listen()` blocks, so the API is split for testability:
//! - `server.bind(host, port) → [boundPort, err]` binds a listener WITHOUT looping
//!   (so tests bind port 0, read the OS-assigned `boundPort`, then drive `serve`).
//! - `server.serve(opts?) → [nil, err]` runs the accept loop. `opts.maxRequests:N`
//!   makes it return after serving N requests (so an `await serve(...)` completes
//!   in tests). With no `maxRequests` it loops until the listener errors.
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
        HttpServerState { routes: Vec::new(), middleware: Vec::new(), listener: None }
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
/// clean EOF before any bytes (client closed), `Err` on a malformed request.
async fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<RawRequest>> {
    // Read until we have the full header block (terminated by CRLF CRLF).
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None); // clean EOF, no request
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed mid-request",
            ));
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

    // Body: header block is `header_end..header_end+4` (the CRLFCRLF), body follows.
    let body_start = header_end + 4;
    let mut body = if buf.len() > body_start { buf[body_start..].to_vec() } else { Vec::new() };
    while body.len() < content_length {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);

    Ok(Some(RawRequest { method, target, headers, body }))
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
        500 => "Internal Server Error",
        _ => "OK",
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
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) && !body.is_empty() {
                headers.push(("content-type".into(), "text/plain; charset=utf-8".into()));
            }
            HttpResponse { status, headers, body }
        }
        Value::Nil => HttpResponse { status: 200, headers: Vec::new(), body: Vec::new() },
        other => HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: other.to_string().into_bytes(),
        },
    }
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
        &mut self,
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
            _ => Err(AsError::at(format!("std/http/server has no function '{}'", func), span).into()),
        }
    }

    /// Dispatch a method on an HTTP server handle (`route`/`use`/`bind`/`serve`/`listen`).
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_http_server_method(
        &mut self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        let server = Value::Native(m.receiver.clone());
        match m.method.as_str() {
            "route" => {
                let method = want_string(&arg(&args, 0), span, "server.route method")?.to_uppercase();
                let path = want_string(&arg(&args, 1), span, "server.route path")?.to_string();
                let handler = arg(&args, 2);
                if !is_callable(&handler) {
                    return Err(AsError::at("server.route handler must be a function", span).into());
                }
                match self.http_server_mut(id) {
                    Some(s) => s.routes.push((method, path, handler)),
                    None => return Err(AsError::at("server.route: server is closed", span).into()),
                }
                Ok(server)
            }
            "use" => {
                let mw = arg(&args, 0);
                if !is_callable(&mw) {
                    return Err(AsError::at("server.use middleware must be a function", span).into());
                }
                match self.http_server_mut(id) {
                    Some(s) => s.middleware.push(mw),
                    None => return Err(AsError::at("server.use: server is closed", span).into()),
                }
                Ok(server)
            }
            "bind" => {
                let host = want_string(&arg(&args, 0), span, "server.bind host")?;
                let port = super::want_number(&arg(&args, 1), span, "server.bind port")?;
                if !(0.0..=65535.0).contains(&port) || port.fract() != 0.0 {
                    return Err(AsError::at("server.bind port must be an integer 0..=65535", span).into());
                }
                let addr = format!("{}:{}", host, port as u16);
                match TcpListener::bind(&addr).await {
                    Ok(listener) => {
                        let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                        match self.http_server_mut(id) {
                            Some(s) => s.listener = Some(listener),
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
                let bound = self.call_http_server_method(
                    &Rc::new(NativeMethod { receiver: m.receiver.clone(), method: "bind".into() }),
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

    /// Run the accept loop on the bound listener, handling requests sequentially.
    async fn http_server_serve(
        &mut self,
        id: u64,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        // Optional `opts.maxRequests` stop condition (for tests / graceful shutdown).
        let max_requests: Option<usize> = match arg(args, 0) {
            Value::Object(o) => match o.borrow().get("maxRequests") {
                Some(Value::Number(n)) if *n >= 0.0 => Some(*n as usize),
                _ => None,
            },
            _ => None,
        };

        // Take the listener out of the resource so we own it across awaits (the
        // resource table can't lend `&mut TcpListener` across a `call_value`).
        let listener = match self.http_server_mut(id) {
            Some(s) => match s.listener.take() {
                Some(l) => l,
                None => {
                    return Ok(err_pair("server.serve: not bound (call bind/listen first)".into()))
                }
            },
            None => return Ok(err_pair("server.serve: server is closed".into())),
        };

        let mut served = 0usize;
        loop {
            if let Some(max) = max_requests {
                if served >= max {
                    break;
                }
            }
            let (mut stream, _peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(e) => return Ok(err_pair(format!("server.serve accept failed: {}", e))),
            };
            // Serve one request per connection (sequential v1 model).
            match read_request(&mut stream).await {
                Ok(Some(req)) => {
                    let resp = self.dispatch_request(id, req, span).await?;
                    // v1 serves ONE request per connection then closes it, so the
                    // response always advertises `connection: close`.
                    let bytes = serialize_response(&resp);
                    let _ = stream.write_all(&bytes).await;
                    let _ = stream.flush().await;
                    // Half-close our side so the client's read terminates promptly.
                    let _ = stream.shutdown().await;
                    served += 1;
                }
                Ok(None) => {
                    // Client closed without sending a request; don't count it.
                    continue;
                }
                Err(_) => {
                    // Malformed request: best-effort 400, don't crash the loop.
                    let resp = HttpResponse {
                        status: 400,
                        headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
                        body: b"bad request".to_vec(),
                    };
                    let _ = stream.write_all(&serialize_response(&resp)).await;
                    let _ = stream.shutdown().await;
                    served += 1;
                }
            }
        }

        Ok(make_pair(Value::Nil, Value::Nil))
    }

    /// Build the request object, run the middleware chain → matched handler, and
    /// convert the result into an `HttpResponse`.
    async fn dispatch_request(
        &mut self,
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
        req_obj.insert("body".to_string(), Value::Str(String::from_utf8_lossy(&req.body).into_owned().into()));
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

        let result = self.run_chain(middleware, 0, handler, request, span).await;
        // A short-circuiting middleware (one that returns without calling `next`)
        // leaves its un-consumed `HttpNext` continuation in the resource table;
        // sweep any leftovers so per-request handles don't accumulate.
        self.drop_pending_http_next();
        Ok(value_to_response(&result?))
    }

    /// Run middleware `[index..]` then the terminal `handler`. Each middleware is
    /// called as `mw(req, next)` where `next` is a callable that advances the chain.
    /// A middleware that returns without calling `next` short-circuits the chain.
    /// Returns the response value.
    #[async_recursion::async_recursion(?Send)]
    async fn run_chain(
        &mut self,
        middleware: Vec<Value>,
        index: usize,
        handler: Value,
        request: Value,
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
        };
        let next_handle = self.register_resource(
            NativeKind::HttpNext,
            IndexMap::new(),
            ResourceState::HttpNext(Box::new(next_state)),
        );
        let next = match &next_handle {
            Value::Native(n) => {
                Value::NativeMethod(Rc::new(NativeMethod { receiver: n.clone(), method: "call".into() }))
            }
            _ => unreachable!("register_resource returns a Native handle"),
        };
        self.call_value(mw, vec![request, next], span).await
    }

    /// Dispatch a call to a `next` callable (an `HttpNext` handle). Resumes the
    /// middleware chain at the saved index. An optional argument lets the middleware
    /// pass a (possibly replaced) request object onward (`next(req)`); with no
    /// argument the original request is forwarded.
    pub(crate) async fn call_http_next(
        &mut self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        // Take the continuation out (a `next` is single-use, like Express's).
        let state = match self.take_resource(id) {
            Some(ResourceState::HttpNext(s)) => *s,
            _ => return Err(AsError::at("next() called twice or on an invalid handle", span).into()),
        };
        let request = match args.first() {
            Some(v) if !matches!(v, Value::Nil) => v.clone(),
            _ => state.request,
        };
        self.run_chain(state.middleware, state.index, state.handler, request, span).await
    }
}

/// The continuation state behind a `next` callable: the remaining middleware
/// chain, the index to resume at, the terminal route handler, and the request.
pub struct NextState {
    middleware: Vec<Value>,
    index: usize,
    handler: Value,
    request: Value,
}

/// Is `v` something `call_value` can invoke?
fn is_callable(v: &Value) -> bool {
    matches!(
        v,
        Value::Function(_) | Value::Builtin(_) | Value::Class(_) | Value::BoundMethod(_) | Value::NativeMethod(_)
    )
}

#[cfg(test)]
mod tests {
    use crate::interp::Interp;

    /// Run an AScript program on a caller-held interp (so we can drive `serve` and
    /// inspect output). Returns the captured output.
    async fn run_on(interp: &mut Interp, src: &str) -> Result<(), String> {
        let tokens = crate::lexer::lex(src).map_err(|e| e.message)?;
        let program = crate::parser::parse(&tokens).map_err(|e| e.message)?;
        let env = crate::interp::global_env().child();
        interp.exec(&program, &env).await.map_err(|c| format!("{:?}", c)).map(|_| ())
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
        let mut interp = Interp::new();
        run_on(&mut interp, src).await.unwrap_or_else(|e| panic!("server: {e}"));
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
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
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
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
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
        assert!(raw.to_lowercase().contains("x-made: yes"), "missing header in:\n{raw}");
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
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
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
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
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
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
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
        let (status, raw) =
            with_server(&src, move || async move { client_request_raw("GET", &url, None).await }).await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert!(raw.to_lowercase().contains("x-powered-by: ascript"), "missing header:\n{raw}");
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
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
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
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let mut interp = Interp::new();
                run_on(&mut interp, &client_src).await.expect("client ran");
                interp.output.clone()
            })
        });
        let mut interp = Interp::new();
        run_on(&mut interp, &server_src).await.expect("server ran");
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
        let mut interp = Interp::new();
        let baseline = interp.resource_count();
        run_on(&mut interp, &src).await.expect("server ran");
        client.await.unwrap();
        // The server handle itself was closed implicitly? No — `create()`'s handle
        // outlives the program, but the transient next-continuation must be gone.
        // Resource count returns to (baseline + 1 server handle), with NO next handle.
        assert_eq!(interp.resource_count(), baseline + 1, "only the server handle should remain");
    }

    /// Like `client_request` but returns the FULL raw response text (head + body)
    /// so tests can assert on headers.
    async fn client_request_raw(method: &str, url: &str, body: Option<String>) -> (String, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rest = url.strip_prefix("http://").unwrap();
        let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap();
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n");
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
        let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n"
        );
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
