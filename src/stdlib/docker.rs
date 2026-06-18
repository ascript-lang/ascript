//! `std/docker` — a typed Docker Engine-API client over a Unix-domain socket
//! (CNTR §4.1–§4.2, feature `docker`).
//!
//! `docker.connect(opts?) -> future<[client, err]>` opens a client to the local
//! Docker daemon, negotiates an API version, and returns a `dockerClient` handle.
//! Each handle method (`containers`/`inspect`/`create`/`start`/…) drives ONE
//! request over a FRESH `UnixStream` using the hardened [`http1`](super::http1)
//! client codec (Docker speaks HTTP/1.1 `Connection: close` per call).
//!
//! ## Socket resolution (§4.1)
//! `opts.socketPath` → `$DOCKER_HOST` (a `unix://<path>` is honored; a `tcp://…`
//! is a Tier-1 error — TCP daemons are not supported, UDS only) → the default
//! `/var/run/docker.sock`.
//!
//! ## Version negotiation (§4.1)
//! On connect we `GET /v1.24/version` (the lowest common base path) and read the
//! daemon's `ApiVersion`. The negotiated version is CLAMPED to the supported range
//! `[1.24, 1.43]`; a daemon below the `1.24` floor is a Tier-1 error. The negotiated
//! version becomes the `/v<ver>/…` prefix for every later call and is readable as
//! `client.apiVersion`.
//!
//! ## Error mapping (§4.2)
//! A non-2xx response is a Tier-1 `[nil, {message, statusCode}]` pair — `message`
//! comes from the daemon's `{"message":…}` body (or a synthesized fallback) and
//! `statusCode` is the HTTP status. A 2xx JSON body is decoded to a `Value`; a 204
//! No Content is `[nil, nil]`.
//!
//! ## Capabilities
//! `docker.*` requires BOTH `net` AND `process` (the §5.2 conjunction, enforced at
//! the `call_stdlib` dispatch gate). The `dockerClient` handle's `governing_caps`
//! is the same `net ∧ process`, so a `caps.drop` after connect HOLDS.
//!
//! Docker is a POSIX/`unix`-only concept here; the non-`unix` arms are Tier-2
//! panics (`Docker is only supported on Unix`).

use super::bi;
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;

#[cfg(unix)]
use super::{arg, want_string};
#[cfg(unix)]
use crate::interp::{make_pair, ResourceState};
#[cfg(unix)]
use crate::value::{NativeKind, NativeMethod, ValueKind};
#[cfg(unix)]
use std::rc::Rc;
#[cfg(unix)]
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt};

/// CNTR §4.4: the hard cap on a single multiplexed-frame payload (16 MiB). A header
/// claiming more than this is a hostile/corrupt daemon — a Tier-1 error, NEVER an
/// allocation of the claimed size (the `.aso`-reader-clamp lesson applied to a new
/// untrusted-bytes boundary).
#[cfg(unix)]
const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

/// The supported Docker Engine API version range. `connect` clamps the daemon's
/// reported `ApiVersion` into `[FLOOR, CEIL]`; a daemon below `FLOOR` is refused.
#[cfg(unix)]
const API_FLOOR: (u32, u32) = (1, 24);
#[cfg(unix)]
const API_CEIL: (u32, u32) = (1, 43);
/// The base path used for the initial `/version` probe (the lowest common version).
#[cfg(unix)]
const PROBE_VERSION: &str = "1.24";
/// The default Docker daemon socket when neither `socketPath` nor `$DOCKER_HOST` set.
#[cfg(unix)]
const DEFAULT_SOCK: &str = "/var/run/docker.sock";

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("connect", bi("docker.connect"))]
}

/// The in-resource state for an open docker client: the resolved socket path and
/// the negotiated API version string. Each API call opens a fresh `UnixStream`, so
/// there is no live socket to hold here — just the connection parameters.
#[cfg(unix)]
pub struct DockerClientState {
    pub socket_path: String,
    pub api_version: String,
}

#[cfg(unix)]
fn docker_err_pair(message: String, status: Option<u16>) -> Value {
    let mut map = indexmap::IndexMap::new();
    map.insert("message".to_string(), Value::str(message));
    if let Some(s) = status {
        map.insert("statusCode".to_string(), Value::int(s as i64));
    }
    make_pair(Value::nil(), Value::object(map))
}

