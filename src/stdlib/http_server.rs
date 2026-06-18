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
use crate::value::{NativeKind, NativeMethod, OwnedKind, Value, ValueKind};
use indexmap::IndexMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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
    /// Malformed request or a mid-request I/O error → 400. Also covers a
    /// conflicting/duplicate or non-numeric/negative `Content-Length` (the parser
    /// refuses to guess a framing length rather than silently last-one-wins or zero).
    BadRequest,
    /// A `Transfer-Encoding` header is present. The server does not implement any
    /// transfer-coding (chunked decoding is unimplemented), so rather than silently
    /// reading a chunked body as EMPTY it fails loudly → 501. (Task 0.19b.)
    NotImplemented,
}

/// Typed route schemas (SP5 §2). A route may declare schemas for any of the
/// request's `params` (path params, string-origin → coerced), `query` (query
/// string, string-origin → coerced), and `body` (JSON-origin → not coerced).
/// All are optional; an all-`None` value means the route is untyped.
///
/// Back-compat: a bare schema 3rd arg (`verb(path, schema, handler)`) lowers to
/// `RouteSchemas { body: Some(schema), .. }` — today's body-only behavior.
#[derive(Clone, Default)]
pub struct RouteSchemas {
    pub params: Option<Value>,
    pub query: Option<Value>,
    pub body: Option<Value>,
}

impl RouteSchemas {
    /// True iff no schema is declared (untyped route — skip all validation).
    fn is_empty(&self) -> bool {
        self.params.is_none() && self.query.is_none() && self.body.is_none()
    }
}

/// Classify a route-registration argument that sits in the schema slot
/// (`verb(path, ARG, handler)` / `route(method, path, ARG, handler)`).
///
/// - A **bare schema** (a tagged Object with `__kind`) → `Some(RouteSchemas { body })`
///   (today's body-only behavior, preserved).
/// - An **Object WITHOUT `__kind`** carrying any of `params`/`query`/`body` (each
///   a schema value) → `Some(RouteSchemas { ... })` reading those fields. Fields
///   that are absent or not schema values are left `None`.
/// - Anything else (the handler itself, e.g. a function) → `None`, meaning the
///   3rd arg is the handler and the route is untyped.
///
/// Requires the `data` feature (schema detection needs the schema engine).
#[cfg(feature = "data")]
fn route_schemas_from_arg(arg: &Value) -> Option<RouteSchemas> {
    use crate::stdlib::schema::schema_kind;
    // Bare schema → body-only.
    if schema_kind(arg).is_some() {
        return Some(RouteSchemas {
            body: Some(arg.clone()),
            ..Default::default()
        });
    }
    // Options object: read params/query/body if each is a schema value.
    if let ValueKind::Object(o) = arg.kind() {
        let pick = |key: &str| -> Option<Value> {
            let v = o.get(key);
            match v {
                Some(ref val) if schema_kind(val).is_some() => v,
                _ => None,
            }
        };
        let schemas = RouteSchemas {
            params: pick("params"),
            query: pick("query"),
            body: pick("body"),
        };
        if !schemas.is_empty() {
            return Some(schemas);
        }
    }
    None
}

/// Routes + middleware + the (optionally) bound listener for one server handle.
pub struct HttpServerState {
    /// `(method_uppercase, path_pattern, schemas, handler)`. Path may contain
    /// `:name` params. `schemas` carries the optional params/query/body schemas
    /// (all `None` for a plain untyped route).
    pub routes: Vec<(String, String, RouteSchemas, Value)>,
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
    vec![
        ("create", bi("http_server.create")),
        // SRV Part A — the module-level multi-isolate entry. `server.serve(opts)` with
        // `workers > 1` + a `setup` worker fn spreads the accept loop across N
        // shared-nothing REUSEPORT isolates (each builds its own handle in `setup`);
        // `workers` absent/1 runs setup single-isolate. Distinct from the handle method
        // `s.serve(opts)` (which serves a pre-bound single handle).
        ("serve", bi("http_server.serve")),
    ]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg)))
}

fn obj(map: IndexMap<String, Value>) -> Value {
    Value::object(map)
}

// ── SRV Part A — multi-isolate REUSEPORT serve helpers ────────────────────────

/// Parsed `serve` options, shared by the handle method `s.serve(opts)` and the
/// module-level `server.serve(opts)`. The per-request limits (`maxBodySize`,
/// `requestTimeout`, `maxConcurrent`) and `maxRequests` shutdown were always here; SRV
/// Part A adds the multi-isolate fields (`workers`/`setup`/`args`/`host`/`port`).
struct ServeOpts {
    max_requests: Option<usize>,
    max_body: usize,
    timeout_ms: u64,
    max_concurrent: usize,
    workers: Option<usize>,
    setup_fn: Option<Value>,
    setup_args: Vec<Value>,
    host: String,
    port: Option<u16>,
}

impl ServeOpts {
    /// Parse the (optional) opts object — the first positional arg. Numbers may be
    /// `Int` or `Float` (NUM §4); a missing/non-object arg yields all defaults.
    fn parse(args: &[Value]) -> ServeOpts {
        let mut o = ServeOpts {
            max_requests: None,
            max_body: DEFAULT_MAX_BODY_BYTES,
            timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            workers: None,
            setup_fn: None,
            setup_args: Vec::new(),
            host: String::from("127.0.0.1"),
            port: None,
        };
        if let ValueKind::Object(obj) = arg(args, 0).kind() {
            if let Some(n) = obj.get("maxRequests").and_then(|v| v.as_f64()) {
                if n >= 0.0 {
                    o.max_requests = Some(n as usize);
                }
            }
            if let Some(n) = obj.get("maxBodySize").and_then(|v| v.as_f64()) {
                if n >= 0.0 {
                    o.max_body = n as usize;
                }
            }
            if let Some(n) = obj.get("requestTimeout").and_then(|v| v.as_f64()) {
                if n > 0.0 {
                    o.timeout_ms = n as u64;
                }
            }
            if let Some(n) = obj.get("maxConcurrent").and_then(|v| v.as_f64()) {
                if n >= 1.0 {
                    o.max_concurrent = n as usize;
                }
            }
            if let Some(n) = obj.get("workers").and_then(|v| v.as_f64()) {
                if n >= 0.0 {
                    o.workers = Some(n as usize);
                }
            }
            if let Some(s) = obj.get("setup") {
                if !matches!(s.kind(), ValueKind::Nil) {
                    o.setup_fn = Some(s);
                }
            }
            if let Some(OwnedKind::Array(a)) = obj.get("args").map(|x| x.into_kind()) {
                o.setup_args = a.borrow().clone();
            }
            if let Some(OwnedKind::Str(h)) = obj.get("host").map(|x| x.into_kind()) {
                o.host = h.to_string();
            }
            if let Some(n) = obj.get("port").and_then(|v| v.as_f64()) {
                if (0.0..=65535.0).contains(&n) && n.fract() == 0.0 {
                    o.port = Some(n as u16);
                }
            }
        }
        o
    }

    /// The effective isolate count: `workers: 0` resolves to `num_cpus`, any `N>=1`
    /// verbatim; `None` (absent) when the multi-isolate path is not requested.
    fn effective_workers(&self) -> Option<usize> {
        self.workers
            .map(|w| if w == 0 { num_cpus_for_serve() } else { w })
    }
}

/// The effective worker count for `workers: 0` (= `num_cpus`), mirroring the worker
/// pool's `$ASCRIPT_WORKERS`-or-`num_cpus` rule so the server tier and the pool agree.
fn num_cpus_for_serve() -> usize {
    std::env::var("ASCRIPT_WORKERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(num_cpus::get)
        .max(1)
}

/// Whether SO_REUSEPORT (kernel connection load-balancing across N sockets bound to
/// the same addr) is available on this platform. Unix-only (Linux/macOS/BSD); Windows
/// has no equivalent (SO_REUSEADDR is last-binder-wins, not balanced — SRV §2.2). A
/// failed `set_reuse_port` at bind time also degrades, so this is the static gate and
/// `bind_reuseport` is the runtime probe.
fn reuseport_available() -> bool {
    cfg!(unix)
}

/// Build ONE listening socket with `SO_REUSEPORT` (+ `SO_REUSEADDR`) set BEFORE bind,
/// so N of these on the same `host:port` form one kernel load-balancing group. Returns
/// a blocking `std::net::TcpListener` (it is `Send`, so it crosses into an isolate's
/// closure; the isolate re-wraps it with tokio's `TcpListener::from_std`). The
/// `set_reuse_port` call is `#[cfg(unix)]`-gated so the non-Unix build never references
/// the Unix-only socket2 API (this fn is only called on a REUSEPORT platform anyway).
#[cfg(unix)]
fn bind_reuseport(host: &str, port: u16) -> std::io::Result<std::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let addr: std::net::SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{e}")))?;
    let domain = if addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
    let sock = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    // REUSEADDR + REUSEPORT must be set BEFORE bind. REUSEPORT is the load-balancing
    // group; REUSEADDR avoids a stale-TIME_WAIT bind refusal.
    sock.set_reuse_address(true)?;
    sock.set_reuse_port(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&addr.into())?;
    sock.listen(1024)?;
    Ok(sock.into())
}

/// Non-Unix fallback — never actually called (the caller gates on `reuseport_available`
/// which is false off-Unix), present only so the module compiles on Windows without
/// referencing the Unix-only `set_reuse_port`.
#[cfg(not(unix))]
fn bind_reuseport(_host: &str, _port: u16) -> std::io::Result<std::net::TcpListener> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "SO_REUSEPORT is not available on this platform",
    ))
}

/// Extract the entry name of a `worker fn` value (the named top-level fn to ship to
/// each isolate). Works for BOTH engines: a tree-walker `Value::function` (reads
/// `func.name` + `func.is_worker`) and a VM `Value::closure` (reads
/// `proto.chunk.name` + `proto.is_worker`). Returns `None` for a non-worker fn or an
/// anonymous one.
fn worker_fn_entry_name(v: &Value) -> Option<String> {
    match v.kind() {
        ValueKind::Function(f) if f.is_worker => f.name.clone(),
        ValueKind::Closure(c) if c.proto.is_worker => c.proto.chunk.name.clone(),
        _ => None,
    }
}

/// Extract the resource id from a server handle `Value::native(HttpServer)` (what
/// `server.create()` returns). `None` for any other value.
fn server_handle_id(v: &Value) -> Option<u64> {
    match v.kind() {
        ValueKind::Native(n) if n.kind == NativeKind::HttpServer => Some(n.id),
        _ => None,
    }
}

