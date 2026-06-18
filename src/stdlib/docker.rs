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
        self.docker_map_response(resp, span)
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
}