/// Parse a `"<major>.<minor>"` version string into a comparable `(u32, u32)` tuple.
#[cfg(unix)]
fn parse_version(s: &str) -> Option<(u32, u32)> {
    let mut it = s.trim().splitn(2, '.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    Some((major, minor))
}

/// Clamp a parsed `(major, minor)` into `[API_FLOOR, API_CEIL]`, returning the
/// clamped `"major.minor"` string. Assumes the caller already enforced the floor.
#[cfg(unix)]
fn clamp_version(v: (u32, u32)) -> String {
    let clamped = if v < API_FLOOR {
        API_FLOOR
    } else if v > API_CEIL {
        API_CEIL
    } else {
        v
    };
    format!("{}.{}", clamped.0, clamped.1)
}

impl Interp {
    /// Module-level dispatch for `std/docker`.
    pub(crate) async fn call_docker(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "connect" => self.docker_connect(args, span).await,
            _ => Err(AsError::at(format!("std/docker has no function '{}'", func), span).into()),
        }
    }

    /// `docker.connect(opts?)` — resolve the socket, negotiate the API version, and
    /// register a `dockerClient` handle. Tier-1 `[nil, err]` on a resolution /
    /// connection / version-floor failure.
    #[cfg(unix)]
    async fn docker_connect(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        // Resolve the socket path (§4.1).
        let opts = arg(args, 0);
        let socket_path = match self.docker_resolve_socket(&opts, span)? {
            Ok(p) => p,
            Err(e) => return Ok(e), // a Tier-1 [nil, err] (e.g. tcp:// host)
        };

        // §3.1 stage-2 carve-out: re-check the resolved socket path before connecting.
        self.check_unix_path(&socket_path, span)?;

        // Version negotiation: GET /v1.24/version on the lowest common base path.
        let path = format!("/v{}/version", PROBE_VERSION);
        let resp = match self
            .docker_unary_raw(&socket_path, "GET", &path, None, span)
            .await
        {
            Ok(r) => r,
            Err(msg) => return Ok(docker_err_pair(msg, None)),
        };
        if !(200..300).contains(&resp.0) {
            let (msg, status) = docker_decode_error(&resp);
            return Ok(docker_err_pair(msg, Some(status)));
        }
        // Decode the /version body and pull out `ApiVersion` via std/json.
        let text = String::from_utf8_lossy(&resp.1).into_owned();
        let body = crate::stdlib::json::call("parse", &[Value::str(text)], span)?;
        // body is a [val, err] pair from json.parse.
        let api_version_raw = match docker_pair_value(&body) {
            Some(v) => match self.read_member(&v, "ApiVersion", span) {
                Ok(av) => match av.kind() {
                    ValueKind::Str(s) => s.to_string(),
                    _ => String::new(),
                },
                Err(_) => String::new(),
            },
            None => String::new(),
        };
        let parsed = match parse_version(&api_version_raw) {
            Some(p) => p,
            None => {
                return Ok(docker_err_pair(
                    format!(
                        "docker: could not parse daemon ApiVersion '{}'",
                        api_version_raw
                    ),
                    None,
                ))
            }
        };
        if parsed < API_FLOOR {
            return Ok(docker_err_pair(
                format!(
                    "docker: daemon API version {}.{} is below the supported floor {}.{} — \
                     upgrade Docker Engine",
                    parsed.0, parsed.1, API_FLOOR.0, API_FLOOR.1
                ),
                None,
            ));
        }
        let negotiated = clamp_version(parsed);

        // Register the client handle. `apiVersion` / `socketPath` are readable fields.
        let mut fields = indexmap::IndexMap::new();
        fields.insert("apiVersion".to_string(), Value::str(negotiated.clone()));
        fields.insert("socketPath".to_string(), Value::str(socket_path.clone()));
        let handle = self.register_resource(
            NativeKind::DockerClient,
            fields,
            ResourceState::DockerClient(DockerClientState {
                socket_path,
                api_version: negotiated,
            }),
        );
        Ok(make_pair(handle, Value::nil()))
    }

    /// Resolve the daemon socket path (§4.1). Returns `Ok(Ok(path))` on success, or
    /// `Ok(Err(tier1_pair))` for a `tcp://` `$DOCKER_HOST` (TCP daemons unsupported).
    #[cfg(unix)]
    fn docker_resolve_socket(
        &self,
        opts: &Value,
        span: Span,
    ) -> Result<Result<String, Value>, Control> {
        // 1. opts.socketPath.
        if let ValueKind::Object(o) = opts.kind() {
            if let Some(sp) = o.get("socketPath") {
                let p = want_string(&sp, span, "docker.connect socketPath")?;
                return Ok(Ok(p.to_string()));
            }
            // opts.host: an explicit unix://… / tcp://… override.
            if let Some(h) = o.get("host") {
                let hs = want_string(&h, span, "docker.connect host")?;
                return Ok(self.docker_parse_host(&hs));
            }
        }
        // 2. $DOCKER_HOST.
        if let Ok(dh) = std::env::var("DOCKER_HOST") {
            if !dh.is_empty() {
                return Ok(self.docker_parse_host(&dh));
            }
        }
        // 3. Default.
        Ok(Ok(DEFAULT_SOCK.to_string()))
    }

    /// Parse a `DOCKER_HOST`-style host string. `unix://<path>` → the path; a
    /// `tcp://…` (or any non-unix scheme) → a Tier-1 unsupported error.
    #[cfg(unix)]
    fn docker_parse_host(&self, host: &str) -> Result<String, Value> {
        if let Some(path) = host.strip_prefix("unix://") {
            return Ok(path.to_string());
        }
        if host.starts_with("tcp://") || host.starts_with("http://") || host.starts_with("https://")
        {
            return Err(docker_err_pair(
                "docker: Docker over TCP is not supported — use a Unix socket (unix://… or socketPath)"
                    .to_string(),
                None,
            ));
        }
        // A bare path with no scheme is treated as a socket path.
        Ok(host.to_string())
    }

    /// Drive ONE Docker API request over a fresh `UnixStream`: connect, send via the
    /// http1 codec, read the (bounded) body. Returns `(status, body_bytes)` or a
    /// transport error string. The caller maps non-2xx + decodes JSON.
    #[cfg(unix)]
    async fn docker_unary_raw(
        &self,
        socket_path: &str,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        _span: Span,
    ) -> Result<(u16, Vec<u8>), String> {
        use crate::stdlib::http1;
        let stream = tokio::net::UnixStream::connect(socket_path)
            .await
            .map_err(|e| format!("docker: connect to '{}' failed: {}", socket_path, e))?;
        let mut headers: Vec<(String, String)> = Vec::new();
        if body.is_some() {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
        }
        let req = http1::Http1Request {
            method,
            path,
            headers,
            body,
        };
        let resp = http1::send_request(stream, &req)
            .await
            .map_err(|e| format!("docker: {}", e))?;
        let status = resp.status;
        let buf = match resp.body {
            http1::Http1Body::Stream(reader) => reader
                .read_to_end(super::MAX_ALLOC_COUNT as usize)
                .await
                .map_err(|e| format!("docker: {}", e))?,
            http1::Http1Body::Upgraded { .. } => {
                return Err(
                    "docker: server returned 101 Switching Protocols (not supported by unary API)"
                        .to_string(),
                )
            }
        };
        Ok((status, buf))
    }

    /// Method dispatch on a `dockerClient` handle.
    #[cfg(unix)]
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_docker_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        // Pull the connection parameters out (cheap clone; the handle stays live so
        // multiple calls can run). NEVER hold the resources borrow across the await.
        let (socket_path, api_version) = match self.with_resource(id, |r| match r {
            Some(ResourceState::DockerClient(c)) => {
                Some((c.socket_path.clone(), c.api_version.clone()))
            }
            _ => None,
        }) {
            Some(t) => t,
            None => {
                // close() drops the resource; a method after close is a Tier-1 err.
                return Ok(docker_err_pair("docker: client is closed".to_string(), None));
            }
        };

        // `close()` tears the handle down (no live socket — just removes the entry).
        if m.method == "close" {
            self.take_resource(id);
            return Ok(Value::nil());
        }

        // §4.3 streaming verbs return a `dockerStream` handle (NOT a unary response).
        match m.method.as_str() {
            "logs" => return self.docker_logs(&socket_path, &api_version, &args, span).await,
            "events" => return self.docker_events(&socket_path, &api_version, &args, span).await,
            "pull" => return self.docker_pull(&socket_path, &api_version, &args, span).await,
            // §4.5 exec verbs.
            "execCreate" => {
                return self
                    .docker_exec_create(&socket_path, &api_version, &args, span)
                    .await
            }
            "execStart" => {
                return self
                    .docker_exec_start(&socket_path, &api_version, &args, span)
                    .await
            }
            "execInspect" => {
                return self
                    .docker_exec_inspect(&socket_path, &api_version, &args, span)
                    .await
            }
            "exec" => return self.docker_exec(&socket_path, &api_version, &args, span).await,
            _ => {}
        }

        self.docker_invoke(&socket_path, &api_version, &m.method, &args, span)
            .await
    }

    /// The unary API table: build the `(method, path, body)` for `name`, drive it,
    /// and map the response. Each verb validates its arguments (a non-string id is a
    /// Tier-2 panic); a non-2xx response is a `[nil, {message, statusCode}]` pair;
    /// 204 → `[nil, nil]`; a 2xx JSON body → `[decoded, nil]`.
    #[cfg(unix)]
    async fn docker_invoke(
        &self,
        socket_path: &str,
        api_version: &str,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let base = format!("/v{}", api_version);
        // (method, path, optional JSON body bytes)
        let (method, path, body): (&str, String, Option<Vec<u8>>) = match name {
            "ping" => ("GET", format!("{base}/_ping"), None),
            "version" => ("GET", format!("{base}/version"), None),
            "info" => ("GET", format!("{base}/info"), None),
            "containers" => {
                let opts = args.first().cloned().unwrap_or_else(Value::nil);
                let mut q: Vec<(String, String)> = Vec::new();
                if let ValueKind::Object(o) = opts.kind() {
                    if matches!(o.get("all").map(|v| v.into_kind()), Some(crate::value::OwnedKind::Bool(true)))
                    {
                        q.push(("all".to_string(), "true".to_string()));
                    }
                    if let Some(f) = o.get("filters") {
                        q.push(("filters".to_string(), self.docker_encode_filters(&f, span)?));
                    }
                }
                ("GET", docker_with_query(format!("{base}/containers/json"), &q), None)
            }
            "inspect" => {
                let cid = self.docker_want_id(args, span, "docker.inspect")?;
                ("GET", format!("{base}/containers/{cid}/json"), None)
            }
            "create" => {
                let cfg = args.first().cloned().unwrap_or_else(Value::nil);
                let body = self.docker_json_body(&cfg, span, "docker.create")?;
                ("POST", format!("{base}/containers/create"), Some(body))
            }
            "start" => {
                let cid = self.docker_want_id(args, span, "docker.start")?;
                ("POST", format!("{base}/containers/{cid}/start"), None)
            }
            "stop" => {
                let cid = self.docker_want_id(args, span, "docker.stop")?;
                ("POST", format!("{base}/containers/{cid}/stop"), None)
            }
            "restart" => {
                let cid = self.docker_want_id(args, span, "docker.restart")?;
                ("POST", format!("{base}/containers/{cid}/restart"), None)
            }
            "wait" => {
                let cid = self.docker_want_id(args, span, "docker.wait")?;
                ("POST", format!("{base}/containers/{cid}/wait"), None)
            }
            "remove" => {
                let cid = self.docker_want_id(args, span, "docker.remove")?;
                let force = matches!(
                    args.get(1)
                        .and_then(|o| if let ValueKind::Object(m) = o.kind() { m.get("force") } else { None })
                        .map(|v| v.into_kind()),
                    Some(crate::value::OwnedKind::Bool(true))
                );
                let mut q: Vec<(String, String)> = Vec::new();
                if force {
                    q.push(("force".to_string(), "true".to_string()));
                }
                ("DELETE", docker_with_query(format!("{base}/containers/{cid}"), &q), None)
            }
            "images" => {
                let opts = args.first().cloned().unwrap_or_else(Value::nil);
                let mut q: Vec<(String, String)> = Vec::new();
                if let ValueKind::Object(o) = opts.kind() {
                    if let Some(f) = o.get("filters") {
                        q.push(("filters".to_string(), self.docker_encode_filters(&f, span)?));
                    }
                }
                ("GET", docker_with_query(format!("{base}/images/json"), &q), None)
            }
            "removeImage" => {
                let iid = self.docker_want_id(args, span, "docker.removeImage")?;
                let force = matches!(
                    args.get(1)
                        .and_then(|o| if let ValueKind::Object(m) = o.kind() { m.get("force") } else { None })
                        .map(|v| v.into_kind()),
                    Some(crate::value::OwnedKind::Bool(true))
                );
                let mut q: Vec<(String, String)> = Vec::new();
                if force {
                    q.push(("force".to_string(), "true".to_string()));
                }
                ("DELETE", docker_with_query(format!("{base}/images/{iid}"), &q), None)
            }
            other => {
                return Err(AsError::at(
                    format!("dockerClient has no method '{}'", other),
                    span,
                )
                .into())
            }
        };

        let resp = match self
            .docker_unary_raw(socket_path, method, &path, body.as_deref(), span)
            .await
        {
            Ok(r) => r,
            Err(msg) => return Ok(docker_err_pair(msg, None)),
        };
        let mapped = self.docker_map_response(resp, span)?;
        Ok(docker_postprocess_unary(name, mapped))
    }

    /// Validate that arg 0 is a string id; a non-string is a Tier-2 panic.
    #[cfg(unix)]
    fn docker_want_id(&self, args: &[Value], span: Span, ctx: &str) -> Result<String, Control> {
        let v = arg(args, 0);
        Ok(want_string(&v, span, ctx)?.to_string())
    }

    /// Encode a `filters` value (an Object or Map) as a JSON string for the query
    /// param. REUSES `std/json`'s encoder (never hand-rolled). A non-object filters
    /// value is a Tier-2 panic.
    #[cfg(unix)]
    fn docker_encode_filters(&self, v: &Value, span: Span) -> Result<String, Control> {
        match crate::stdlib::json::from_ascript(v, &mut Vec::new()) {
            Ok(jv) => Ok(jv.to_string()),
            Err(msg) => Err(AsError::at(format!("docker filters: {}", msg), span).into()),
        }
    }

    /// Encode a config value as a JSON request body via `std/json`'s encoder.
    #[cfg(unix)]
    fn docker_json_body(&self, v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
        match crate::stdlib::json::from_ascript(v, &mut Vec::new()) {
            Ok(jv) => Ok(jv.to_string().into_bytes()),
            Err(msg) => Err(AsError::at(format!("{}: {}", ctx, msg), span).into()),
        }
    }

    /// Map a `(status, body)` response to a script-visible result: 204 → `[nil, nil]`;
    /// 2xx with a JSON body → `[decoded, nil]`; non-2xx → `[nil, {message, statusCode}]`.
    #[cfg(unix)]
    fn docker_map_response(&self, resp: (u16, Vec<u8>), span: Span) -> Result<Value, Control> {
        let (status, body) = resp;
        if status == 204 || body.is_empty() {
            if (200..300).contains(&status) {
                return Ok(make_pair(Value::nil(), Value::nil()));
            }
            let (msg, _) = docker_decode_error(&(status, body));
            return Ok(docker_err_pair(msg, Some(status)));
        }
        if !(200..300).contains(&status) {
            let (msg, _) = docker_decode_error(&(status, body));
            return Ok(docker_err_pair(msg, Some(status)));
        }
        // 2xx with a body: decode JSON via std/json. Docker mostly returns JSON, but a
        // few endpoints (`/_ping`) return plain text (`OK`); a non-JSON 2xx body is NOT
        // an error — surface the raw text so `ping()` succeeds with `["OK", nil]`.
        let text = String::from_utf8_lossy(&body).into_owned();
        let parsed = crate::stdlib::json::call("parse", &[Value::str(text.clone())], span)?;
        match docker_pair_err(&parsed) {
            Some(true) => Ok(make_pair(Value::str(text), Value::nil())),
            _ => {
                let v = docker_pair_value(&parsed).unwrap_or_else(Value::nil);
                Ok(make_pair(v, Value::nil()))
            }
        }
    }

    /// Non-Unix stub: docker is a POSIX/UDS concept here.
    #[cfg(not(unix))]
    async fn docker_connect(&self, _args: &[Value], span: Span) -> Result<Value, Control> {
        Err(AsError::at("Docker is only supported on Unix".to_string(), span).into())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// CNTR §4.3–4.4: streaming — logs / events / pull as `for await` streams.
// ──────────────────────────────────────────────────────────────────────────────

/// The buffered byte source behind a `dockerStream`: a `BufReader` over the http1
/// body's `ByteStream` (the EXACT type SSE/HttpBody use — pull-driven, backpressure
/// to the daemon). Reads are bounded by the framing; the `BufReader` transparently
/// reassembles a frame whose bytes are split across multiple daemon writes.
#[cfg(unix)]
pub(crate) type DockerByteSource = tokio::io::BufReader<
    tokio_util::io::StreamReader<crate::stdlib::net_http::ByteStream, tokio_util::bytes::Bytes>,
>;

/// How a docker stream's body is framed (resolved at `logs`/`events`/`pull` time, or —
/// for logs — lazily on the FIRST 8 bytes via the §4.4 auto-detection).
#[cfg(unix)]
pub(crate) enum StreamFraming {
    /// A logs/attach stream whose framing has not yet been resolved (resolve on the
    /// first 8 bytes: a valid multiplex header → `Multiplexed`, else → `Tty`).
    LogsUnresolved,
    /// The §4.4 8-byte multiplex demux (stdout/stderr interleaved).
    Multiplexed,
    /// A raw TTY stream — no framing; the whole body is stdout text.
    Tty,
    /// Newline-delimited JSON (events / pull progress). `pull` surfaces an in-stream
    /// `{"error": …}` line as a terminal `[nil, err]`.
    JsonLines { is_pull: bool },
}

/// The live state behind a `Value::native(DockerStream)`: the byte source + framing.
#[cfg(unix)]
pub(crate) struct DockerStreamState {
    pub source: DockerByteSource,
    pub framing: StreamFraming,
}

/// Build the buffered byte source from an http1 streaming body. A `101`/upgraded body
/// is not expected for these endpoints — caller maps it to a Tier-1 error first.
#[cfg(unix)]
pub(crate) fn docker_byte_source(body: crate::stdlib::http1::BodyReader<tokio::net::UnixStream>) -> DockerByteSource {
    tokio::io::BufReader::new(tokio_util::io::StreamReader::new(body.into_byte_stream()))
}

/// The outcome of decoding one item from a docker stream.
#[cfg(unix)]
pub(crate) enum DockerItem {
    /// A decoded item to yield (`[item, nil]`).
    Item(Value),
    /// A terminal error (`[nil, err]`) — e.g. a truncated/oversize frame, or a pull
    /// `{"error": …}` line. The stream is finished after this.
    Err(Value),
    /// Clean end of stream (`[nil, nil]`).
    End,
}

/// Decode the next item from a logs stream (multiplex demux OR raw TTY). Resolves the
/// framing on the first 8 bytes when `unresolved` is true. Generic over the reader so
/// unit tests drive it over a duplex / 1-byte-at-a-time reader (no daemon).
///
/// Reads exactly 8 header bytes: EOF at a frame boundary (0 header bytes read) = clean
/// `End`; EOF after ≥1 header byte = a truncated-stream `Err`. A valid header
/// (`STREAM_TYPE ∈ {0,1,2}`, bytes 1–3 zero, `SIZE ≤ MAX_FRAME_SIZE`) → read exactly
/// SIZE payload bytes (EOF mid-payload = truncated `Err`) → `{stream, text}`. An
/// invalid first header → the whole stream is raw TTY (the header bytes already read
/// are the first chunk).
#[cfg(unix)]
pub(crate) async fn demux_next<R: AsyncRead + AsyncBufRead + Unpin>(
    reader: &mut R,
    framing: &mut StreamFraming,
) -> std::io::Result<DockerItem> {
    loop {
        match framing {
            StreamFraming::Tty => {
                // Raw TTY: read a chunk and emit it as one stdout item; EOF = End.
                let mut buf = [0u8; 16 * 1024];
                let n = reader.read(&mut buf).await?;
                if n == 0 {
                    return Ok(DockerItem::End);
                }
                return Ok(DockerItem::Item(logs_item("stdout", &buf[..n])));
            }
            StreamFraming::LogsUnresolved | StreamFraming::Multiplexed => {
                let resolving = matches!(framing, StreamFraming::LogsUnresolved);
                // Read exactly 8 header bytes (transparently reassembled across reads).
                let mut header = [0u8; 8];
                let got = read_up_to(reader, &mut header).await?;
                if got == 0 {
                    return Ok(DockerItem::End); // clean EOF at a frame boundary
                }
                if got < 8 {
                    // EOF mid-header after ≥1 byte → truncated stream.
                    return Ok(DockerItem::Err(stream_err(
                        "docker: log stream truncated mid-frame-header",
                    )));
                }
                let stream_type = header[0];
                let valid = matches!(stream_type, 0..=2)
                    && header[1] == 0
                    && header[2] == 0
                    && header[3] == 0;
                if resolving && !valid {
                    // §4.4 TTY auto-detection: the first 8 bytes are not a valid frame
                    // header → the whole stream is raw TTY. The bytes already read are
                    // the first chunk of text.
                    *framing = StreamFraming::Tty;
                    return Ok(DockerItem::Item(logs_item("stdout", &header[..got])));
                }
                if !valid {
                    // A mid-stream invalid header on a stream we already resolved as
                    // multiplexed is a corrupt/desynced daemon → truncated-stream Err.
                    return Ok(DockerItem::Err(stream_err(
                        "docker: log stream frame header is invalid (corrupt multiplex frame)",
                    )));
                }
                let size = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
                if size > MAX_FRAME_SIZE {
                    // Oversize frame: a Tier-1 error WITHOUT allocating the claimed size.
                    return Ok(DockerItem::Err(stream_err(&format!(
                        "docker: log stream frame size {} exceeds the {}-byte cap",
                        size, MAX_FRAME_SIZE
                    ))));
                }
                if resolving {
                    *framing = StreamFraming::Multiplexed;
                }
                // Read exactly SIZE payload bytes (reassembled across reads).
                let mut payload = vec![0u8; size as usize];
                let pgot = read_up_to(reader, &mut payload).await?;
                if pgot < size as usize {
                    return Ok(DockerItem::Err(stream_err(
                        "docker: log stream truncated mid-frame-payload",
                    )));
                }
                let name = if stream_type == 2 { "stderr" } else { "stdout" };
                return Ok(DockerItem::Item(logs_item(name, &payload)));
            }
            StreamFraming::JsonLines { is_pull } => {
                let is_pull = *is_pull;
                let mut line: Vec<u8> = Vec::new();
                let n = reader.read_until(b'\n', &mut line).await?;
                if n == 0 {
                    return Ok(DockerItem::End); // clean EOF
                }
                // Strip the trailing '\n' (and an optional '\r').
                if line.last() == Some(&b'\n') {
                    line.pop();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                }
                if line.is_empty() {
                    continue; // stray blank line — keep reading
                }
                let text = String::from_utf8_lossy(&line).into_owned();
                let decoded = match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(jv) => crate::stdlib::json::to_ascript(&jv),
                    Err(_) => {
                        // A non-JSON line is surfaced as a raw-string item rather than
                        // ending the stream (tolerant of a daemon quirk).
                        return Ok(DockerItem::Item(Value::str(text)));
                    }
                };
                // pull's in-stream `{"error": …}` line → a terminal [nil, err] item.
                if is_pull {
                    if let ValueKind::Object(o) = decoded.kind() {
                        if let Some(e) = o.get("error") {
                            let msg = match e.kind() {
                                ValueKind::Str(s) => s.to_string(),
                                _ => e.to_string(),
                            };
                            return Ok(DockerItem::Err(stream_err(&msg)));
                        }
                    }
                }
                return Ok(DockerItem::Item(decoded));
            }
        }
    }
}