/// The body each multi-isolate REUSEPORT isolate runs (SRV §3.7a): load the `setup`
/// slice into this isolate's Vm, decode the args (reconstructing any frozen
/// `Value::shared` by `Arc` bump), run `setup(...args)` to build THIS isolate's server
/// handle, wrap the REUSEPORT listener with tokio, and run the shared `accept_loop`
/// against the group-wide budget/stop. Returns `Ok(())` on a clean stop or an error
/// string (reported back to the main isolate over the `Send` completion channel).
#[allow(clippy::too_many_arguments)]
async fn run_isolate_server(
    vm: &Rc<crate::vm::Vm>,
    iso: &Rc<Interp>,
    slice_bytes: &[u8],
    entry_name: &str,
    encoded_args: &[u8],
    encoded_shared: &[std::sync::Arc<crate::value::SharedNode>],
    std_listener: std::net::TcpListener,
    budget: Arc<AtomicUsize>,
    stop: Arc<tokio::sync::Notify>,
    bounded: bool,
    max_body: usize,
    timeout_ms: u64,
    max_concurrent: usize,
) -> Result<(), String> {
    // Define the setup slice's globals on this isolate's Vm (entry + transitive deps).
    crate::worker::isolate::load_slice(vm, Some(slice_bytes)).await?;
    // Decode the setup args against THIS isolate's interp (Shared args reconstruct by
    // Arc bump from the side-vector — zero copy of the frozen graph).
    let args =
        crate::worker::isolate::decode_args_with_shared(encoded_args, encoded_shared, iso)?;
    // Resolve + run the setup entry to build this isolate's OWN server handle.
    let entry = vm
        .user_global(entry_name)
        .ok_or_else(|| format!("setup entry '{entry_name}' is not defined in the shipped slice"))?;
    let span = Span::new(0, 0);
    let handle_val = vm
        .call_value(entry, args, span)
        .await
        .map_err(|c| match c {
            Control::Panic(e) => e.message,
            _ => "setup failed".to_string(),
        })?;
    let id = server_handle_id(&handle_val).ok_or_else(|| {
        "server.serve: `setup` must return a server handle (from server.create())".to_string()
    })?;
    // Wrap the pre-bound REUSEPORT std listener with tokio (it was set nonblocking at
    // bind time). Each isolate accepts on its OWN socket in the shared kernel group.
    let listener = TcpListener::from_std(std_listener)
        .map_err(|e| format!("server.serve: could not register listener with tokio: {e}"))?;
    // Run the shared accept loop on this isolate, against the group budget/stop.
    match iso
        .accept_loop(
            listener, id, max_body, timeout_ms, max_concurrent, budget, stop, bounded, span,
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(Control::Panic(e)) => Err(e.message),
        Err(_) => Err("server.serve: accept loop ended abnormally".to_string()),
    }
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
    // Framing-header tracking (Task 0.19b). The server hand-rolls HTTP/1 framing and
    // must FAIL LOUDLY on anything it can't frame correctly, never silently guess:
    //  - any `Transfer-Encoding` → 501 (no transfer-coding/chunked decoding is
    //    implemented; reading a chunked body as empty would be a silent wrong result).
    //  - a duplicate `Content-Length` with a DIFFERING value, or a non-numeric/negative
    //    one → 400 (RFC 7230 §3.3.2; identical duplicates are collapsed to one).
    let mut content_length: Option<usize> = None;
    let mut has_transfer_encoding = false;
    let mut bad_content_length = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.eq_ignore_ascii_case("transfer-encoding") {
                has_transfer_encoding = true;
            }
            if name.eq_ignore_ascii_case("content-length") {
                // Parse strictly: a non-numeric/negative value is malformed framing.
                // (`value` is already trimmed at the top of this loop.)
                match value.parse::<usize>() {
                    Ok(n) => match content_length {
                        // A second Content-Length must MATCH the first, else it's a
                        // conflicting framing length (smuggling-class ambiguity) → 400.
                        Some(prev) if prev != n => bad_content_length = true,
                        _ => content_length = Some(n),
                    },
                    Err(_) => bad_content_length = true,
                }
            }
            headers.push((name, value));
        }
    }

    // Transfer-Encoding present → 501 (we implement no transfer-coding). Checked
    // BEFORE the body is read or the handler runs, so a chunked upload fails loudly.
    if has_transfer_encoding {
        return Err(ReadError::NotImplemented);
    }
    // Conflicting/duplicate or non-numeric/negative Content-Length → 400.
    if bad_content_length {
        return Err(ReadError::BadRequest);
    }
    let content_length = content_length.unwrap_or(0);

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
        query.insert(url_decode(k), Value::str(url_decode(v)));
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
            params.insert(name.to_string(), Value::str(url_decode(a)));
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
        501 => "Not Implemented",
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

/// Build a 400 JSON response for a schema validation failure.
/// Body: `{"error":"validation failed","path":"<path>","message":"<msg>"}`.
/// Only available when the `data` feature is enabled (requires `serde_json`).
#[cfg(feature = "data")]
fn validation_error_response(path: &str, message: &str, where_part: &str) -> HttpResponse {
    let body = format!(
        r#"{{"error":"validation failed","where":{},"path":{},"message":{}}}"#,
        serde_json::to_string(where_part).unwrap_or_else(|_| "\"\"".to_string()),
        serde_json::to_string(path).unwrap_or_else(|_| "\"\"".to_string()),
        serde_json::to_string(message).unwrap_or_else(|_| "\"validation failed\"".to_string()),
    );
    HttpResponse {
        status: 400,
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.into_bytes(),
    }
}

/// Read a top-level field (`params`/`query`/`body`) out of the request object,
/// cloning it under a short borrow (no borrow held across the subsequent await).
#[cfg(feature = "data")]
fn read_request_field(request: &Value, key: &str) -> Value {
    match request.kind() {
        ValueKind::Object(o) => o.get(key).unwrap_or(Value::nil()),
        _ => Value::nil(),
    }
}

/// Replace a top-level field in the request object with the coerced/validated value.
#[cfg(feature = "data")]
fn set_request_field(request: &Value, key: &str, value: Value) {
    if let ValueKind::Object(o) = request.kind() {
        o.borrow_mut().insert(key.to_string(), value);
    }
}

/// Map a `ParseFail` from route-schema validation to the appropriate response /
/// control flow: `Mismatch` → 400 with `where`; `InvalidSchema` → 500; a refine
/// `Control` is re-raised (handled by the panic→500 path in handle_connection).
#[cfg(feature = "data")]
fn route_schema_failure(
    e: crate::stdlib::schema::ParseFail,
    where_part: &str,
) -> Result<HttpResponse, Control> {
    use crate::stdlib::schema::ParseFail;
    match e {
        ParseFail::Mismatch(err_obj_val) => {
            let get = |k: &str| -> String {
                match err_obj_val.kind() {
                    ValueKind::Object(o) => match o.get(k).map(|x| x.into_kind()) {
                        Some(OwnedKind::Str(s)) => s.to_string(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                }
            };
            let path = get("path");
            let message = {
                let m = get("message");
                if m.is_empty() {
                    "validation failed".to_string()
                } else {
                    m
                }
            };
            Ok(validation_error_response(&path, &message, where_part))
        }
        ParseFail::InvalidSchema(msg) => {
            Ok(simple_response(500, &format!("invalid schema: {}", msg)))
        }
        ParseFail::Control(c) => Err(c),
    }
}

/// Convert a handler's return value into an `HttpResponse`.
/// Validate a single handler-supplied response header before it is written to the
/// wire. This is the chokepoint that prevents **HTTP response splitting / header
/// injection**: a handler that reflects user-controlled input into a header value
/// (or name) containing CR/LF could otherwise inject extra headers or a whole second
/// response. Every header collected in `value_to_response` passes through here.
///
/// - The NAME must be a non-empty HTTP token (RFC 7230 §3.2.6): no controls, no
///   separators (incl. `:`), and no space. We reject the ASCII control range
///   (`< 0x21`, covering CTL + space), DEL (`0x7f`), and the `tchar`-excluded
///   separators; `-` and alphanumerics (the norm) are fine. Bytes `>= 0x80` are
///   deliberately ACCEPTED — they are neither a `tchar` separator nor a
///   response-splitting risk (the security-critical bytes are CR/LF only), and
///   rejecting them would needlessly break non-ASCII names some clients tolerate.
/// - The VALUE must not contain a bare CR or LF (the security-critical bytes). Other
///   bytes — including `:` (legitimate in e.g. a `Location:` URL) — are allowed.
///
/// On violation it raises a recoverable Tier-2 panic (`AsError` → `Control::Panic`),
/// which `dispatch_request` converts to a 500 — so the malformed header is never
/// written and the response is never split.
fn validate_header(name: &str, val: &str, span: Span) -> Result<(), Control> {
    // `tchar` separators that must NOT appear in a token (RFC 7230 §3.2.6), plus the
    // ASCII control range (`< 0x21` covers CTL + space) and DEL (`0x7f`). Bytes
    // `>= 0x80` are intentionally NOT rejected (see the doc comment above).
    let is_bad_name_byte = |b: u8| {
        b < 0x21
            || b == 0x7f
            || matches!(
                b,
                b'"' | b'(' | b')' | b',' | b'/' | b':' | b';' | b'<' | b'=' | b'>' | b'?'
                    | b'@' | b'[' | b'\\' | b']' | b'{' | b'}'
            )
    };
    if name.is_empty() || name.bytes().any(is_bad_name_byte) {
        return Err(AsError::at(
            format!("invalid response header name {name:?}: must be a valid HTTP token (no control chars, separators, or spaces)"),
            span,
        )
        .into());
    }
    if val.bytes().any(|b| b == b'\r' || b == b'\n') {
        return Err(AsError::at(
            format!("invalid response header value for {name:?}: must not contain CR or LF (response-splitting guard)"),
            span,
        )
        .into());
    }
    Ok(())
}

/// - string → 200 text/plain
/// - object `{status?, headers?, body?}` → as specified (defaults 200, body "")
/// - `[value, err]` → if err non-nil → 500 with the error message; else convert value
///
/// Returns `Err(Control)` when a handler-supplied header name/value fails
/// `validate_header` (response-splitting guard). The server-built headers
/// (`content-type`, etc.) are constant tokens and always pass.
fn value_to_response(v: &Value, span: Span) -> Result<HttpResponse, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: s.as_bytes().to_vec(),
        }),
        ValueKind::Array(a) => {
            // A Result pair `[value, err]`.
            let a = a.borrow();
            if a.len() == 2 {
                let err = &a[1];
                if !matches!(err.kind(), ValueKind::Nil) {
                    let msg = error_message(err);
                    return Ok(HttpResponse {
                        status: 500,
                        headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
                        body: msg.into_bytes(),
                    });
                }
                return value_to_response(&a[0], span);
            }
            // A non-pair array: serialize via display.
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
                body: v.to_string().into_bytes(),
            })
        }
        ValueKind::Object(o) => {
            let status = match o.get("status").and_then(|v| v.as_f64()) {
                Some(n) => n as u16,
                _ => 200,
            };
            let mut headers: Vec<(String, String)> = Vec::new();
            if let Some(ValueKind::Object(h)) = o.get("headers").as_ref().map(|x| x.kind()) {
                for (k, val) in h.entries() {
                    let val = val.to_string();
                    // Reject CRLF in handler-supplied header names/values BEFORE they
                    // reach serialize_response (response-splitting guard).
                    validate_header(k.as_ref(), &val, span)?;
                    headers.push((k.to_string(), val));
                }
            }
            let body = match o.get("body") {
                None => Vec::new(),
                Some(b) => match b.kind() {
                    ValueKind::Str(s) => s.as_bytes().to_vec(),
                    ValueKind::Bytes(bytes) => bytes.borrow().clone(),
                    ValueKind::Nil => Vec::new(),
                    _ => b.to_string().into_bytes(),
                },
            };
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                && !body.is_empty()
            {
                headers.push(("content-type".into(), "text/plain; charset=utf-8".into()));
            }
            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        }
        ValueKind::Nil => Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: Vec::new(),
        }),
        _ => Ok(HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
            body: v.to_string().into_bytes(),
        }),
    }
}