/// Read into `buf` until it is full or EOF, returning the number of bytes read. A
/// short return (< `buf.len()`) means EOF was hit. Unlike `read_exact`, EOF is not an
/// error — the caller decides whether a short read is a clean end or a truncation.
#[cfg(unix)]
async fn read_up_to<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// Build a `{stream, text}` logs item (UTF-8-lossy text decode).
#[cfg(unix)]
fn logs_item(stream: &str, payload: &[u8]) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("stream".to_string(), Value::str(stream));
    m.insert(
        "text".to_string(),
        Value::str(String::from_utf8_lossy(payload).into_owned()),
    );
    Value::object(m)
}

/// Build a stream error value `{message}` for a terminal `[nil, err]` item.
#[cfg(unix)]
fn stream_err(message: &str) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("message".to_string(), Value::str(message));
    Value::object(m)
}

impl Interp {
    /// Method dispatch on a `dockerStream` handle: `next()` / `close()`.
    #[cfg(unix)]
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_docker_stream_method(
        &self,
        m: &Rc<NativeMethod>,
        _args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "next" => self.docker_stream_next(id, span).await,
            "close" => {
                // Drop the resource (and its underlying connection) → deterministic fd
                // reclaim. A subsequent next() finds no resource and returns [nil, nil].
                self.take_resource(id);
                Ok(Value::nil())
            }
            other => Err(AsError::at(
                format!("dockerStream has no method '{}'", other),
                span,
            )
            .into()),
        }
    }

    /// `stream.next()` → `[item, nil]` for the next item, `[nil, err]` for a terminal
    /// error (truncated/oversize frame, pull error line), or `[nil, nil]` at end. The
    /// state is taken OUT across the read await (never hold the table borrow), then put
    /// back; a terminal `Err`/`End` drops the resource.
    #[cfg(unix)]
    async fn docker_stream_next(&self, id: u64, span: Span) -> Result<Value, Control> {
        let mut state = match self.take_resource(id) {
            Some(ResourceState::DockerStream(s)) => s,
            other => {
                if let Some(o) = other {
                    self.return_resource(id, o);
                }
                return Ok(make_pair(Value::nil(), Value::nil())); // closed/gone → ended
            }
        };
        let outcome = demux_next(&mut state.source, &mut state.framing).await;
        match outcome {
            Ok(DockerItem::Item(v)) => {
                self.return_resource(id, ResourceState::DockerStream(state));
                Ok(make_pair(v, Value::nil()))
            }
            Ok(DockerItem::Err(e)) => {
                // Terminal error — drop the connection (do NOT return the state).
                Ok(make_pair(Value::nil(), e))
            }
            Ok(DockerItem::End) => {
                // Clean end — drop the connection.
                Ok(make_pair(Value::nil(), Value::nil()))
            }
            Err(io) => {
                // A transport read error → a terminal `[nil, err]` (drop the connection).
                let _ = span;
                Ok(make_pair(
                    Value::nil(),
                    stream_err(&format!("docker: stream read failed: {}", io)),
                ))
            }
        }
    }

    /// Open a streaming request over a dedicated `UnixStream` and register a
    /// `dockerStream` handle. `framing` selects the demux. A connect/transport failure
    /// or a non-2xx head is a Tier-1 `[nil, err]`.
    #[cfg(unix)]
    async fn docker_open_stream(
        &self,
        socket_path: &str,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        framing: StreamFraming,
        _span: Span,
    ) -> Result<Value, Control> {
        use crate::stdlib::http1;
        let stream = match tokio::net::UnixStream::connect(socket_path).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(docker_err_pair(
                    format!("docker: connect to '{}' failed: {}", socket_path, e),
                    None,
                ))
            }
        };
        let mut headers: Vec<(String, String)> = Vec::new();
        if body.is_some() {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
        }
        let req = http1::Http1Request {
            method,
            path,
            headers,
            body,
        };
        let resp = match http1::send_request(stream, &req).await {
            Ok(r) => r,
            Err(e) => return Ok(docker_err_pair(format!("docker: {}", e), None)),
        };
        if !(200..300).contains(&resp.status) {
            // Drain the (bounded) error body so the daemon's message is surfaced.
            let buf = match resp.body {
                http1::Http1Body::Stream(r) => r
                    .read_to_end(super::MAX_ALLOC_COUNT as usize)
                    .await
                    .unwrap_or_default(),
                http1::Http1Body::Upgraded { .. } => Vec::new(),
            };
            let (msg, status) = docker_decode_error(&(resp.status, buf));
            return Ok(docker_err_pair(msg, Some(status)));
        }
        let reader = match resp.body {
            http1::Http1Body::Stream(r) => docker_byte_source(r),
            http1::Http1Body::Upgraded { .. } => {
                return Ok(docker_err_pair(
                    "docker: stream endpoint returned 101 Switching Protocols (unexpected)"
                        .to_string(),
                    None,
                ))
            }
        };
        let handle = self.register_resource(
            NativeKind::DockerStream,
            indexmap::IndexMap::new(),
            ResourceState::DockerStream(Box::new(DockerStreamState {
                source: reader,
                framing,
            })),
        );
        Ok(make_pair(handle, Value::nil()))
    }

    /// `d.logs(id, opts?)` — open `GET /containers/{id}/logs` as a demuxed stream.
    #[cfg(unix)]
    async fn docker_logs(
        &self,
        socket_path: &str,
        api_version: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let cid = self.docker_want_id(args, span, "docker.logs")?;
        let opts = args.get(1).cloned().unwrap_or_else(Value::nil);
        let mut q: Vec<(String, String)> = Vec::new();
        // stdout/stderr default ON (a logs call with neither yields nothing useful);
        // follow/timestamps default OFF; tail/since/until pass through.
        let stdout = !matches!(opt_bool(&opts, "stdout"), Some(false));
        let stderr = !matches!(opt_bool(&opts, "stderr"), Some(false));
        q.push(("stdout".to_string(), bool_param(stdout)));
        q.push(("stderr".to_string(), bool_param(stderr)));
        if matches!(opt_bool(&opts, "follow"), Some(true)) {
            q.push(("follow".to_string(), "true".to_string()));
        }
        if matches!(opt_bool(&opts, "timestamps"), Some(true)) {
            q.push(("timestamps".to_string(), "true".to_string()));
        }
        if let Some(s) = opt_scalar(&opts, "tail") {
            q.push(("tail".to_string(), s));
        }
        if let Some(s) = opt_scalar(&opts, "since") {
            q.push(("since".to_string(), s));
        }
        if let Some(s) = opt_scalar(&opts, "until") {
            q.push(("until".to_string(), s));
        }
        let base = format!("/v{}", api_version);
        let path = docker_with_query(format!("{base}/containers/{cid}/logs"), &q);
        self.docker_open_stream(socket_path, "GET", &path, None, StreamFraming::LogsUnresolved, span)
            .await
    }

    /// `d.events(opts?)` — open `GET /events` as a JSON-lines stream.
    #[cfg(unix)]
    async fn docker_events(
        &self,
        socket_path: &str,
        api_version: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let opts = args.first().cloned().unwrap_or_else(Value::nil);
        let mut q: Vec<(String, String)> = Vec::new();
        if let Some(s) = opt_scalar(&opts, "since") {
            q.push(("since".to_string(), s));
        }
        if let Some(s) = opt_scalar(&opts, "until") {
            q.push(("until".to_string(), s));
        }
        if let ValueKind::Object(o) = opts.kind() {
            if let Some(f) = o.get("filters") {
                q.push(("filters".to_string(), self.docker_encode_filters(&f, span)?));
            }
        }
        let base = format!("/v{}", api_version);
        let path = docker_with_query(format!("{base}/events"), &q);
        self.docker_open_stream(
            socket_path,
            "GET",
            &path,
            None,
            StreamFraming::JsonLines { is_pull: false },
            span,
        )
        .await
    }

    /// `d.pull(ref, opts?)` — `POST /images/create?fromImage=…&tag=…` as a JSON-lines
    /// progress stream (an in-stream `{"error": …}` → a terminal `[nil, err]`).
    #[cfg(unix)]
    async fn docker_pull(
        &self,
        socket_path: &str,
        api_version: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let image = self.docker_want_id(args, span, "docker.pull")?;
        // A `name:tag` ref splits into fromImage + tag; an explicit opts.tag wins.
        let opts = args.get(1).cloned().unwrap_or_else(Value::nil);
        let (from_image, tag) = match opt_scalar(&opts, "tag") {
            Some(t) => (image.clone(), Some(t)),
            None => match image.rsplit_once(':') {
                // Avoid splitting a registry-port `host:5000/img` — only treat the LAST
                // colon as a tag separator when it has no '/' after it.
                Some((name, t)) if !t.contains('/') => (name.to_string(), Some(t.to_string())),
                _ => (image.clone(), None),
            },
        };
        let mut q: Vec<(String, String)> = vec![("fromImage".to_string(), from_image)];
        if let Some(t) = tag {
            q.push(("tag".to_string(), t));
        }
        let base = format!("/v{}", api_version);
        let path = docker_with_query(format!("{base}/images/create"), &q);
        self.docker_open_stream(
            socket_path,
            "POST",
            &path,
            None,
            StreamFraming::JsonLines { is_pull: true },
            span,
        )
        .await
    }
}

/// The pinned Tier-2 deferral message for `attachStdin: true` (interactive stdin
/// attach is out of v1 scope — §4.5). A bare prefix `.contains(...)` survives any
/// future tail; do NOT reword without updating the asserting tests.
#[cfg(unix)]
const ATTACH_STDIN_DEFERRED: &str =
    "docker exec: attachStdin is not supported (interactive stdin attach is deferred) — \
     use exec without stdin or process.spawn";

impl Interp {
    /// `d.execCreate(containerId, opts)` — POST `/containers/{id}/exec` with the exec
    /// config; a 201 body's `Id` is returned as `[execId, nil]`. A non-string container
    /// id is a Tier-2 panic; a non-2xx is a `[nil, {message, statusCode}]` pair.
    #[cfg(unix)]
    async fn docker_exec_create(
        &self,
        socket_path: &str,
        api_version: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let cid = self.docker_want_id(args, span, "docker.execCreate")?;
        let opts = args.get(1).cloned().unwrap_or_else(Value::nil);
        // §4.5 attachStdin deferral guard (also rejected at create time, before any IO).
        if matches!(opt_bool(&opts, "attachStdin"), Some(true)) {
            return Err(AsError::at(ATTACH_STDIN_DEFERRED.to_string(), span).into());
        }
        let body = self.docker_exec_config_body(&opts, span)?;
        let base = format!("/v{}", api_version);
        let path = format!("{base}/containers/{cid}/exec");
        let resp = match self
            .docker_unary_raw(socket_path, "POST", &path, Some(&body), span)
            .await
        {
            Ok(r) => r,
            Err(msg) => return Ok(docker_err_pair(msg, None)),
        };
        if !(200..300).contains(&resp.0) {
            let (msg, status) = docker_decode_error(&resp);
            return Ok(docker_err_pair(msg, Some(status)));
        }
        // 201 body `{"Id":"…"}` → the exec id string.
        let text = String::from_utf8_lossy(&resp.1).into_owned();
        let parsed = crate::stdlib::json::call("parse", &[Value::str(text)], span)?;
        let exec_id = match docker_pair_value(&parsed) {
            Some(v) => match self.read_member(&v, "Id", span) {
                Ok(idv) => match idv.kind() {
                    ValueKind::Str(s) => s.to_string(),
                    _ => String::new(),
                },
                Err(_) => String::new(),
            },
            None => String::new(),
        };
        if exec_id.is_empty() {
            return Ok(docker_err_pair(
                "docker: exec create response had no Id".to_string(),
                None,
            ));
        }
        Ok(make_pair(Value::str(exec_id), Value::nil()))
    }