/// Recover the error value carried by a `Control::Propagate`. A `?` propagation
/// carries the function's would-be return — usually a `[nil, err]` Result pair, in
/// which case the second element is the error; otherwise the value stands in.
fn propagated_error(v: &Value) -> Value {
    if let ValueKind::Array(a) = v.kind() {
        let a = a.borrow();
        if a.len() == 2 {
            return a[1].clone();
        }
    }
    v.clone()
}

/// Pull a human-readable message out of an error value (`{message}` object or string).
fn error_message(err: &Value) -> String {
    match err.kind() {
        ValueKind::Object(o) => match o.get("message").map(|x| x.into_kind()) {
            Some(OwnedKind::Str(s)) => s.to_string(),
            _ => err.to_string(),
        },
        ValueKind::Str(s) => s.to_string(),
        _ => err.to_string(),
    }
}

/// Serialize an `HttpResponse` into HTTP/1.1 wire bytes. The connection is always
/// closed after one request (v1 serves one request per connection), so a
/// `connection: close` header is emitted unless the handler set one explicitly.
///
/// RFC 9110 §9.3.2: a HEAD response MUST NOT include a message body, but the
/// headers (including `Content-Length`) must be identical to what a GET would
/// return.  `is_head` suppresses body bytes while preserving the length header.
fn serialize_response(resp: &HttpResponse, is_head: bool) -> Vec<u8> {
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
    // HEAD: headers identical to GET, body suppressed (RFC 9110 §9.3.2).
    if !is_head {
        out.extend_from_slice(&resp.body);
    }
    out
}