    /// `d.execStart(execId, opts)` — POST `/exec/{id}/start` with `Connection: Upgrade`/
    /// `Upgrade: tcp` to trigger the hijack; the `101` upgrade body becomes a
    /// `dockerStream` demuxing `leftover` then the raw transport (Multiplexed unless
    /// `tty:true`). A non-string id is a Tier-2 panic; `attachStdin:true` is the pinned
    /// deferral.
    #[cfg(unix)]
    async fn docker_exec_start(
        &self,
        socket_path: &str,
        api_version: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let exec_id = self.docker_want_id(args, span, "docker.execStart")?;
        let opts = args.get(1).cloned().unwrap_or_else(Value::nil);
        self.docker_exec_start_inner(socket_path, api_version, &exec_id, &opts, span)
            .await
    }

    /// The shared exec-start body: drive the hijack and register the `dockerStream`. Used
    /// by both `execStart` and the `exec` convenience (which already validated its id).
    #[cfg(unix)]
    async fn docker_exec_start_inner(
        &self,
        socket_path: &str,
        api_version: &str,
        exec_id: &str,
        opts: &Value,
        span: Span,
    ) -> Result<Value, Control> {
        if matches!(opt_bool(opts, "attachStdin"), Some(true)) {
            return Err(AsError::at(ATTACH_STDIN_DEFERRED.to_string(), span).into());
        }
        let tty = matches!(opt_bool(opts, "tty"), Some(true));
        let detach = matches!(opt_bool(opts, "detach"), Some(true));
        // Body: {"Detach":<detach>,"Tty":<tty>}.
        let body = format!(
            "{{\"Detach\":{},\"Tty\":{}}}",
            bool_param(detach),
            bool_param(tty)
        )
        .into_bytes();
        let base = format!("/v{}", api_version);
        let path = format!("{base}/exec/{exec_id}/start");
        use crate::stdlib::http1;
        let stream = match tokio::net::UnixStream::connect(socket_path).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(docker_err_pair(
                    format!("docker: connect to '{}' failed: {}", socket_path, e),
                    None,
                ))
            }
        };
        let req = http1::Http1Request {
            method: "POST",
            path: &path,
            // The hijack: setting Connection/Upgrade makes the codec skip its default
            // `Connection: close` and triggers the `101 Switching Protocols` arm.
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Connection".to_string(), "Upgrade".to_string()),
                ("Upgrade".to_string(), "tcp".to_string()),
            ],
            body: Some(&body),
        };
        let resp = match http1::send_request(stream, &req).await {
            Ok(r) => r,
            Err(e) => return Ok(docker_err_pair(format!("docker: {}", e), None)),
        };
        // A non-101 (e.g. a 4xx error head) is a Tier-1 error pair.
        if resp.status != 101 {
            let buf = match resp.body {
                http1::Http1Body::Stream(r) => r
                    .read_to_end(super::MAX_ALLOC_COUNT as usize)
                    .await
                    .unwrap_or_default(),
                http1::Http1Body::Upgraded { .. } => Vec::new(),
            };
            let (msg, status) = docker_decode_error(&(resp.status, buf));
            return Ok(docker_err_pair(msg, Some(status)));
        }
        let (transport, leftover) = match resp.body {
            http1::Http1Body::Upgraded {
                transport,
                leftover,
            } => (transport, leftover),
            http1::Http1Body::Stream(_) => {
                return Ok(docker_err_pair(
                    "docker: exec start did not upgrade (no 101 hijack)".to_string(),
                    None,
                ))
            }
        };
        // Chain `leftover` ++ the raw transport into the demux byte source, then register
        // a dockerStream over it (TTY framing if requested, else multiplexed demux).
        let source = upgraded_byte_source(transport, leftover);
        let framing = if tty {
            StreamFraming::Tty
        } else {
            StreamFraming::Multiplexed
        };
        let handle = self.register_resource(
            NativeKind::DockerStream,
            indexmap::IndexMap::new(),
            ResourceState::DockerStream(Box::new(DockerStreamState { source, framing })),
        );
        Ok(make_pair(handle, Value::nil()))
    }

    /// `d.execInspect(execId)` — GET `/exec/{id}/json` → the exec status object.
    #[cfg(unix)]
    async fn docker_exec_inspect(
        &self,
        socket_path: &str,
        api_version: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let exec_id = self.docker_want_id(args, span, "docker.execInspect")?;
        let base = format!("/v{}", api_version);
        let path = format!("{base}/exec/{exec_id}/json");
        let resp = match self
            .docker_unary_raw(socket_path, "GET", &path, None, span)
            .await
        {
            Ok(r) => r,
            Err(msg) => return Ok(docker_err_pair(msg, None)),
        };
        self.docker_map_response(resp, span)
    }

    /// `d.exec(containerId, opts)` — the convenience composition (§4.5): execCreate →
    /// execStart → DRAIN the stream collecting stdout/stderr → execInspect for the exit
    /// code → `{exitCode, code, stdout, stderr}` (mirrors `process.run`'s shape, with a
    /// `code` alias). A failure at any stage short-circuits to that stage's `[nil, err]`.
    #[cfg(unix)]
    async fn docker_exec(
        &self,
        socket_path: &str,
        api_version: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let cid = self.docker_want_id(args, span, "docker.exec")?;
        let opts = args.get(1).cloned().unwrap_or_else(Value::nil);
        // attachStdin is rejected up-front (the deferral guard, before any IO).
        if matches!(opt_bool(&opts, "attachStdin"), Some(true)) {
            return Err(AsError::at(ATTACH_STDIN_DEFERRED.to_string(), span).into());
        }
        let tty = matches!(opt_bool(&opts, "tty"), Some(true));

        // 1. Create the exec.
        let created = self
            .docker_exec_create(socket_path, api_version, &[Value::str(cid), opts.clone()], span)
            .await?;
        let exec_id = match docker_pair_ok_str(&created) {
            Ok(s) => s,
            Err(pair) => return Ok(pair), // surface the create [nil, err]
        };

        // 2. Start the exec (hijack) → a dockerStream id.
        let started = self
            .docker_exec_start_inner(socket_path, api_version, &exec_id, &opts, span)
            .await?;
        let stream_id = match docker_pair_native_id(&started) {
            Some(id) => id,
            None => return Ok(started), // a start [nil, err]
        };

        // 3. Drain the stream, accumulating per-stream text.
        let mut stdout = String::new();
        let mut stderr = String::new();
        loop {
            // Take the state OUT across the read await (never hold the table borrow).
            let mut state = match self.take_resource(stream_id) {
                Some(ResourceState::DockerStream(s)) => s,
                other => {
                    if let Some(o) = other {
                        self.return_resource(stream_id, o);
                    }
                    break;
                }
            };
            let outcome = demux_next(&mut state.source, &mut state.framing).await;
            match outcome {
                Ok(DockerItem::Item(v)) => {
                    self.return_resource(stream_id, ResourceState::DockerStream(state));
                    let (s, t) = item_stream_text(&v, tty);
                    if s == "stderr" {
                        stderr.push_str(&t);
                    } else {
                        stdout.push_str(&t);
                    }
                }
                Ok(DockerItem::Err(e)) => {
                    // Terminal stream error — drop the connection, surface the pair.
                    return Ok(make_pair(Value::nil(), e));
                }
                Ok(DockerItem::End) => break, // drop the connection (state not returned)
                Err(io) => {
                    return Ok(make_pair(
                        Value::nil(),
                        stream_err(&format!("docker: stream read failed: {}", io)),
                    ))
                }
            }
        }

        // 4. Inspect for the exit code.
        let inspected = self
            .docker_exec_inspect(socket_path, api_version, &[Value::str(exec_id)], span)
            .await?;
        let exit_code = match docker_pair_value_ok(&inspected) {
            Ok(v) => match self.read_member(&v, "ExitCode", span) {
                Ok(c) => match c.into_kind() {
                    crate::value::OwnedKind::Int(i) => i,
                    crate::value::OwnedKind::Float(f) => f as i64,
                    _ => 0,
                },
                Err(_) => 0,
            },
            Err(pair) => return Ok(pair), // an inspect [nil, err]
        };

        let mut result = indexmap::IndexMap::new();
        result.insert("exitCode".to_string(), Value::int(exit_code));
        result.insert("code".to_string(), Value::int(exit_code));
        result.insert("stdout".to_string(), Value::str(stdout));
        result.insert("stderr".to_string(), Value::str(stderr));
        Ok(make_pair(Value::object(result), Value::nil()))
    }

    /// Build the exec-create JSON body from the opts object. Maps the §4.5 fields
    /// (`cmd`→`Cmd`, `env`→`Env`, `workingDir`→`WorkingDir`, `user`→`User`,
    /// `attachStdout`/`attachStderr`/`tty`) onto the Engine-API config; an absent
    /// `attachStdout`/`attachStderr` defaults to ON (an exec with neither attached
    /// yields no output). REUSES `std/json`'s encoder.
    #[cfg(unix)]
    fn docker_exec_config_body(&self, opts: &Value, span: Span) -> Result<Vec<u8>, Control> {
        let mut cfg = indexmap::IndexMap::new();
        if let ValueKind::Object(o) = opts.kind() {
            if let Some(cmd) = o.get("cmd") {
                cfg.insert("Cmd".to_string(), cmd.clone());
            }
            if let Some(env) = o.get("env") {
                cfg.insert("Env".to_string(), env.clone());
            }
            if let Some(wd) = o.get("workingDir") {
                cfg.insert("WorkingDir".to_string(), wd.clone());
            }
            if let Some(user) = o.get("user") {
                cfg.insert("User".to_string(), user.clone());
            }
        }
        let attach_stdout = !matches!(opt_bool(opts, "attachStdout"), Some(false));
        let attach_stderr = !matches!(opt_bool(opts, "attachStderr"), Some(false));
        let tty = matches!(opt_bool(opts, "tty"), Some(true));
        cfg.insert("AttachStdout".to_string(), Value::bool_(attach_stdout));
        cfg.insert("AttachStderr".to_string(), Value::bool_(attach_stderr));
        cfg.insert("Tty".to_string(), Value::bool_(tty));
        self.docker_json_body(&Value::object(cfg), span, "docker.execCreate")
    }
}