impl Interp {
    /// Module-level dispatch for `std/http/server` (`create`).
    pub(crate) async fn call_http_server(
        &self,
        func: &str,
        args: &[Value],
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
            // SRV Part A — the module-level multi-isolate entry. Unlike the handle method
            // `s.serve(opts)` (which serves ONE pre-bound handle), `server.serve(opts)`
            // builds every isolate's handle via `setup`: `workers > 1` fans out across N
            // REUSEPORT isolates; `workers` absent/<=1 runs `setup` single-isolate.
            "serve" => {
                let opts = ServeOpts::parse(args);
                let n = opts.effective_workers().unwrap_or(1);
                if n > 1 {
                    self.http_server_serve_multi(n, opts, span).await
                } else {
                    // Single-isolate, but still `setup`-driven (no pre-bound handle on the
                    // module path) — reuse the fallback, which runs setup on this interp.
                    self.http_server_serve_single_fallback(
                        opts.setup_fn,
                        opts.setup_args,
                        &opts.host,
                        opts.port,
                        opts.max_requests,
                        opts.max_body,
                        opts.timeout_ms,
                        opts.max_concurrent,
                        span,
                    )
                    .await
                }
            }
            // Internal terminal "handler" used when no route matched: returns a 404.
            // (Runs after any middleware, so middleware still sees unmatched requests.)
            "__not_found" => Ok(Value::object({
                let mut m = IndexMap::new();
                m.insert("status".to_string(), Value::int(404));
                m.insert("body".to_string(), Value::str("not found"));
                m
            })),
            _ => {
                Err(AsError::at(format!("std/http/server has no function '{}'", func), span).into())
            }
        }
    }

    /// Register a route with a fixed HTTP method string. Shared by `route` and the
    /// seven verb convenience methods (`get`, `post`, `put`, `patch`, `delete`,
    /// `head`, `options`) so each verb is a thin wrapper with no duplicated logic.
    ///
    /// `schema` is `Some(Value)` for a typed route (the Phase-6 schema object is
    /// stored on the route entry so `dispatch_request` can validate the body before
    /// calling the handler). `None` for a plain route (no validation).
    #[allow(clippy::too_many_arguments)]
    fn register_route(
        &self,
        id: u64,
        server: Value,
        method: String,
        path: String,
        schemas: RouteSchemas,
        handler: Value,
        span: Span,
    ) -> Result<Value, Control> {
        if !is_callable(&handler) {
            return Err(AsError::at(
                format!(
                    "server.{} handler must be a function",
                    method.to_lowercase()
                ),
                span,
            )
            .into());
        }
        match self.http_server_mut(id) {
            Some(mut s) => s.routes.push((method, path, schemas, handler)),
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
        let server = Value::native(m.receiver.clone());
        match m.method.as_str() {
            "route" => {
                let method =
                    want_string(&arg(&args, 0), span, "server.route method")?.to_uppercase();
                let path = want_string(&arg(&args, 1), span, "server.route path")?.to_string();
                // 4-arg form: route(method, path, schemaSpec, handler) when args[2]
                // is a bare schema OR a {params, query, body} options object.
                // 3-arg form: route(method, path, handler).
                // Schema detection requires the `data` feature (serde_json + schema engine).
                #[cfg(feature = "data")]
                let (schemas, handler) = {
                    let third = arg(&args, 2);
                    match route_schemas_from_arg(&third) {
                        Some(s) => (s, arg(&args, 3)),
                        None => (RouteSchemas::default(), third),
                    }
                };
                #[cfg(not(feature = "data"))]
                let (schemas, handler) = (RouteSchemas::default(), arg(&args, 2));
                self.register_route(id, server, method, path, schemas, handler, span)
            }
            // Verb shortcuts — each is a thin wrapper over register_route.
            // 3-arg form: verb(path, schema, handler) when args[1] is a schema.
            // 2-arg form: verb(path, handler).
            // Schema detection requires the `data` feature (serde_json + schema engine).
            "get" | "post" | "put" | "patch" | "delete" | "head" | "options" => {
                let verb = m.method.to_uppercase();
                let path = want_string(&arg(&args, 0), span, &format!("server.{} path", m.method))?
                    .to_string();
                #[cfg(feature = "data")]
                let (schemas, handler) = {
                    let second = arg(&args, 1);
                    match route_schemas_from_arg(&second) {
                        Some(s) => (s, arg(&args, 2)),
                        None => (RouteSchemas::default(), second),
                    }
                };
                #[cfg(not(feature = "data"))]
                let (schemas, handler) = (RouteSchemas::default(), arg(&args, 1));
                self.register_route(id, server, verb, path, schemas, handler, span)
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
                // FFI §4.4 stage-2 (net carve-out, BLOCKER 1): re-check the bind host.
                // Gate-12: no carve-out → immediate `Ok` with no comparison.
                self.check_net_host(&host, span)?;
                let addr = format!("{}:{}", host, port as u16);
                match TcpListener::bind(&addr).await {
                    Ok(listener) => {
                        let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                        match self.http_server_mut(id) {
                            Some(mut s) => s.listener = Some(listener),
                            None => return Ok(err_pair("server.bind: server is closed".into())),
                        }
                        // NUM §4: a bound port is an `Int`.
                        Ok(make_pair(Value::int(i64::from(bound)), Value::nil()))
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
                if let ValueKind::Array(a) = bound.kind() {
                    if !matches!(a.borrow().get(1).map(|x| x.kind()), Some(ValueKind::Nil)) {
                        return Ok(bound);
                    }
                }
                let serve_args = vec![arg(&args, 2)];
                self.http_server_serve(id, &serve_args, span).await
            }
            "close" => {
                self.take_resource(id);
                Ok(Value::nil())
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
        let opts = ServeOpts::parse(args);

        // SRV Part A — multi-isolate: when `workers > 1` (0 = num_cpus) the request is
        // fanned out across N shared-nothing REUSEPORT isolates, each building its OWN
        // server handle via `setup`. Otherwise the single-isolate path below runs on
        // THIS pre-bound handle, byte-for-byte unchanged.
        if let Some(n) = opts.effective_workers() {
            if n > 1 {
                return self.http_server_serve_multi(n, opts, span).await;
            }
        }

        let ServeOpts {
            max_requests,
            max_body,
            timeout_ms,
            max_concurrent,
            ..
        } = opts;

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

        // Seed the SHARED accept budget + stop signal that `accept_loop` runs against.
        // For the single-isolate path these are private to this one loop: the budget
        // is `maxRequests` (or `usize::MAX` = unbounded), and the stop `Notify` is
        // never fired by anyone but this loop reaching budget 0. The multi-isolate
        // path (SRV Part A, Task 8) clones the SAME `Arc<AtomicUsize>` budget +
        // `Arc<Notify>` into every isolate's `accept_loop`, which is exactly why the
        // body is factored out here. Single-isolate behavior is byte-for-byte the same
        // as the prior inline loop (budget reproduces `served`/`maxRequests`).
        let budget = Arc::new(AtomicUsize::new(max_requests.unwrap_or(usize::MAX)));
        let stop = Arc::new(tokio::sync::Notify::new());
        // `bounded` mirrors the old `max_requests.is_some()` — when set, the loop
        // retains + drains in-flight handler tasks and observes the shared stop.
        let bounded = max_requests.is_some();

        self.accept_loop(
            listener,
            id,
            max_body,
            timeout_ms,
            max_concurrent,
            budget,
            stop,
            bounded,
            span,
        )
        .await
    }

    /// The per-listener accept + dispatch loop, factored out of `http_server_serve`
    /// so the single-isolate path AND each multi-isolate REUSEPORT isolate (SRV
    /// Part A) run the SAME body on their own listener (`listener` by value), their
    /// own per-isolate handle `id`, sharing one `budget`/`stop` across the group.
    ///
    /// `budget` is the remaining global request count (`usize::MAX` = unbounded);
    /// each accepted connection does a saturating `fetch_sub` and, when the budget
    /// is exhausted, fires `stop` so sibling isolates blocked on `accept()` also
    /// halt. `bounded` (true iff `maxRequests` was set) toggles the deterministic
    /// drain: when set, in-flight handler tasks are retained and awaited before
    /// returning, and the loop also wakes on `stop` so a sibling reaching the global
    /// budget stops this loop too. When unset (`serve` with no `maxRequests`), this
    /// is an unbounded server: tasks are detached and the loop runs forever (the old
    /// behavior exactly — `stop` is never fired and `accept()` is awaited directly).
    #[allow(clippy::too_many_arguments)]
    async fn accept_loop(
        &self,
        listener: TcpListener,
        id: u64,
        max_body: usize,
        timeout_ms: u64,
        max_concurrent: usize,
        budget: Arc<AtomicUsize>,
        stop: Arc<tokio::sync::Notify>,
        bounded: bool,
        span: Span,
    ) -> Result<Value, Control> {
        // Bounds the number of connections handled at once. Each spawned handler
        // task holds an `OwnedSemaphorePermit` for its lifetime; the permit is
        // released (returned to the semaphore) when the task finishes. This caps
        // memory/fd usage even under a flood of slow clients.
        // `Arc` (not `Rc`): `Semaphore::acquire_owned` requires `Arc<Semaphore>` so
        // the resulting `OwnedSemaphorePermit` is `'static` and can move into the
        // spawned handler task. Arc is fine in this `!Send` single-threaded runtime —
        // the permit never crosses a thread (every task stays on the LocalSet).
        let sem = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        // In-flight handler tasks, retained ONLY when bounded (`maxRequests` set) so
        // the shutdown path can DRAIN them (await completion) before returning —
        // otherwise an accepted-but-not-yet-finished slow handler's response could be
        // lost. Without `maxRequests` (an unbounded `serve`) tasks are detached; the
        // semaphore alone bounds concurrency and finished tasks free themselves, so
        // we don't accumulate join handles forever.
        let mut inflight: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        loop {
            // Accept the next connection. On the bounded path we race `accept()` against
            // the shared `stop` so a sibling reaching the global budget wakes THIS loop
            // out of a blocking `accept()` (otherwise it would hang). On the unbounded
            // path `stop` is never fired, so we await `accept()` directly (byte-identical
            // to the old loop).
            let accepted = if bounded {
                // LOST-WAKEUP FIX: `Notify::notify_waiters` only wakes ALREADY-registered
                // waiters (it stores no permit). So we REGISTER the `stop` waiter FIRST,
                // then re-check the budget. A sibling that drove the budget to 0 and fired
                // `notify_waiters` BEFORE we registered is caught by this budget re-check
                // (it set budget=0 before notifying); one that fires AFTER is delivered to
                // our already-registered `notified`. Either way an idle isolate can no
                // longer miss the shutdown and park in `accept()` forever (which would hang
                // its thread → `serve` never returns). Reading 0 here means this loop or a
                // sibling claimed the global total — wake every sibling, then drain+return.
                let notified = stop.notified();
                tokio::pin!(notified);
                // `Notified` registers its waiter on first POLL, not on creation — so we
                // `enable()` it to register NOW, BEFORE the budget re-check. Without this
                // the register-before-check is a no-op (the select would register only
                // after the check, re-opening the lost-wakeup window). With it: a sibling
                // that fired `notify_waiters` after this `enable()` reaches our registered
                // waiter; one that fired before set budget=0 first, caught by the re-check.
                notified.as_mut().enable();
                if budget.load(Ordering::SeqCst) == 0 {
                    stop.notify_waiters();
                    break;
                }
                tokio::select! {
                    biased;
                    r = listener.accept() => Some(r),
                    _ = &mut notified => None,
                }
            } else {
                Some(listener.accept().await)
            };
            let (stream, _peer) = match accepted {
                Some(Ok(pair)) => pair,
                Some(Err(e)) => {
                    return Ok(err_pair(format!("server.serve accept failed: {}", e)))
                }
                // Woken by `stop` (a sibling hit the global budget) — drain + return.
                None => break,
            };
            // We have a connection in hand — now CLAIM one unit of the shared budget.
            // The claim is the source of truth for "exactly `maxRequests` total" (a
            // saturating `fetch_update`: an exhausted budget stays at 0, never wraps).
            // A claimed unit is ALWAYS served — we never race `stop` after claiming —
            // so the group serves exactly the budget total. If the claim fails (a
            // sibling took the last unit between our accept and here), we drop this
            // connection (it closes cleanly, the client sees a reset) and stop. This
            // is the documented OS-scheduling nondeterminism: the TOTAL is exact, the
            // per-isolate split is not (§4.1/§5). On the single-isolate path there are
            // no siblings, so the claim never fails before the budget is spent and the
            // behavior reproduces the old `served >= max` exactly.
            if bounded {
                let claimed = budget
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                        cur.checked_sub(1)
                    })
                    .is_ok();
                if !claimed {
                    // A sibling claimed the last unit first; release this connection
                    // and stop. (`stream` drops here → the socket closes.)
                    drop(stream);
                    stop.notify_waiters();
                    break;
                }
            }
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
                // RESIL §5.1/§6.4: each connection task gets a FRESH `ambient_root_scope`
                // (None-seeded TASK_LOCALS — it does NOT inherit any serve-level deadline,
                // so requests "start fresh" per §5.1) so a per-request
                // `resilience.handler({deadlineMs})` / `resilience.deadline` can set+read
                // its OWN deadline local in-server; without the scope `call_deadline`'s
                // `try_with` errs and the deadline silently no-ops. Also gives top-level
                // telemetry spans in a handler a root scope.
                crate::interp::ambient_root_scope(vm.handle_connection(
                    id, stream, max_body, timeout_ms, span,
                ))
                .await;
            });
            if bounded {
                inflight.push(handle);
                // If that claim drove the budget to 0, signal every sibling to stop
                // (and stop ourselves on the next iteration's budget==0 check).
                if budget.load(Ordering::SeqCst) == 0 {
                    stop.notify_waiters();
                }
            }
            // (Unbounded serve: the handle is dropped — the task is detached and runs
            // to completion on the LocalSet; the semaphore bounds concurrency.)

            // Periodic cycle collection (V13-T3): a long-running `serve` is THE soak
            // target — each request may build and drop a cyclic `Cc` graph that
            // refcounting alone cannot reclaim. This is a coarse, cheap safe point
            // (once per accepted connection): `maybe_collect` only sweeps once the
            // tracked-object count has grown past the threshold since the last sweep,
            // so a server with no cyclic garbage pays just a thread-local read here.
            crate::gc::maybe_collect();
        }

        // Deterministic shutdown: drain every in-flight handler task so all accepted
        // connections have had their responses written before `serve` returns.
        for handle in inflight {
            let _ = handle.await;
        }

        Ok(make_pair(Value::nil(), Value::nil()))
    }

    /// SRV Part A — the multi-isolate REUSEPORT serve path. Spawns `n` shared-nothing
    /// isolates (each a full `!Send` `Interp`/`Vm` on its OWN OS thread, sharing NO
    /// memory), each binding the SAME `host:port` via `SO_REUSEPORT` so the kernel
    /// load-balances incoming connections across them. The `setup` worker fn runs once
    /// inside each isolate at boot to build that isolate's OWN server handle (open its
    /// own DB pool, register handlers); only the sendable `args` (incl. any frozen
    /// `Value::shared` — an `Arc` pointer bump, zero copy) cross into the isolate, via
    /// direct capture in the `Send` `make_loop` closure (SRV §3.7a).
    ///
    /// A shared `Arc<AtomicUsize>` budget plus an `Arc<Notify>` stop coordinate
    /// `maxRequests` and graceful shutdown across the N threads: each accepted
    /// connection claims one unit of the SHARED budget; reaching 0 fires the stop so
    /// every isolate halts. Only the TOTAL is bounded — the per-isolate split is
    /// OS-scheduling nondeterminism (§4.1/§5). `serve` resolves once all N isolates'
    /// accept loops have stopped.
    ///
    /// **Windows / non-REUSEPORT platforms:** `SO_REUSEPORT` is Unix-only, so this is
    /// only reached on Unix — the caller (`http_server_serve`) routes Windows / a
    /// non-REUSEPORT platform to the single-isolate fallback + a one-time warn (see
    /// `reuseport_available`). Bind failures (EADDRINUSE, etc.) surface here as a clean
    /// recoverable `[nil, err]` pair, never a panic; on any spawn/bind error already
    /// spawned isolates are torn down (their `IsolateHandle` drops join the threads).
    async fn http_server_serve_multi(
        &self,
        n: usize,
        opts: ServeOpts,
        span: Span,
    ) -> Result<Value, Control> {
        let ServeOpts {
            setup_fn,
            setup_args,
            ref host,
            port,
            max_requests,
            max_body,
            timeout_ms,
            max_concurrent,
            ..
        } = opts;
        let host = host.as_str();
        // Platform gate (SRV §2.2): SO_REUSEPORT is Unix-only. On Windows / any platform
        // without it, fall back to the single-isolate path + a one-time warn — honest
        // degradation (correct, just single-core), never a silent wrong behavior.
        if !reuseport_available() {
            self.warn_reuseport_unavailable(n).await;
            return self
                .http_server_serve_single_fallback(
                    setup_fn,
                    setup_args,
                    host,
                    port,
                    max_requests,
                    max_body,
                    timeout_ms,
                    max_concurrent,
                    span,
                )
                .await;
        }

        // The `setup` worker fn is required for the multi-isolate path: each isolate is
        // a fresh, shared-nothing runtime that must build its OWN server handle (the
        // main-isolate `id` means nothing in another isolate's resource table).
        let setup = match &setup_fn {
            Some(v) => v,
            None => {
                return Ok(err_pair(
                    "server.serve: workers>1 requires a `setup` worker fn (each isolate \
                     builds its own server)"
                        .into(),
                ))
            }
        };
        let entry_name = match worker_fn_entry_name(setup) {
            Some(name) => name,
            None => {
                return Ok(err_pair(
                    "server.serve: `setup` must be a named `worker fn` (it ships to each \
                     isolate and runs there at boot)"
                        .into(),
                ))
            }
        };
        // Build the shippable code slice for `setup` from the program source/.aso
        // (the same path a `worker fn` call uses). Its `.aso` bytes are `Send`.
        let slice = crate::worker::build_code_slice_for_interp(self, &entry_name)?;
        let slice_bytes: Vec<u8> = slice.entry_aso.to_vec();
        let entry_name_owned = slice.entry_name.to_string();

        // Gate sendability of the setup args + encode them ONCE (bytes + frozen-`Arc`
        // side-vector, both `Send`); each isolate clones this to reconstruct its args.
        for a in &setup_args {
            crate::worker::serialize::check_sendable(a)
                .map_err(|e| Control::Panic(crate::error::AsError::at(e.message(), span)))?;
        }
        let args_array = Value::array(setup_args);
        let (encoded_args, encoded_shared) = crate::worker::serialize::encode(&args_array)
            .map_err(|e| Control::Panic(crate::error::AsError::at(e.message(), span)))?;

        // Bind N REUSEPORT listeners on THIS thread so a bind error (EADDRINUSE, a bad
        // host) surfaces synchronously as a clean recoverable pair — BEFORE any isolate
        // is spawned. A `std::net::TcpListener` is `Send`, so each is moved into its
        // isolate's closure and re-wrapped with tokio's `TcpListener::from_std` there.
        // All N share one kernel load-balancing group (same addr + SO_REUSEPORT).
        let bind_port = port.unwrap_or(0);
        // Port 0 = an ephemeral port: bind the FIRST socket to discover the kernel's
        // chosen port, then bind the rest to THAT same port (so all N join one group).
        let mut std_listeners: Vec<std::net::TcpListener> = Vec::with_capacity(n);
        let mut chosen_port = bind_port;
        for i in 0..n {
            let p = if i == 0 { bind_port } else { chosen_port };
            let l = match bind_reuseport(host, p) {
                Ok(l) => l,
                Err(e) => {
                    return Ok(err_pair(format!(
                        "server.serve: REUSEPORT bind on {host}:{p} failed: {e}"
                    )))
                }
            };
            if i == 0 {
                chosen_port = l.local_addr().map(|a| a.port()).unwrap_or(bind_port);
            }
            std_listeners.push(l);
        }

        // The SHARED coordination state: one budget (remaining global request count;
        // usize::MAX = unbounded) + one stop signal, cloned into every isolate.
        let budget = Arc::new(AtomicUsize::new(max_requests.unwrap_or(usize::MAX)));
        let stop = Arc::new(tokio::sync::Notify::new());
        let bounded = max_requests.is_some();

        // Spawn N dedicated isolates. Each captures (a CLONE of) the slice bytes, the
        // encoded args + frozen-`Arc` side-vector, ITS listener, the shared budget/stop
        // — all `Send` — directly in the `Send` `make_loop` closure (SRV §3.7a path-a:
        // the accept-loop isolate's inbound `Vec<u8>` channel is unused; no
        // `WorkerRequest` is built). A completion is reported back over a `Send`
        // `std::mpsc` channel so `serve` can await all N.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let mut handles: Vec<crate::worker::isolate::IsolateHandle> = Vec::with_capacity(n);
        for std_listener in std_listeners.into_iter() {
            let slice_bytes = slice_bytes.clone();
            let entry_name = entry_name_owned.clone();
            let encoded_args = encoded_args.clone();
            let encoded_shared = encoded_shared.clone();
            let budget = budget.clone();
            let stop_iso = stop.clone();
            let done_tx = done_tx.clone();
            let caps = self.caps();

            let spawned = crate::worker::isolate::spawn_isolate(move |vm, _inbound| async move {
                // Inside the fresh, shared-nothing isolate (its own thread + Interp/Vm).
                let iso = vm.interp().clone();
                // Mirror the pooled-worker authority floor (FFI §4.5a): install the
                // caller's caps; an accept-loop isolate is server-lifetime, single-
                // tenant — a `caps.drop` inside `setup` is durable there (default).
                iso.set_caps(caps);

                let result = run_isolate_server(
                    &vm,
                    &iso,
                    &slice_bytes,
                    &entry_name,
                    &encoded_args,
                    &encoded_shared,
                    std_listener,
                    budget,
                    stop_iso,
                    bounded,
                    max_body,
                    timeout_ms,
                    max_concurrent,
                )
                .await;
                let _ = done_tx.send(result);
                // The inbound channel is unused; the closure returns here, ending the
                // isolate's run-loop, so its thread exits and `IsolateHandle::drop`
                // joins it cleanly (no zombie thread).
            });

            match spawned {
                Ok(h) => handles.push(h),
                Err(e) => {
                    // A spawn failure: fire the stop so any already-running isolate
                    // halts, drop the handles (joins their threads), and report.
                    stop.notify_waiters();
                    drop(handles);
                    return Ok(err_pair(format!(
                        "server.serve: could not spawn worker isolate: {e}"
                    )));
                }
            }
        }
        // Drop our extra sender so `done_rx` closes once every isolate has reported.
        drop(done_tx);

        // Await all N isolates' accept loops on a blocking helper (the `std::mpsc`
        // recv would otherwise stall this current-thread runtime). The handles are
        // moved in so the isolate threads stay alive until they finish, then drop →
        // join (no zombie thread). A non-empty error is surfaced as the serve result.
        let first_err = tokio::task::spawn_blocking(move || {
            let _handles = handles; // keep isolates alive until all report
            let mut first_err: Option<String> = None;
            for _ in 0..n {
                match done_rx.recv() {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                    // A sender dropped without reporting (isolate thread died): treat
                    // as a clean stop of that isolate (the others still coordinate).
                    Err(_) => break,
                }
            }
            // `_handles` drops here → each `IsolateHandle::drop` joins its thread.
            first_err
        })
        .await
        .unwrap_or(None);

        match first_err {
            Some(e) => Ok(err_pair(e)),
            None => Ok(make_pair(Value::nil(), Value::nil())),
        }
    }

    /// The single-isolate fallback for `workers>1` on a non-REUSEPORT platform: run the
    /// `setup` worker fn INLINE on this interp (building one server handle here), bind a
    /// plain listener, and serve single-isolate. Behavior matches today's single-core
    /// server exactly (correct, just not parallel) — the documented Windows degradation.
    #[allow(clippy::too_many_arguments)]
    async fn http_server_serve_single_fallback(
        &self,
        setup_fn: Option<Value>,
        setup_args: Vec<Value>,
        host: &str,
        port: Option<u16>,
        max_requests: Option<usize>,
        max_body: usize,
        timeout_ms: u64,
        max_concurrent: usize,
        span: Span,
    ) -> Result<Value, Control> {
        // If a `setup` was supplied, run it on THIS interp to build the server handle
        // (it returns a server `Native`); else there is nothing to serve.
        let setup = match setup_fn {
            Some(v) => v,
            None => {
                return Ok(err_pair(
                    "server.serve: workers>1 requires a `setup` worker fn".into(),
                ))
            }
        };
        // Run setup inline. A `worker fn` called from outside an isolate would dispatch
        // to the pool; here we want it to build the handle IN this interp, so we resolve
        // the entry and call it directly as an ordinary function on this interp's VM.
        let handle_val = self.run_setup_inline(&setup, setup_args, span).await?;
        let id = match server_handle_id(&handle_val) {
            Some(id) => id,
            None => {
                return Ok(err_pair(
                    "server.serve: `setup` must return a server handle (from server.create())"
                        .into(),
                ))
            }
        };
        // Bind a plain listener on this interp's resource and serve single-isolate.
        let bind_port = port.unwrap_or(0);
        let addr = format!("{host}:{bind_port}");
        let listener = match TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => return Ok(err_pair(format!("server.serve bind on {addr} failed: {e}"))),
        };
        let budget = Arc::new(AtomicUsize::new(max_requests.unwrap_or(usize::MAX)));
        let stop = Arc::new(tokio::sync::Notify::new());
        let bounded = max_requests.is_some();
        self.accept_loop(
            listener, id, max_body, timeout_ms, max_concurrent, budget, stop, bounded, span,
        )
        .await
    }

    /// Run a `setup` worker fn inline on THIS interp (for the single-isolate fallback):
    /// resolve its entry global and call it as an ordinary function so it builds its
    /// server handle in this interp's resource table. Returns the handle value.
    async fn run_setup_inline(
        &self,
        setup: &Value,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        // A worker fn value is callable directly; calling it via `call_value` while NOT
        // in an isolate would route to the pool. To run it locally we look up the entry
        // by name on the VM (if present) or call the closure with the worker flag
        // bypassed. Simplest correct path: temporarily treat it as a normal call by
        // invoking the underlying function body. We rely on the entry being a global.
        let name = worker_fn_entry_name(setup);
        if let (Some(name), Some(vm)) = (name.as_ref(), self.vm()) {
            if let Some(entry) = vm.user_global(name) {
                // Calling the resolved global routes through the worker path again; to
                // avoid the pool we mark ourselves "in isolate" for this one call so the
                // dispatch runs inline on this VM (the entry global is already defined).
                return crate::worker::with_inline_dispatch(|| vm.call_value(entry, args, span))
                    .await;
            }
        }
        // Tree-walker (no VM) or unresolved: call the value directly under the inline
        // guard so a worker fn runs locally rather than dispatching to the pool.
        let setup = setup.clone();
        let interp = self.rc();
        crate::worker::with_inline_dispatch(|| interp.call_value(setup, args, span)).await
    }

    /// Emit the ONE-TIME `warn` that `workers:N` requested REUSEPORT but the platform
    /// lacks it, so the server is degrading to single-isolate (SRV §2.2). Best-effort,
    /// `warn`-level: routed through `std/log` when the `log` feature is on (stderr/Live
    /// or the test capture buffer), else a plain stderr line. Guarded by a process-wide
    /// atomic so it fires at most once even across many `serve` calls.
    async fn warn_reuseport_unavailable(&self, n: usize) {
        static WARNED: AtomicUsize = AtomicUsize::new(0);
        if WARNED.swap(1, Ordering::SeqCst) != 0 {
            return;
        }
        let msg = format!(
            "workers: {n} requested but SO_REUSEPORT is unavailable on this platform; \
             serving single-isolate"
        );
        #[cfg(feature = "log")]
        {
            let _ = self
                .call_log("warn", &[Value::str(msg)], Span::new(0, 0))
                .await;
        }
        #[cfg(not(feature = "log"))]
        {
            eprintln!("warning: {msg}");
        }
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
        // Track whether this is a HEAD request so we can suppress the body in
        // the serialized response while keeping Content-Length correct (RFC 9110).
        let mut is_head = false;
        let resp: Option<HttpResponse> = match read {
            Ok(Ok(Some(req))) => {
                is_head = req.method.eq_ignore_ascii_case("HEAD");
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
            // Transfer-Encoding present: we implement no transfer-coding → 501.
            Ok(Err(ReadError::NotImplemented)) => {
                Some(simple_response(501, "transfer-encoding not implemented"))
            }
            // Timer elapsed: the read didn't complete in time.
            Err(_) => Some(simple_response(408, "request timeout")),
        };
        if let Some(resp) = resp {
            // One request per connection then close, so the response always
            // advertises `connection: close`.
            let bytes = serialize_response(&resp, is_head);
            let _ = stream.write_all(&bytes).await;
            let _ = stream.flush().await;
            // Half-close our side so the client's read terminates promptly.
            let _ = stream.shutdown().await;
        }
    }

    /// Find the route matching `method`+`path`, returning its handler, optional body
    /// schema, and captured `:name` params. `(None, None, {})` if nothing matched.
    fn match_route_for(
        &self,
        id: u64,
        method: &str,
        path: &str,
    ) -> (Option<Value>, RouteSchemas, IndexMap<String, Value>) {
        let routes = match self.http_server_mut(id) {
            Some(s) => s.routes.clone(),
            None => Vec::new(),
        };
        for (rmethod, rpath, rschemas, rhandler) in &routes {
            if !rmethod.eq_ignore_ascii_case(method) {
                continue;
            }
            if let Some(params) = match_route(rpath, path) {
                return (Some(rhandler.clone()), rschemas.clone(), params);
            }
        }
        (None, RouteSchemas::default(), IndexMap::new())
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

        // Match a route to extract params (and find the handler + optional schemas).
        // `route_schemas` is only consumed by the `data`-gated validation block below;
        // under a net-without-data build it is unused, hence the `_`-prefixed name.
        #[cfg(feature = "data")]
        let (handler, route_schemas, params) = self.match_route_for(id, &req.method, &path);
        #[cfg(not(feature = "data"))]
        let (handler, _route_schemas, params) = self.match_route_for(id, &req.method, &path);

        // Build the request object passed to handlers/middleware.
        let raw_body = String::from_utf8_lossy(&req.body).into_owned();
        let mut headers_obj = IndexMap::new();
        for (k, v) in &req.headers {
            headers_obj.insert(k.to_ascii_lowercase(), Value::str(v.clone()));
        }
        let mut req_obj = IndexMap::new();
        req_obj.insert("method".to_string(), Value::str(req.method.clone()));
        req_obj.insert("path".to_string(), Value::str(path.clone()));
        req_obj.insert("query".to_string(), query);
        req_obj.insert("headers".to_string(), obj(headers_obj));
        req_obj.insert("params".to_string(), obj(params));
        req_obj.insert("body".to_string(), Value::str(raw_body.clone()));
        let request = obj(req_obj);

        // Schema validation (SP5 §2 typed routes): if the matched route declares
        // params/query/body schemas, validate (and coerce) the corresponding
        // request parts BEFORE the handler runs, in the order params → query →
        // body. On the first failure → 400 with a `where` field naming the part.
        //
        // - params/query are string-origin (HTTP), so they validate with
        //   coerce=true (a `schema.number()` accepts `"7"` → `7`). The coerced
        //   values REPLACE the raw strings in the request object the handler sees.
        // - body is JSON-origin: JSON-decode the raw body, validate with
        //   coerce=false (strict shape). On success req.body becomes the validated
        //   value and req.rawBody carries the original string (preserved behavior).
        // - JSON parse failure → 400 (body wasn't valid JSON).
        // - InvalidSchema → 500 (programmer error in the schema definition).
        // - Control (a refine-fn panic) → re-raised via the panic→500 path.
        //
        // Borrow discipline: schemas + the relevant request sub-objects are cloned
        // out under a short borrow before each `parse_value` await; no RefCell
        // borrow is held across an await.
        //
        // Only compiled when `data` feature is enabled (serde_json + schema engine).
        #[cfg(feature = "data")]
        if !route_schemas.is_empty() {
            // ── params (string-origin, coerce=true) ──────────────────────────
            if let Some(schema) = route_schemas.params.clone() {
                let cur = read_request_field(&request, "params");
                match self.parse_value(&schema, &cur, "", true, span).await {
                    Ok(validated) => set_request_field(&request, "params", validated),
                    Err(e) => return route_schema_failure(e, "params"),
                }
            }
            // ── query (string-origin, coerce=true) ───────────────────────────
            if let Some(schema) = route_schemas.query.clone() {
                let cur = read_request_field(&request, "query");
                match self.parse_value(&schema, &cur, "", true, span).await {
                    Ok(validated) => set_request_field(&request, "query", validated),
                    Err(e) => return route_schema_failure(e, "query"),
                }
            }
            // ── body (JSON-origin, coerce=false) ─────────────────────────────
            if let Some(schema) = route_schemas.body.clone() {
                let decoded = match serde_json::from_str::<serde_json::Value>(&raw_body) {
                    Ok(jv) => crate::stdlib::json::to_ascript(&jv),
                    Err(_) => {
                        return Ok(validation_error_response("", "body is not valid JSON", "body"));
                    }
                };
                match self.parse_value(&schema, &decoded, "", false, span).await {
                    Ok(validated) => {
                        if let ValueKind::Object(ro) = request.kind() {
                            let mut map = ro.borrow_mut();
                            map.insert("body".to_string(), validated);
                            map.insert(
                                "rawBody".to_string(),
                                Value::str(raw_body.clone()),
                            );
                        }
                    }
                    Err(e) => return route_schema_failure(e, "body"),
                }
            }
        }

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

        // Single dispatch site: run the middleware chain → handler and convert the
        // result to a response. Reached both by plain routes and by typed routes
        // after successful body validation (which mutated `request` in place).
        self.dispatch_handler(id, handler, request, span).await
    }

    /// Run the registered middleware chain → the terminal `handler` for one request,
    /// settling any returned `Value::future`, sweeping this dispatch's `next`
    /// continuations, and converting the outcome to an `HttpResponse`.
    ///
    /// This is the SINGLE copy of the dispatch logic, shared by the plain (no-schema)
    /// path and the post-validation path in `dispatch_request`.
    async fn dispatch_handler(
        &self,
        id: u64,
        handler: Value,
        request: Value,
        span: Span,
    ) -> Result<HttpResponse, Control> {
        let middleware = match self.http_server_mut(id) {
            Some(s) => s.middleware.clone(),
            None => Vec::new(),
        };

        // A fresh id tags every `HttpNext` continuation created for this dispatch so
        // the post-chain sweep only drops THIS request's leftovers — concurrent
        // connections (each on its own task) must not clobber one another's pending
        // continuations.
        let dispatch_id = self.next_http_dispatch_id();
        // An `async` handler/middleware returns a `Value::future` (eagerly spawned on
        // the LocalSet); settle it before converting to a response so the client sees
        // the resolved body, not the future itself. A plain (sync) handler returns a
        // non-future, so this is the identity for sequential handlers (mirrors how the
        // `await` expression drives a future, spec: `await 5 == 5`). Errors inside the
        // future surface as `Control` and become a 500 below.
        let result = match self
            .run_chain(middleware, 0, handler, request, dispatch_id, span)
            .await
        {
            Ok(v) => match v.kind() {
                ValueKind::Future(f) => f.get().await,
                _ => Ok(v),
            },
            err => err,
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
            // A handler-supplied header containing CR/LF (or an invalid name) makes
            // `value_to_response` raise a Tier-2 panic — converted to a 500 here, so a
            // response-splitting attempt fails closed instead of reaching the wire.
            Ok(v) => match value_to_response(&v, span) {
                Ok(resp) => resp,
                Err(Control::Panic(e)) => simple_response(500, &e.message),
                Err(Control::Propagate(pv)) => {
                    simple_response(500, &error_message(&propagated_error(&pv)))
                }
                Err(Control::Exit(code)) => return Err(Control::Exit(code)),
            },
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
        let next = match next_handle.kind() {
            ValueKind::Native(n) => Value::native_method(Rc::new(NativeMethod {
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
            Some(v) if !matches!(v.kind(), ValueKind::Nil) => v.clone(),
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
        v.kind(),
        // `Value::closure` is the VM's compiled-function value — `call_value` (the
        // V4-T5 bridge) dispatches it to the VM. A route handler / middleware passed
        // from a VM program is a Closure; accept it like any other callable.
        ValueKind::Function(_)
            | ValueKind::Closure(_)
            | ValueKind::Builtin(_)
            | ValueKind::Class(_)
            | ValueKind::BoundMethod(_)
            | ValueKind::NativeMethod(_)
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

    /// Security: a handler that echoes user-controlled input containing CR/LF into
    /// a response header VALUE must NOT split the response (HTTP response splitting /
    /// header injection). The CRLF-bearing header is rejected → the request fails
    /// with a 500, and crucially the injected `X-Injected` header / second response
    /// never reaches the wire.
    #[tokio::test]
    async fn crlf_in_header_value_is_rejected_not_split() {
        let port = reserve_port().await;
        // The handler puts "a\r\nX-Injected: 1" into a header value (as a real
        // attacker would by reflecting unsanitized input).
        let src = format!(
            "import {{ create }} from \"std/http/server\"\n\
             let s = create()\n\
             s.route(\"GET\", \"/inject\", (req) => ({{ status: 200, headers: {{ \"X-Reflect\": \"a\\r\\nX-Injected: 1\" }}, body: \"ok\" }}))\n\
             await s.bind(\"127.0.0.1\", {port})\n\
             await s.serve({{ maxRequests: 1 }})\n"
        );
        let url = format!("http://127.0.0.1:{port}/inject");
        let (status, raw) = with_server(&src, move || async move {
            client_request_raw("GET", &url, None).await
        })
        .await;
        // The CRLF header is rejected → a 500, NOT a 200 with a split body.
        assert_eq!(status, "HTTP/1.1 500 Internal Server Error", "raw:\n{raw}");
        // Inspect ONLY the response head (the error message body legitimately names
        // the rejected header in its diagnostic text). No injected/reflected header
        // line may appear in the head.
        let head = raw.split_once("\r\n\r\n").map(|(h, _)| h).unwrap_or(&raw);
        let head = head.to_lowercase();
        assert!(
            !head.contains("x-injected"),
            "response was split — injected header reached the wire:\n{raw}"
        );
        assert!(
            !head.contains("x-reflect"),
            "the unvalidated header leaked onto the wire:\n{raw}"
        );
    }

    /// Security: a handler-supplied header NAME containing a newline is rejected too
    /// (injecting via the name side rather than the value side).
    #[tokio::test]
    async fn crlf_in_header_name_is_rejected() {
        let port = reserve_port().await;
        let src = format!(
            "import {{ create }} from \"std/http/server\"\n\
             let s = create()\n\
             s.route(\"GET\", \"/inject\", (req) => ({{ status: 200, headers: {{ \"X-Bad\\r\\nX-Injected: 1\": \"v\" }}, body: \"ok\" }}))\n\
             await s.bind(\"127.0.0.1\", {port})\n\
             await s.serve({{ maxRequests: 1 }})\n"
        );
        let url = format!("http://127.0.0.1:{port}/inject");
        let (status, raw) = with_server(&src, move || async move {
            client_request_raw("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 500 Internal Server Error", "raw:\n{raw}");
        let head = raw.split_once("\r\n\r\n").map(|(h, _)| h).unwrap_or(&raw);
        assert!(
            !head.to_lowercase().contains("x-injected"),
            "response was split via the header name:\n{raw}"
        );
    }

    /// A legitimate header (alphanumerics + `-`, ordinary value) still works.
    #[tokio::test]
    async fn legitimate_header_still_works() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("GET", "/ok", (req) => ({{ status: 200, headers: {{ "X-Request-Id": "abc-123" }}, body: "ok" }}))
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/ok");
        let (status, raw) = with_server(&src, move || async move {
            client_request_raw("GET", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK", "raw:\n{raw}");
        assert!(
            raw.to_lowercase().contains("x-request-id: abc-123"),
            "valid header missing:\n{raw}"
        );
    }

    #[test]
    fn validate_header_unit() {
        use super::validate_header;
        let sp = crate::span::Span::new(0, 0);
        // Valid.
        assert!(validate_header("X-Request-Id", "abc-123", sp).is_ok());
        assert!(validate_header("content-type", "text/plain; charset=utf-8", sp).is_ok());
        // A colon in the VALUE is intentionally allowed (a common legitimate case,
        // e.g. a `Location` URL) — only the NAME is colon-restricted, and only CR/LF
        // are rejected from the value. Guards against a future over-restriction.
        assert!(validate_header("Location", "https://example.com/redir", sp).is_ok());
        // CR/LF in value → rejected.
        assert!(validate_header("X-Reflect", "a\r\nX-Injected: 1", sp).is_err());
        assert!(validate_header("X-Reflect", "a\nb", sp).is_err());
        assert!(validate_header("X-Reflect", "a\rb", sp).is_err());
        // Bad name: empty, contains separators / control / space / colon.
        assert!(validate_header("", "v", sp).is_err());
        assert!(validate_header("X-Bad\r\nX-Injected: 1", "v", sp).is_err());
        assert!(validate_header("has space", "v", sp).is_err());
        assert!(validate_header("has:colon", "v", sp).is_err());
        assert!(validate_header("tab\there", "v", sp).is_err());
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
    async fn max_requests_serves_exactly_n_then_returns() {
        // SRV Task 7 parity: the refactored `accept_loop` must reproduce the old
        // `served >= max` semantics EXACTLY — `maxRequests: 3` serves exactly three
        // sequential requests then `serve` returns. The 4th connection (sent after
        // serve has returned) must FAIL to get an HTTP response (the listener is gone).
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("GET", "/n", (req) => "ok")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 3 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/n");
        let (served, fourth_failed) = with_server(&src, move || async move {
            let mut served = 0usize;
            // Three sequential requests — each must succeed.
            for _ in 0..3 {
                let (status, body) = client_request("GET", &url, None).await;
                if status == "HTTP/1.1 200 OK" && body == "ok" {
                    served += 1;
                }
            }
            // A 4th connection after serve has returned: the listener is closed, so a
            // connect/request attempt must NOT yield a 200 (it errors or is refused).
            let fourth = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                client_request("GET", &url, None),
            )
            .await;
            let fourth_failed = match fourth {
                Err(_) => true, // timed out — nothing listening
                Ok((status, _)) => status != "HTTP/1.1 200 OK",
            };
            (served, fourth_failed)
        })
        .await;
        assert_eq!(served, 3, "exactly maxRequests=3 connections must be served");
        assert!(
            fourth_failed,
            "the 4th connection must fail — serve returned after exactly 3"
        );
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
        // HEAD responses must have an empty body (RFC 9110 §9.3.2).
        assert_eq!((s3.as_str(), b3.as_str()), ("HTTP/1.1 200 OK", ""));
        assert_eq!(
            (s4.as_str(), b4.as_str()),
            ("HTTP/1.1 200 OK", "options-ok")
        );
    }

    /// HEAD response: Content-Length header reflects the would-be body length but
    /// the body bytes are suppressed (RFC 9110 §9.3.2).
    #[tokio::test]
    async fn head_response_has_content_length_but_no_body() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.head("/h", (req) => "xyz")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/h");
        // Use client_request_raw to inspect headers.
        let (status, raw) = with_server(&src, move || async move {
            client_request_raw("HEAD", &url, None).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        // Content-Length must reflect the handler body ("xyz" = 3 bytes).
        assert!(
            raw.to_ascii_lowercase().contains("content-length: 3"),
            "expected content-length: 3 in headers; got: {raw:?}"
        );
        // Body section after the blank line must be empty.
        let body_after_headers = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        assert!(
            body_after_headers.is_empty(),
            "HEAD response body must be empty; got: {body_after_headers:?}"
        );
    }

    // ── End verb-method tests ──────────────────────────────────────────────────

    // ── Schema-validated route tests (sub-phase 7b) ───────────────────────────

    /// `s.post(path, schema, handler)` — valid body passes schema; handler gets
    /// a validated `req.body` Object (not the raw string).
    #[tokio::test]
    async fn schema_route_valid_body_sets_req_body() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let bodySchema = schema.object({{ name: schema.string(), age: schema.number() }})
s.post("/users", bodySchema, (req) => "ok:" + req.body.name)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/users");
        let (status, body) = with_server(&src, move || async move {
            client_request(
                "POST",
                &url,
                Some(r#"{"name":"alice","age":30}"#.to_string()),
            )
            .await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK", "valid body should reach handler");
        assert_eq!(body, "ok:alice");
    }

    /// Valid body also exposes the raw JSON string at `req.rawBody`.
    #[tokio::test]
    async fn schema_route_valid_body_keeps_raw_body() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let bodySchema = schema.object({{ name: schema.string() }})
s.post("/raw", bodySchema, (req) => "raw:" + req.rawBody)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/raw");
        let (status, body) = with_server(&src, move || async move {
            client_request("POST", &url, Some(r#"{"name":"bob"}"#.to_string())).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert!(body.starts_with("raw:"), "expected raw: prefix, got {body}");
        assert!(body.contains("bob"), "rawBody should contain the raw JSON");
    }

    /// Schema mismatch → 400, handler is NOT called (no "ok:" prefix in body).
    #[tokio::test]
    async fn schema_route_invalid_body_returns_400() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let bodySchema = schema.object({{ name: schema.string(), age: schema.number() }})
s.post("/users", bodySchema, (req) => "ok:" + req.body.name)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/users");
        let (status, body) = with_server(&src, move || async move {
            // "age" is a string, not a number → mismatch
            client_request(
                "POST",
                &url,
                Some(r#"{"name":"alice","age":"not-a-number"}"#.to_string()),
            )
            .await
        })
        .await;
        assert_eq!(
            status, "HTTP/1.1 400 Bad Request",
            "bad shape must yield 400"
        );
        // Response body should be JSON with validation error fields
        assert!(
            body.contains("validation failed"),
            "response should contain 'validation failed', got: {body}"
        );
        // The field path "age" should appear in the error details
        assert!(
            body.contains("age"),
            "error path should mention 'age', got: {body}"
        );
        // Handler output must NOT appear
        assert!(
            !body.contains("ok:"),
            "handler must NOT run on bad shape, got: {body}"
        );
    }

    /// Malformed JSON body (not valid JSON) → 400 (fused: invalid JSON = validation
    /// failed, handler not called).
    #[tokio::test]
    async fn schema_route_malformed_json_returns_400() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let bodySchema = schema.object({{ name: schema.string() }})
s.post("/users", bodySchema, (req) => "ok:" + req.body.name)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/users");
        let (status, body) = with_server(&src, move || async move {
            client_request("POST", &url, Some("{not json".to_string())).await
        })
        .await;
        assert_eq!(
            status, "HTTP/1.1 400 Bad Request",
            "malformed JSON must 400"
        );
        assert!(
            !body.contains("ok:"),
            "handler must NOT run on bad JSON, got: {body}"
        );
    }

    /// REGRESSION: plain 2-arg `s.post(path, handler)` (no schema) still works.
    #[tokio::test]
    async fn schema_route_plain_2arg_still_works() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.post("/plain", (req) => "p")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/plain");
        let (status, body) = with_server(&src, move || async move {
            client_request("POST", &url, Some("anything".to_string())).await
        })
        .await;
        assert_eq!(
            status, "HTTP/1.1 200 OK",
            "plain route (no schema) must still work"
        );
        assert_eq!(body, "p");
    }

    /// `s.route(method, path, schema, handler)` — 4-arg variant via `route()`.
    #[tokio::test]
    async fn schema_route_via_route_method_valid() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let bodySchema = schema.object({{ x: schema.number() }})
s.route("POST", "/add", bodySchema, (req) => `x:${{req.body.x}}`)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/add");
        let (status, body) = with_server(&src, move || async move {
            client_request("POST", &url, Some(r#"{"x":42}"#.to_string())).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "x:42");
    }

    // ── SP5 §2: typed params + query route schemas ────────────────────────────

    /// `:id` path param validated as `schema.number()` coerces "7" → 7; the
    /// handler does arithmetic on it (req.params.id + 1 → 8).
    #[tokio::test]
    async fn param_schema_coerces_string_to_number() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let opts = {{ params: schema.object({{ id: schema.number() }}) }}
s.get("/users/:id", opts, (req) => `id+1=${{req.params.id + 1}}`)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/users/7");
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "id+1=8.0", "param coerced to number then +1");
    }

    /// A bad param (non-numeric where number expected) → 400 with where:"params".
    #[tokio::test]
    async fn bad_param_returns_400_where_params() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let opts = {{ params: schema.object({{ id: schema.number() }}) }}
s.get("/users/:id", opts, (req) => "ok")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/users/abc");
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
        assert!(body.contains("validation failed"), "got: {body}");
        assert!(body.contains(r#""where":"params""#), "where:params; got: {body}");
        assert!(!body.contains("ok"), "handler must not run; got: {body}");
    }

    /// A query schema coerces ?page=2 → number; handler echoes req.query.page + 1.
    #[tokio::test]
    async fn query_schema_coerces() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let opts = {{ query: schema.object({{ page: schema.number() }}) }}
s.get("/list", opts, (req) => `page+1=${{req.query.page + 1}}`)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/list?page=2");
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "page+1=3.0");
    }

    /// A bad query field → 400 with where:"query".
    #[tokio::test]
    async fn bad_query_returns_400_where_query() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let opts = {{ query: schema.object({{ page: schema.number() }}) }}
s.get("/list", opts, (req) => "ok")
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/list?page=notnum");
        let (status, body) =
            with_server(&src, move || async move { client_request("GET", &url, None).await }).await;
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
        assert!(body.contains(r#""where":"query""#), "where:query; got: {body}");
    }

    /// All three schemas (params + query + body) on one POST route.
    #[tokio::test]
    async fn all_three_schemas_on_one_route() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let opts = {{
  params: schema.object({{ id: schema.number() }}),
  query: schema.object({{ verbose: schema.bool() }}),
  body: schema.object({{ name: schema.string() }}),
}}
s.route("POST", "/items/:id", opts, (req) =>
  `${{req.params.id}}|${{req.query.verbose}}|${{req.body.name}}`)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/items/9?verbose=true");
        let (status, body) = with_server(&src, move || async move {
            client_request("POST", &url, Some(r#"{"name":"widget"}"#.to_string())).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "9.0|true|widget");
    }

    /// A body schema declared via the options object (not bare) still validates.
    #[tokio::test]
    async fn body_schema_via_options_object() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import * as schema from "std/schema"
let s = create()
let opts = {{ body: schema.object({{ name: schema.string() }}) }}
s.post("/u", opts, (req) => "ok:" + req.body.name)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/u");
        let (status, body) = with_server(&src, move || async move {
            client_request("POST", &url, Some(r#"{"name":"zoe"}"#.to_string())).await
        })
        .await;
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "ok:zoe");
    }

    // ── End schema-validated route tests ──────────────────────────────────────

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

    /// Send an EXACT byte string to `host:port` (no framing helpers) and return the
    /// full raw response text. Lets a test craft otherwise-impossible requests —
    /// a `Transfer-Encoding` header, duplicate `Content-Length`, a malformed request
    /// line — that the framed `client_request`/`client_request_raw` helpers can't.
    async fn send_raw(hostport: String, raw: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            match tokio::net::TcpStream::connect(&hostport).await {
                Ok(mut s) => {
                    s.write_all(raw.as_bytes()).await.unwrap();
                    s.flush().await.unwrap();
                    let mut resp = Vec::new();
                    s.read_to_end(&mut resp).await.unwrap();
                    return String::from_utf8_lossy(&resp).into_owned();
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            }
        }
    }

    /// Issue `n` sequential GET requests to `url`, ignoring the response bodies.
    /// Owned args so the future is `'static` + `Send` (it runs in a spawned task,
    /// off the server's `!Send` LocalSet thread). Sequential (not concurrent) so
    /// the server's `maxRequests` accounting matches request count exactly and the
    /// soak runs at steady state — one in-flight request at a time.
    /// Returns how many of the `n` requests got a 200 with the expected body, so the
    /// soak test can ASSERT the handler actually ran (and built its cyclic garbage) on
    /// every request — a silently-500ing handler (e.g. a broken body) would allocate
    /// nothing and make the bounded-memory assertion vacuous.
    async fn hammer(url: String, n: usize) -> usize {
        let mut ok = 0;
        for _ in 0..n {
            let (status, body) = client_request("GET", &url, None).await;
            if status == "HTTP/1.1 200 OK" && body == "ok" {
                ok += 1;
            }
        }
        ok
    }

    // ─────────────────────────── V13-T5 SOAK GATE ───────────────────────────
    //
    // A long-running `http.serve` is THE workload M17 + the cycle GC must keep
    // BOUNDED: each request builds per-request garbage — here deliberately CYCLIC
    // (a ring of mutually-referencing objects PLUS a self-referential array) so that
    // refcounting ALONE cannot reclaim it; only the trial-deletion collector wired
    // into the accept loop (`gc::maybe_collect`, called once per accepted connection,
    // V13-T3) can. If anything retained per-request state (or that collection point
    // never fired), the live tracked-Cc count would grow ~linearly with the request
    // count — the unbounded-growth class this gate exists to reject.
    //
    // The assertion is DETERMINISTIC: `gcmodule::count_thread_tracked()` deltas, not
    // RSS. The server runs INLINE on the test thread (`run_on`'s LocalSet is on this
    // thread), and the GC tracked count is THREAD-LOCAL, so the counts observed here
    // are exactly the server's live set. Two checks:
    //
    //   (1) IN-LOOP safe point works: per-request garbage (`RING * N`) is sized to
    //       exceed the auto-collect growth threshold MANY times over, so the accept
    //       loop's `maybe_collect` MUST fire repeatedly during the run. We read the
    //       tracked count BEFORE any final forced collect; it must be bounded near a
    //       single threshold-window (≈ COLLECT_GROWTH_THRESHOLD), NOT `N*RING`.
    //   (2) Nothing is permanently retained: a final `collect()` returns the count to
    //       within a small constant of the pre-serve baseline.
    #[tokio::test]
    async fn soak_http_serve_bounded_memory() {
        // N requests; each builds a RING-node cyclic graph. `N*RING` (= 300*48 = 14_400)
        // exceeds the GC's COLLECT_GROWTH_THRESHOLD (10_000) so the in-loop safe point is
        // exercised, while staying fast (sub-second) and NOT requiring `#[ignore]`.
        const N: usize = 300;
        const RING: usize = 48;
        // Steady-state ceiling: the in-loop sweep bounds live garbage to roughly one
        // growth window plus a partial ring + buffers/route-table, NEVER `N*RING`. A
        // generous-but-still-sublinear cap: leak growth would be ≥ N*RING (14_400), so
        // anything ≤ this proves boundedness with wide margin.
        let live_ceiling: usize = crate::gc::collect_growth_threshold() + 4 * RING;
        // Final post-collect slop: tolerate a tiny constant (buffers, route table), never
        // N-proportional growth.
        const SLOP: usize = 64;

        let port = reserve_port().await;
        // Handler builds per-request CYCLIC garbage, then returns a plain string (so the
        // response retains nothing). `ring` is a RING-element array each of whose entries
        // is an object pointing at the NEXT, and the last points back at the first —
        // a single big cycle. `cyc` is additionally a self-referential array. Both are
        // dead the instant the handler returns, but neither can be freed by refcounting;
        // only the cycle collector reclaims them.
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
import {{ push }} from "std/array"
let s = create()
s.route("GET", "/garbage", (req) => {{
    let ring = []
    for (i in 0..{RING}) {{
        push(ring, {{ idx: i }})
    }}
    for (i in 0..{RING}) {{
        ring[i].next = ring[(i + 1) % {RING}]
    }}
    let cyc = []
    push(cyc, cyc)
    return "ok"
}})
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: {N} }})
"#
        );
        let url = format!("http://127.0.0.1:{port}/garbage");

        // Clean GC baseline on THIS thread, then build + run the server inline while
        // the client hammers it from a spawned task. `with_server` runs the server via
        // `run_on` (LocalSet on this thread) and returns once `serve` has handled all N
        // requests and drained its in-flight handler tasks.
        crate::gc::collect();
        let before = gcmodule::count_thread_tracked();

        let ok = with_server(&src, move || hammer(url, N)).await;

        // Guard against a vacuous pass: if the handler had silently 500'd (e.g. a broken
        // body), it would allocate no garbage and the bounded-memory check below would be
        // meaningless. Require every request to have returned the expected 200/"ok".
        assert_eq!(
            ok, N,
            "soak handler did not run successfully on every request \
             ({ok}/{N} returned 200 \"ok\") — the per-request cyclic garbage was never built, \
             so the memory assertion would be vacuous"
        );

        // CHECK 1 — the IN-LOOP collection point bounded memory DURING the run. Read the
        // live tracked count BEFORE forcing any collect: if `maybe_collect` fired in the
        // accept loop as designed, this is bounded near one growth window; if it never
        // fired / something is retained, it is ≈ before + N*RING (≥ 14_400).
        let live_before_final = gcmodule::count_thread_tracked();
        assert!(
            live_before_final <= before + live_ceiling,
            "http.serve let per-request cyclic garbage accumulate UNBOUNDED during the run: \
             live tracked = {live_before_final} (baseline {before}, ceiling {}), \
             N*RING potential = {}. The accept-loop maybe_collect safe point did not bound \
             memory — do NOT weaken this assertion; find why collection isn't firing or what \
             is retained.",
            before + live_ceiling,
            N * RING
        );

        // CHECK 2 — nothing is PERMANENTLY retained. Force a final sweep and confirm the
        // live set returns to within a small constant of the pre-serve baseline.
        let reclaimed = crate::gc::collect();
        let after = gcmodule::count_thread_tracked();

        eprintln!(
            "soak: N={N} RING={RING} (N*RING={}), tracked before={before} \
             live_before_final_collect={live_before_final} after={after} (delta={}), \
             final collect reclaimed={reclaimed}",
            N * RING,
            after as isize - before as isize
        );

        // PRIMARY GATE: bounded memory. A per-request leak of the cyclic garbage would
        // leave ~3*N (=900) extra tracked nodes; we require the post-serve live set to be
        // within a small constant of baseline, i.e. NOT proportional to N.
        assert!(
            after <= before + SLOP,
            "http.serve leaked per-request memory: tracked grew from {before} to {after} \
             over {N} requests (delta {}, allowed slop {SLOP}). Cyclic per-request garbage \
             is NOT being reclaimed — the accept-loop collection point or a retained \
             per-request reference is the bug; do NOT weaken this assertion.",
            after as isize - before as isize
        );
    }

    // ── Task 0.19b: fail loudly on unsupported/conflicting framing headers ──────
    //
    // The hand-rolled HTTP/1 parser does NOT implement transfer-codings (chunked) and
    // must not silently read a chunked body as EMPTY. It also must not last-one-wins a
    // conflicting/duplicate Content-Length. Both are silent-WRONG-result bugs; these
    // tests pin the loud failures (501 / 400).

    /// A request with `Transfer-Encoding` (any value) is rejected with a clean 501
    /// BEFORE the handler runs — NOT a 2xx with a silently-empty body. (We do not
    /// implement chunked decoding; failing loudly is the correct, safe behavior.)
    #[tokio::test]
    async fn transfer_encoding_chunked_is_501() {
        let port = reserve_port().await;
        let src = format!(
            r#"
import {{ create }} from "std/http/server"
let s = create()
s.route("POST", "/upload", (req) => "got:" + req.body)
await s.bind("127.0.0.1", {port})
await s.serve({{ maxRequests: 1 }})
"#
        );
        // A chunked POST: "5\r\nhello\r\n0\r\n\r\n" body. The old parser would read
        // content_length=0 → an EMPTY body and a 200 "got:".
        let hostport = format!("127.0.0.1:{port}");
        let raw = format!(
            "POST /upload HTTP/1.1\r\nHost: {hostport}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n"
        );
        let resp = with_server(&src, move || async move {
            send_raw(hostport, raw).await
        })
        .await;
        let status = resp.lines().next().unwrap_or("");
        assert_eq!(
            status, "HTTP/1.1 501 Not Implemented",
            "chunked request must get a clean 501, not a silent 2xx empty body:\n{resp}"
        );
        // It must NOT have reached the handler (no "got:" body).
        assert!(
            !resp.contains("got:"),
            "chunked request reached the handler (silent empty body):\n{resp}"
        );
    }

    /// Two `Content-Length` headers with DIFFERING values are rejected with a 400
    /// (the parser must not last-one-wins a conflicting framing length).
    #[tokio::test]
    async fn conflicting_content_length_is_400() {
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
        let hostport = format!("127.0.0.1:{port}");
        let raw = format!(
            "POST /echo HTTP/1.1\r\nHost: {hostport}\r\nContent-Length: 3\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
        );
        let resp = with_server(&src, move || async move {
            send_raw(hostport, raw).await
        })
        .await;
        let status = resp.lines().next().unwrap_or("");
        assert_eq!(
            status, "HTTP/1.1 400 Bad Request",
            "conflicting Content-Length must get a 400:\n{resp}"
        );
        assert!(
            !resp.contains("got:"),
            "conflicting Content-Length reached the handler:\n{resp}"
        );
    }

    /// A non-numeric / negative `Content-Length` is rejected with a 400 (the parser
    /// must not silently treat an absurd length as 0).
    #[tokio::test]
    async fn non_numeric_content_length_is_400() {
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
        let hostport = format!("127.0.0.1:{port}");
        let raw = format!(
            "POST /echo HTTP/1.1\r\nHost: {hostport}\r\nContent-Length: abc\r\nConnection: close\r\n\r\nhello"
        );
        let resp = with_server(&src, move || async move {
            send_raw(hostport, raw).await
        })
        .await;
        let status = resp.lines().next().unwrap_or("");
        assert_eq!(
            status, "HTTP/1.1 400 Bad Request",
            "non-numeric Content-Length must get a 400:\n{resp}"
        );
    }

    /// A NEGATIVE `Content-Length` is rejected with a 400 (the `usize` parse rejects
    /// the leading `-`, so the negative case is covered, not just non-numeric text).
    #[tokio::test]
    async fn negative_content_length_is_400() {
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
        let hostport = format!("127.0.0.1:{port}");
        let raw = format!(
            "POST /echo HTTP/1.1\r\nHost: {hostport}\r\nContent-Length: -1\r\nConnection: close\r\n\r\nhello"
        );
        let resp = with_server(&src, move || async move {
            send_raw(hostport, raw).await
        })
        .await;
        let status = resp.lines().next().unwrap_or("");
        assert_eq!(
            status, "HTTP/1.1 400 Bad Request",
            "negative Content-Length must get a 400:\n{resp}"
        );
    }

    /// REGRESSION: a normal request (single valid Content-Length, no Transfer-Encoding)
    /// still works — both a GET and a POST with a real body.
    #[tokio::test]
    async fn normal_request_still_works_no_regression() {
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

    /// REGRESSION: two IDENTICAL Content-Length headers are accepted (RFC 7230 §3.3.2
    /// permits collapsing identical duplicates to one) — the body is read normally.
    #[tokio::test]
    async fn identical_duplicate_content_length_is_accepted() {
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
        let hostport = format!("127.0.0.1:{port}");
        let raw = format!(
            "POST /echo HTTP/1.1\r\nHost: {hostport}\r\nContent-Length: 5\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
        );
        let resp = with_server(&src, move || async move {
            send_raw(hostport, raw).await
        })
        .await;
        let status = resp.lines().next().unwrap_or("");
        assert_eq!(
            status, "HTTP/1.1 200 OK",
            "identical duplicate Content-Length should be accepted:\n{resp}"
        );
        assert!(resp.ends_with("got:hello"), "body wrong:\n{resp}");
    }
}