/// Read `opts.<key>` as a bool, or `None` if absent / not a bool.
#[cfg(unix)]
fn opt_bool(opts: &Value, key: &str) -> Option<bool> {
    if let ValueKind::Object(o) = opts.kind() {
        if let Some(v) = o.get(key) {
            if let crate::value::OwnedKind::Bool(b) = v.into_kind() {
                return Some(b);
            }
        }
    }
    None
}

/// Read `opts.<key>` as a query-string scalar (string/int/float), or `None`.
#[cfg(unix)]
fn opt_scalar(opts: &Value, key: &str) -> Option<String> {
    if let ValueKind::Object(o) = opts.kind() {
        if let Some(v) = o.get(key) {
            return match v.kind() {
                ValueKind::Str(s) => Some(s.to_string()),
                ValueKind::Int(_) | ValueKind::Float(_) => Some(v.to_string()),
                _ => None,
            };
        }
    }
    None
}

/// A boolean query-param value (`"true"`/`"false"` — Docker expects the literal).
#[cfg(unix)]
fn bool_param(b: bool) -> String {
    if b { "true" } else { "false" }.to_string()
}

/// Build a `DockerByteSource` over a `101`-upgraded transport: the `leftover` bytes
/// (already read past the head) are emitted FIRST, then the raw `UnixStream` is read to
/// EOF. The result is the EXACT `DockerByteSource` type the §4.3 demux drives, so the
/// upgrade hijack reuses the multiplex/TTY framing verbatim.
#[cfg(unix)]
fn upgraded_byte_source(
    transport: tokio::net::UnixStream,
    leftover: Vec<u8>,
) -> DockerByteSource {
    use tokio_util::bytes::Bytes;
    // State: (optional leftover to emit once, then the live transport).
    let init = (Some(leftover), transport);
    let stream = futures_util::stream::unfold(init, |(pending, mut transport)| async move {
        // Emit the leftover bytes as the first chunk (only once, and only if non-empty).
        if let Some(lo) = pending {
            if !lo.is_empty() {
                return Some((Ok(Bytes::from(lo)), (None, transport)));
            }
        }
        // Then read frames off the raw transport until EOF.
        let mut buf = [0u8; 16 * 1024];
        match transport.read(&mut buf).await {
            Ok(0) => None, // EOF — end of stream
            Ok(n) => Some((Ok(Bytes::copy_from_slice(&buf[..n])), (None, transport))),
            Err(e) => Some((Err(e), (None, transport))),
        }
    });
    let byte_stream: crate::stdlib::net_http::ByteStream = Box::pin(stream);
    tokio::io::BufReader::new(tokio_util::io::StreamReader::new(byte_stream))
}

/// Read the `{stream, text}` fields of a demuxed logs item; a TTY raw item is all
/// stdout. Returns `(stream_name, text)`.
#[cfg(unix)]
fn item_stream_text(v: &Value, tty: bool) -> (String, String) {
    if let ValueKind::Object(o) = v.kind() {
        let s = if tty {
            "stdout".to_string()
        } else {
            o.get("stream")
                .map(|x| x.to_string())
                .unwrap_or_else(|| "stdout".to_string())
        };
        let t = o.get("text").map(|x| x.to_string()).unwrap_or_default();
        return (s, t);
    }
    ("stdout".to_string(), v.to_string())
}

/// For a `[value, err]` pair: `Ok(string)` if err is nil and value is a string; else
/// `Err(the original pair)` so the caller can surface the error pair unchanged.
#[cfg(unix)]
fn docker_pair_ok_str(pair: &Value) -> Result<String, Value> {
    if docker_pair_err(pair) == Some(false) {
        if let Some(v) = docker_pair_value(pair) {
            if let ValueKind::Str(s) = v.kind() {
                return Ok(s.to_string());
            }
        }
    }
    Err(pair.clone())
}

/// For a `[value, err]` pair: `Ok(value)` if err is nil; else `Err(the original pair)`.
#[cfg(unix)]
fn docker_pair_value_ok(pair: &Value) -> Result<Value, Value> {
    if docker_pair_err(pair) == Some(false) {
        if let Some(v) = docker_pair_value(pair) {
            return Ok(v);
        }
    }
    Err(pair.clone())
}

/// For a `[value, err]` pair whose value is a `Value::native` handle: its resource id.
#[cfg(unix)]
fn docker_pair_native_id(pair: &Value) -> Option<u64> {
    if docker_pair_err(pair) == Some(false) {
        if let Some(v) = docker_pair_value(pair) {
            if let ValueKind::Native(n) = v.kind() {
                return Some(n.id);
            }
        }
    }
    None
}

/// Extract the `value` (index 0) from a `[value, err]` pair Value.
#[cfg(unix)]
fn docker_pair_value(pair: &Value) -> Option<Value> {
    if let ValueKind::Array(a) = pair.kind() {
        let b = a.borrow();
        if b.len() == 2 {
            return Some(b[0].clone());
        }
    }
    None
}

/// Spec §4.2 return-shape conformance for the two endpoints whose Engine-API body
/// shape differs from the documented AScript return. `ping` → `[true, nil]` (a 2xx
/// `/_ping` means the daemon is alive — the script gets a boolean, not the raw `"OK"`
/// text the daemon sends). `wait` → `[StatusCode_int, nil]` (unwrap the daemon's
/// `{"StatusCode":N}` object to the bare exit code the spec table promises).
///
/// Every other method returns its decoded body unchanged. An ERROR pair (index-1
/// non-nil) passes through untouched (so a non-2xx `ping`/`wait` keeps its err).
#[cfg(unix)]
fn docker_postprocess_unary(name: &str, mapped: Value) -> Value {
    // Only transform a SUCCESS pair `[value, nil]`; an err pair passes through.
    if docker_pair_err(&mapped) != Some(false) {
        return mapped;
    }
    match name {
        "ping" => make_pair(Value::bool_(true), Value::nil()),
        "wait" => {
            let code = docker_pair_value(&mapped)
                .and_then(|v| match v.kind() {
                    ValueKind::Object(o) => o.get("StatusCode").and_then(|s| s.as_int()),
                    _ => None,
                })
                .unwrap_or(0);
            make_pair(Value::int(code), Value::nil())
        }
        _ => mapped,
    }
}

/// Is the `err` slot (index 1) of a `[value, err]` pair non-nil?
#[cfg(unix)]
fn docker_pair_err(pair: &Value) -> Option<bool> {
    if let ValueKind::Array(a) = pair.kind() {
        let b = a.borrow();
        if b.len() == 2 {
            return Some(!matches!(b[1].kind(), ValueKind::Nil));
        }
    }
    None
}

/// Decode the daemon's error body `{"message": "..."}` → (message, status). Falls
/// back to a synthesized message if the body is not the expected shape.
#[cfg(unix)]
fn docker_decode_error(resp: &(u16, Vec<u8>)) -> (String, u16) {
    let (status, body) = resp;
    let fallback = format!("docker: request failed with status {}", status);
    if body.is_empty() {
        return (fallback, *status);
    }
    if let Ok(jv) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(msg) = jv.get("message").and_then(|m| m.as_str()) {
            return (msg.to_string(), *status);
        }
    }
    (fallback, *status)
}

/// Append `pairs` as a `?k=v&…` query string to `path` (URL-encoding each value).
#[cfg(unix)]
fn docker_with_query(mut path: String, pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return path;
    }
    path.push('?');
    let qs: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", docker_url_encode(k), docker_url_encode(v)))
        .collect();
    path.push_str(&qs.join("&"));
    path
}

/// Minimal percent-encoding for query values (the `filters` JSON needs `{`/`}`/`"`
/// escaped). Unreserved chars pass through; everything else is `%XX`.
#[cfg(unix)]
fn docker_url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[test]
    #[cfg(unix)]
    fn version_parse_and_clamp() {
        assert_eq!(parse_version("1.43"), Some((1, 43)));
        assert_eq!(parse_version("1.20"), Some((1, 20)));
        assert_eq!(parse_version("garbage"), None);
        // Clamp into [1.24, 1.43].
        assert_eq!(clamp_version((1, 50)), "1.43");
        assert_eq!(clamp_version((1, 30)), "1.30");
        assert_eq!(clamp_version((1, 24)), "1.24");
    }

    #[test]
    #[cfg(unix)]
    fn query_string_encoding() {
        assert_eq!(docker_with_query("/x".to_string(), &[]), "/x");
        let q = docker_with_query(
            "/containers/json".to_string(),
            &[("all".to_string(), "true".to_string())],
        );
        assert_eq!(q, "/containers/json?all=true");
        // A JSON filters value gets its braces/quotes percent-encoded.
        let q2 = docker_with_query(
            "/containers/json".to_string(),
            &[("filters".to_string(), r#"{"status":["running"]}"#.to_string())],
        );
        assert!(q2.contains("filters=%7B"), "got {q2}");
    }

    #[test]
    #[cfg(unix)]
    fn error_decode_extracts_message() {
        let body = br#"{"message":"No such container: xyz"}"#.to_vec();
        let (msg, status) = docker_decode_error(&(404, body));
        assert_eq!(msg, "No such container: xyz");
        assert_eq!(status, 404);
        // A non-JSON body falls back to a synthesized message.
        let (msg2, _) = docker_decode_error(&(500, b"oops".to_vec()));
        assert!(msg2.contains("500"), "got {msg2}");
    }

    // ── CNTR §4.4 demux unit tests (no daemon — drive demux_next over fixture bytes) ──

    #[cfg(unix)]
    use std::pin::Pin;
    #[cfg(unix)]
    use std::task::{Context, Poll};
    #[cfg(unix)]
    use tokio::io::ReadBuf;

    /// Build one 8-byte-framed multiplex frame: `[type,0,0,0, size_be_u32]` + payload.
    #[cfg(unix)]
    fn mux_frame(stream_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![stream_type, 0, 0, 0];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Read the `stream`/`text` fields out of a `{stream,text}` logs item.
    #[cfg(unix)]
    fn logs_fields(v: &Value) -> (String, String) {
        if let ValueKind::Object(o) = v.kind() {
            let s = o.get("stream").map(|x| x.to_string()).unwrap_or_default();
            let t = o.get("text").map(|x| x.to_string()).unwrap_or_default();
            return (s, t);
        }
        panic!("not a logs item: {v:?}");
    }

    /// An `AsyncRead` that yields its bytes ONE AT A TIME (each `poll_read` returns at
    /// most one byte), proving the demux reassembles a frame split across reads.
    #[cfg(unix)]
    struct ByteDripReader {
        data: Vec<u8>,
        pos: usize,
    }
    #[cfg(unix)]
    impl AsyncRead for ByteDripReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.pos < self.data.len() && buf.remaining() > 0 {
                let b = self.data[self.pos];
                self.pos += 1;
                buf.put_slice(&[b]);
            }
            Poll::Ready(Ok(()))
        }
    }

    /// Drive `demux_next` to completion, collecting items until End or Err.
    #[cfg(unix)]
    async fn drain<R: AsyncRead + AsyncBufRead + Unpin>(
        reader: &mut R,
        mut framing: StreamFraming,
    ) -> (Vec<Value>, Option<Value>, bool) {
        let mut items = Vec::new();
        loop {
            match demux_next(reader, &mut framing).await.unwrap() {
                DockerItem::Item(v) => items.push(v),
                DockerItem::Err(e) => return (items, Some(e), false),
                DockerItem::End => return (items, None, true),
            }
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn demux_multiplexed_stdout_stderr() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&mux_frame(1, b"hello\n")); // stdout
        bytes.extend_from_slice(&mux_frame(2, b"oops\n")); // stderr
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(bytes));
        let (items, err, ended) = drain(&mut r, StreamFraming::LogsUnresolved).await;
        assert!(err.is_none() && ended, "clean end expected");
        assert_eq!(items.len(), 2);
        assert_eq!(logs_fields(&items[0]), ("stdout".into(), "hello\n".into()));
        assert_eq!(logs_fields(&items[1]), ("stderr".into(), "oops\n".into()));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn demux_tty_autodetect_one_stdout_item() {
        // Raw text: byte[0]='p' (0x70) ∉ {0,1,2} → TTY auto-detection.
        let bytes = b"plain tty output\n".to_vec();
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(bytes));
        let (items, err, ended) = drain(&mut r, StreamFraming::LogsUnresolved).await;
        assert!(err.is_none() && ended);
        // First item is the 8 header bytes already read, then the rest as a chunk.
        let joined: String = items
            .iter()
            .map(|v| logs_fields(v).1)
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(joined, "plain tty output\n");
        assert!(items.iter().all(|v| logs_fields(v).0 == "stdout"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn demux_frame_split_one_byte_at_a_time_reassembles() {
        // Same multiplexed bytes, but delivered ONE byte per read — the demux must
        // buffer partial header/payload and complete the frame on later bytes.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&mux_frame(1, b"hello\n"));
        bytes.extend_from_slice(&mux_frame(2, b"oops\n"));
        let drip = ByteDripReader { data: bytes, pos: 0 };
        let mut r = tokio::io::BufReader::new(drip);
        let (items, err, ended) = drain(&mut r, StreamFraming::LogsUnresolved).await;
        assert!(err.is_none() && ended);
        assert_eq!(items.len(), 2);
        assert_eq!(logs_fields(&items[0]), ("stdout".into(), "hello\n".into()));
        assert_eq!(logs_fields(&items[1]), ("stderr".into(), "oops\n".into()));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn demux_truncated_mid_payload_is_tier1_err() {
        // Header claims 20 bytes; only 4 follow then EOF.
        let mut bytes = vec![1u8, 0, 0, 0];
        bytes.extend_from_slice(&20u32.to_be_bytes());
        bytes.extend_from_slice(b"abc!"); // 4 of 20
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(bytes));
        let (_items, err, ended) = drain(&mut r, StreamFraming::Multiplexed).await;
        assert!(!ended, "must not be a clean end");
        let e = err.expect("a truncated frame is a Tier-1 err");
        assert!(
            error_message_contains(&e, "truncated"),
            "expected truncation err, got {e:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn demux_truncated_mid_header_is_tier1_err() {
        // 3 header bytes then EOF (≥1 but < 8).
        let bytes = vec![1u8, 0, 0];
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(bytes));
        let (_items, err, ended) = drain(&mut r, StreamFraming::Multiplexed).await;
        assert!(!ended);
        assert!(err.is_some(), "mid-header EOF is a Tier-1 err");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn demux_oversize_size_is_err_without_allocating() {
        // SIZE = 16 MiB + 1, header only, no payload. Must Err on the size check alone
        // (never allocate the claimed ~16 MiB).
        let mut bytes = vec![1u8, 0, 0, 0];
        bytes.extend_from_slice(&(MAX_FRAME_SIZE + 1).to_be_bytes());
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(bytes));
        let (_items, err, ended) = drain(&mut r, StreamFraming::Multiplexed).await;
        assert!(!ended);
        let e = err.expect("oversize frame is a Tier-1 err");
        assert!(error_message_contains(&e, "cap"), "got {e:?}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn demux_clean_eof_at_frame_boundary_is_end() {
        // One full frame then clean EOF at the next frame boundary → End, no err.
        let bytes = mux_frame(1, b"hi");
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(bytes));
        let (items, err, ended) = drain(&mut r, StreamFraming::Multiplexed).await;
        assert!(err.is_none() && ended);
        assert_eq!(items.len(), 1);
        assert_eq!(logs_fields(&items[0]).1, "hi");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn demux_jsonlines_decodes_objects_and_pull_error_is_terminal() {
        // events-style: two JSON objects then EOF.
        let body = b"{\"Action\":\"start\"}\n{\"Action\":\"stop\"}\n".to_vec();
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(body));
        let (items, err, ended) =
            drain(&mut r, StreamFraming::JsonLines { is_pull: false }).await;
        assert!(err.is_none() && ended);
        assert_eq!(items.len(), 2);

        // pull-style: a progress line then an {"error":…} line → terminal err.
        let body2 = b"{\"status\":\"Pulling\"}\n{\"error\":\"manifest unknown\"}\n".to_vec();
        let mut r2 = tokio::io::BufReader::new(std::io::Cursor::new(body2));
        let (items2, err2, ended2) =
            drain(&mut r2, StreamFraming::JsonLines { is_pull: true }).await;
        assert!(!ended2, "pull error ends the stream as an err, not a clean End");
        assert_eq!(items2.len(), 1, "the progress line, then the error terminates");
        let e = err2.expect("the error line is a terminal [nil, err]");
        assert!(error_message_contains(&e, "manifest unknown"), "got {e:?}");
    }

    /// True if the `{message}` field of a stream-err value contains `needle`.
    #[cfg(unix)]
    fn error_message_contains(v: &Value, needle: &str) -> bool {
        if let ValueKind::Object(o) = v.kind() {
            if let Some(m) = o.get("message") {
                return m.to_string().contains(needle);
            }
        }
        false
    }
}
