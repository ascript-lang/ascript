//! `std/net/ws` — WebSocket client + server native handles (feature `net`), spec §11.2.
//!
//! Built on `tokio-tungstenite` so it rides the §7 event loop. Module entry points:
//!
//! - `connect(url, opts?) -> [conn, err]` — async; opens a client WebSocket to a
//!   `ws://` or `wss://` URL. (`wss://` rides tokio-tungstenite's rustls support.)
//!   `opts.headers` (an object of string→string) and `opts.auth` (`{bearer: tok}` →
//!   `Authorization: Bearer tok`; `{basic: [user, pass]}` → `Authorization: Basic
//!   <base64(user:pass)>`) are applied to the handshake request — mirroring the
//!   `std/net/http` client's `headers`/`auth` opts for consistency.
//! - `listen(host, port) -> [wsListener, err]` — binds a `TcpListener`. The handle's
//!   `fields` carry the bound `port` (so `listen("127.0.0.1", 0)` is usable: read the
//!   OS-assigned port off `wsListener.port`).
//!
//! ## Server shape — accept-based (documented)
//!
//! The server is *accept-based*, mirroring `std/net/tcp` and matching the
//! single-threaded interpreter model (rather than a `listen(host, port, handler)`
//! callback form):
//!
//! ```text
//! let [server, err] = listen("127.0.0.1", 0)   // binds a TcpListener
//! let port = server.port                        // OS-assigned when binding port 0
//! let [conn, aerr] = await server.accept()      // TCP accept + WS handshake → conn
//! ```
//!
//! `accept()` performs the TCP accept and the WebSocket handshake
//! (`tokio_tungstenite::accept_async`) and returns a `WsConnection` — the *same*
//! handle kind a client `connect` returns, so `send`/`recv`/`close` are identical on
//! both ends.
//!
//! ## Connection
//!
//! A `WsConnection` is message-oriented:
//! - `send(data) -> [nil, err]` — a string sends a Text frame; bytes send a Binary frame.
//! - `recv() -> [message, err]` — async; a Text frame decodes to a string, a Binary
//!   frame to bytes, and a Close frame (or transport EOF) yields `nil` AND finalizes
//!   the connection (so a subsequent `recv()` keeps returning `nil` without panicking).
//!   Control frames (Ping/Pong) are handled transparently by tungstenite and skipped.
//! - `close() -> [nil, err]` — sends a Close frame and finalizes the handle.
//!
//! Like the M13/M14 readers, the connection finalizes itself (`take_resource`) on
//! close / on a received Close frame, so no fd leaks and use-after-close degrades
//! gracefully (recv → nil; send → Tier-1 err).
//!
//! ## Stream-type unification
//!
//! A client connection is a `WebSocketStream<MaybeTlsStream<TcpStream>>` while a
//! server-accepted one is a `WebSocketStream<TcpStream>` — different concrete types.
//! We unify them behind a single boxed trait object: a `WsStream` trait blanket-impl'd
//! for anything that is both a `Stream<Item = Result<Message, _>>` and a
//! `Sink<Message>` (which every `WebSocketStream<T>` is), so `send`/`recv` dispatch is
//! identical regardless of origin.

use super::{arg, bi, want_array, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value, ValueKind};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use std::rc::Rc;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::ClientRequestBuilder;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;

/// A unified WebSocket stream: every `tokio_tungstenite::WebSocketStream<T>` (client
/// `MaybeTlsStream<TcpStream>` or server `TcpStream`) implements both halves, so a
/// single boxed `dyn WsStream` carries either origin behind one type.
pub trait WsStream:
    futures_util::Stream<Item = Result<Message, WsError>>
    + futures_util::Sink<Message, Error = WsError>
    + Unpin
{
}

impl<T> WsStream for T where
    T: futures_util::Stream<Item = Result<Message, WsError>>
        + futures_util::Sink<Message, Error = WsError>
        + Unpin
{
}

/// The live, unified WebSocket connection stored in the resource table.
pub struct WsConnState {
    inner: Box<dyn WsStream>,
}

impl WsConnState {
    pub fn new<S: WsStream + 'static>(stream: S) -> Self {
        WsConnState {
            inner: Box::new(stream),
        }
    }
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("connect", bi("net_ws.connect")),
        ("listen", bi("net_ws.listen")),
    ]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg)))
}

fn bytes_value(b: Vec<u8>) -> Value {
    Value::bytes(b)
}

/// Parse `connect`'s optional `opts` into a flat list of handshake request headers.
///
/// `opts.headers` is an object of string→string; `opts.auth` is `{bearer: tok}` (→
/// `Authorization: Bearer tok`) or `{basic: [user, pass?]}` (→ `Authorization: Basic
/// <base64(user:pass)>`). nil/absent opts → no headers. Mirrors the `std/net/http`
/// client's `headers`/`auth` shape; misuse (non-object opts/headers/auth, bad auth
/// shape) is a Tier-2 panic.
fn ws_connect_headers(opts: &Value, span: Span) -> Result<Vec<(String, String)>, Control> {
    let mut out: Vec<(String, String)> = Vec::new();
    let obj = match opts.kind() {
        ValueKind::Nil => return Ok(out),
        ValueKind::Object(o) => o,
        _ => {
            return Err(AsError::at(
                format!(
                    "net/ws.connect opts expects an object, got {}",
                    crate::interp::type_name(opts)
                ),
                span,
            )
            .into());
        }
    };
    // headers: object of string→string.
    if let Some(h) = obj.get("headers") {
        let map = match h.kind() {
            ValueKind::Object(o) => o,
            _ => {
                return Err(AsError::at(
                    format!(
                        "net/ws.connect headers expects an object, got {}",
                        crate::interp::type_name(&h)
                    ),
                    span,
                )
                .into());
            }
        };
        for (k, v) in map.entries() {
            let vs = want_string(&v, span, "net/ws.connect header value")?;
            out.push((k.to_string(), vs.to_string()));
        }
    }

    // auth: {bearer: tok} or {basic: [user, pass?]}.
    if let Some(a) = obj.get("auth") {
        let ao = match a.kind() {
            ValueKind::Object(o) => o,
            _ => {
                return Err(AsError::at(
                    format!(
                        "net/ws.connect auth expects an object, got {}",
                        crate::interp::type_name(&a)
                    ),
                    span,
                )
                .into());
            }
        };
        if let Some(tok) = ao.get("bearer") {
            let tok = want_string(&tok, span, "net/ws.connect auth.bearer")?;
            out.push(("Authorization".to_string(), format!("Bearer {}", tok)));
        } else if let Some(basic) = ao.get("basic") {
            let arr = want_array(&basic, span, "net/ws.connect auth.basic")?;
            let arr = arr.borrow();
            let user = want_string(
                arr.first().unwrap_or(&Value::nil()),
                span,
                "net/ws.connect auth.basic[0]",
            )?;
            let pass = match arr.get(1) {
                None => String::new(),
                Some(p) if matches!(p.kind(), ValueKind::Nil) => String::new(),
                Some(p) => want_string(p, span, "net/ws.connect auth.basic[1]")?.to_string(),
            };
            let creds =
                base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", user, pass));
            out.push(("Authorization".to_string(), format!("Basic {}", creds)));
        } else {
            return Err(AsError::at(
                "net/ws.connect auth expects {bearer} or {basic:[user,pass]}",
                span,
            )
            .into());
        }
    }

    Ok(out)
}

impl Interp {
    /// Module-level dispatch for `std/net/ws` (`connect`/`listen`).
    pub(crate) async fn call_net_ws(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "connect" => self.ws_connect(args, span).await,
            "listen" => self.ws_listen(args, span).await,
            _ => Err(AsError::at(format!("std/net/ws has no function '{}'", func), span).into()),
        }
    }

    async fn ws_connect(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let url = want_string(&arg(args, 0), span, "net/ws.connect url")?;
        // FFI §4.4 stage-2 (net carve-out, BLOCKER 1): re-check the resolved host
        // BEFORE the handshake. Gate-12: no carve-out → immediate `Ok`.
        if let Some(host) = crate::stdlib::caps::host_of_url(&url) {
            self.check_net_host(&host, span)?;
        }
        // opts.headers / opts.auth → handshake request headers (Tier-2 on misuse,
        // consistent with the std/net/http client). nil/absent opts → no headers.
        let headers = ws_connect_headers(&arg(args, 1), span)?;

        let result = if headers.is_empty() {
            // No custom headers: connect straight from the URL string.
            tokio_tungstenite::connect_async(url.as_ref()).await
        } else {
            // Custom headers require a ClientRequestBuilder, which is built from a
            // parsed `http::Uri`. A malformed URL surfaces as a Tier-1 connect err.
            let uri: Uri = match url.parse() {
                Ok(u) => u,
                Err(e) => {
                    return Ok(err_pair(format!(
                        "net/ws.connect invalid url {}: {}",
                        url, e
                    )));
                }
            };
            let mut builder = ClientRequestBuilder::new(uri);
            for (k, v) in headers {
                builder = builder.with_header(k, v);
            }
            tokio_tungstenite::connect_async(builder).await
        };

        match result {
            Ok((stream, _resp)) => {
                let handle = self.register_resource(
                    NativeKind::WsConnection,
                    indexmap::IndexMap::new(),
                    ResourceState::WsConnection(WsConnState::new(stream)),
                );
                Ok(make_pair(handle, Value::nil()))
            }
            Err(e) => Ok(err_pair(format!("net/ws.connect to {} failed: {}", url, e))),
        }
    }

    async fn ws_listen(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let host = want_string(&arg(args, 0), span, "net/ws.listen host")?;
        let port = super::want_number(&arg(args, 1), span, "net/ws.listen port")?;
        if !(0.0..=65535.0).contains(&port) || port.fract() != 0.0 {
            return Err(
                AsError::at("net/ws.listen port must be an integer 0..=65535", span).into(),
            );
        }
        // FFI §4.4 stage-2 (net carve-out, BLOCKER 1): re-check the bind host.
        // Gate-12: no carve-out → immediate `Ok` with no comparison.
        self.check_net_host(&host, span)?;
        let addr = format!("{}:{}", host, port as u16);
        match TcpListener::bind(&addr).await {
            Ok(listener) => {
                let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                let mut fields = indexmap::IndexMap::new();
                // NUM §4: a port is an `Int`.
                fields.insert("port".to_string(), Value::int(i64::from(bound)));
                let handle = self.register_resource(
                    NativeKind::WsListener,
                    fields,
                    ResourceState::WsListener(listener),
                );
                Ok(make_pair(handle, Value::nil()))
            }
            Err(e) => Ok(err_pair(format!("net/ws.listen on {} failed: {}", addr, e))),
        }
    }

    /// Dispatch a method on a WebSocket connection / listener handle.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_ws_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.receiver.kind {
            NativeKind::WsConnection => self.ws_conn_method(id, &m.method, &args, span).await,
            NativeKind::WsListener => self.ws_listener_method(id, &m.method, &args, span).await,
            _ => {
                Err(AsError::at(format!("native handle has no method '{}'", m.method), span).into())
            }
        }
    }

    async fn ws_conn_method(
        &self,
        id: u64,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "send" => {
                let send_arg = arg(args, 0);
                let msg = match send_arg.kind() {
                    ValueKind::Str(s) => Message::Text(s.to_string()),
                    ValueKind::Bytes(b) => Message::Binary(b.borrow().clone()),
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "ws.send expects a string or bytes, got {}",
                                crate::interp::type_name(&send_arg)
                            ),
                            span,
                        )
                        .into());
                    }
                };
                // Take the connection OUT so no table borrow is held across the await.
                let mut conn = match self.take_resource(id) {
                    Some(ResourceState::WsConnection(c)) => c,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(err_pair("ws.send: connection is closed".to_string()));
                    }
                };
                let r = conn.inner.send(msg).await;
                self.return_resource(id, ResourceState::WsConnection(conn));
                match r {
                    Ok(()) => Ok(make_pair(Value::nil(), Value::nil())),
                    Err(e) => Ok(err_pair(format!("ws.send failed: {}", e))),
                }
            }
            "recv" => {
                // Take the connection OUT so no table borrow is held across the await.
                // A closed/finalized connection degrades to nil rather than panicking.
                let mut conn = match self.take_resource(id) {
                    Some(ResourceState::WsConnection(c)) => c,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(make_pair(Value::nil(), Value::nil()));
                    }
                };
                loop {
                    match conn.inner.next().await {
                        Some(Ok(Message::Text(s))) => {
                            self.return_resource(id, ResourceState::WsConnection(conn));
                            return Ok(make_pair(Value::str(s), Value::nil()));
                        }
                        Some(Ok(Message::Binary(b))) => {
                            self.return_resource(id, ResourceState::WsConnection(conn));
                            return Ok(make_pair(bytes_value(b), Value::nil()));
                        }
                        // Control frames are handled by tungstenite (it auto-replies to
                        // Ping); skip them and keep reading for an application message.
                        Some(Ok(Message::Ping(_)))
                        | Some(Ok(Message::Pong(_)))
                        | Some(Ok(Message::Frame(_))) => continue,
                        // Peer sent a Close frame, or the stream ended: drop the conn
                        // and report end-of-connection as nil.
                        Some(Ok(Message::Close(_))) | None => {
                            return Ok(make_pair(Value::nil(), Value::nil()));
                        }
                        Some(Err(e)) => {
                            // A transport-level reset after the peer is gone is an EOF
                            // for our purposes: drop the conn and surface nil, not a
                            // Tier-1 err (matches the tcp reader's EOF-as-nil contract).
                            if matches!(e, WsError::ConnectionClosed | WsError::AlreadyClosed) {
                                return Ok(make_pair(Value::nil(), Value::nil()));
                            }
                            return Ok(err_pair(format!("ws.recv failed: {}", e)));
                        }
                    }
                }
            }
            "close" => {
                // Send a Close frame (best-effort), then drop the handle.
                if let Some(ResourceState::WsConnection(mut conn)) = self.take_resource(id) {
                    let _ = conn.inner.close().await;
                }
                Ok(make_pair(Value::nil(), Value::nil()))
            }
            other => {
                Err(AsError::at(format!("wsConnection has no method '{}'", other), span).into())
            }
        }
    }

    async fn ws_listener_method(
        &self,
        id: u64,
        method: &str,
        _args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "accept" => {
                let listener = match self.take_resource(id) {
                    Some(ResourceState::WsListener(l)) => l,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(err_pair(
                            "ws listener.accept: listener is closed".to_string(),
                        ));
                    }
                };
                let accepted = listener.accept().await;
                // The listener keeps accepting: put it back before the handshake.
                self.return_resource(id, ResourceState::WsListener(listener));
                let (tcp, _peer) = match accepted {
                    Ok(pair) => pair,
                    Err(e) => return Ok(err_pair(format!("ws listener.accept failed: {}", e))),
                };
                match tokio_tungstenite::accept_async(tcp).await {
                    Ok(stream) => {
                        let handle = self.register_resource(
                            NativeKind::WsConnection,
                            indexmap::IndexMap::new(),
                            ResourceState::WsConnection(WsConnState::new(stream)),
                        );
                        Ok(make_pair(handle, Value::nil()))
                    }
                    Err(e) => Ok(err_pair(format!(
                        "ws listener.accept handshake failed: {}",
                        e
                    ))),
                }
            }
            "close" => {
                self.take_resource(id);
                Ok(Value::nil())
            }
            other => Err(AsError::at(format!("wsListener has no method '{}'", other), span).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::interp::Interp;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    /// Lex/parse/exec `src` against a caller-held `Interp` (so the resource table
    /// can be inspected afterward).
    async fn run_on(interp: &Interp, src: &str) {
        let tokens = crate::lexer::lex(src).expect("lex");
        let program = crate::parser::parse(&tokens).expect("parse");
        let env = crate::interp::global_env().child();
        interp.exec(&program, &env).await.expect("exec");
    }

    /// Run an AScript program and return its captured output.
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    /// Spawn a one-shot tungstenite WebSocket echo server on 127.0.0.1:0; return its
    /// bound port. Accepts ONE connection, echoes every Text/Binary message back, and
    /// stops on Close / stream end.
    async fn spawn_ws_echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let mut ws = tokio_tungstenite::accept_async(tcp)
                .await
                .expect("handshake");
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Text(t)) => {
                        ws.send(Message::Text(t)).await.ok();
                    }
                    Ok(Message::Binary(b)) => {
                        ws.send(Message::Binary(b)).await.ok();
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
        });
        port
    }

    /// Spawn a one-shot ws server that CAPTURES the handshake request headers (via
    /// `accept_hdr_async` + a closure `Callback`), echoes one message, and reports the
    /// captured headers over a oneshot channel. Returns `(port, header_receiver)`.
    /// Headers come back as lowercased `name: value` lines joined by '\n' for easy
    /// substring assertions.
    // The handshake `Callback` returns tungstenite's `Result<Response, ErrorResponse>`;
    // the large `Err` variant is dictated by that external trait, not our choice.
    #[allow(clippy::result_large_err)]
    async fn spawn_header_capturing_server() -> (u16, tokio::sync::oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            // The `Callback` (an FnOnce) runs synchronously during the handshake; it
            // owns `tx` and sends the captured headers directly — no shared cell, so
            // the spawned future stays `Send`.
            let callback = move |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                                 resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
                let mut s = String::new();
                for (name, value) in req.headers().iter() {
                    s.push_str(name.as_str());
                    s.push_str(": ");
                    s.push_str(value.to_str().unwrap_or(""));
                    s.push('\n');
                }
                tx.send(s).ok();
                Ok(resp)
            };
            let mut ws = tokio_tungstenite::accept_hdr_async(tcp, callback)
                .await
                .expect("handshake");
            // Echo one message so the client's round-trip completes.
            if let Some(Ok(msg)) = ws.next().await {
                ws.send(msg).await.ok();
            }
        });
        (port, rx)
    }

    #[tokio::test]
    async fn connect_sends_custom_headers_and_auth_at_handshake() {
        let (port, rx) = spawn_header_capturing_server().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, err] = connect("ws://127.0.0.1:{port}", {{
  headers: {{ "x-test": "yes" }},
  auth: {{ bearer: "tok" }}
}})
print(err)
await conn.send("hi")
let [msg, _r] = await conn.recv()
print(msg)
conn.close()
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\nhi\n");
        let headers = rx.await.expect("server should report captured headers");
        assert!(
            headers.contains("x-test: yes"),
            "missing x-test header in:\n{}",
            headers
        );
        assert!(
            headers.contains("authorization: Bearer tok"),
            "missing bearer auth header in:\n{}",
            headers
        );
    }

    #[tokio::test]
    async fn connect_basic_auth_sends_base64_authorization() {
        let (port, rx) = spawn_header_capturing_server().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, err] = connect("ws://127.0.0.1:{port}", {{
  auth: {{ basic: ["user", "pass"] }}
}})
print(err)
await conn.send("hi")
let [_m, _r] = await conn.recv()
conn.close()
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\n");
        let headers = rx.await.expect("server should report captured headers");
        // base64("user:pass") == "dXNlcjpwYXNz"
        assert!(
            headers.contains("authorization: Basic dXNlcjpwYXNz"),
            "missing/incorrect basic auth header in:\n{}",
            headers
        );
    }

    #[tokio::test]
    async fn connect_with_non_object_opts_is_tier2_panic() {
        let port = spawn_ws_echo_server().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, _e] = connect("ws://127.0.0.1:{port}", 42)
print(conn)
"#
        );
        let err = crate::run_source(&src)
            .await
            .expect_err("non-object opts must panic");
        assert!(
            err.to_string().contains("opts expects an object"),
            "unexpected error: {}",
            err
        );
    }

    #[tokio::test]
    async fn client_connect_send_recv_text_against_echo_server() {
        let port = spawn_ws_echo_server().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, err] = connect("ws://127.0.0.1:{port}")
print(err)
let [_s, serr] = await conn.send("hi")
print(serr)
let [msg, rerr] = await conn.recv()
print(rerr)
print(msg)
conn.close()
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\nnil\nnil\nhi\n");
    }

    #[tokio::test]
    async fn client_binary_frame_round_trip() {
        let port = spawn_ws_echo_server().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
import {{ fromArray, toArray }} from "std/bytes"
let [conn, _e] = connect("ws://127.0.0.1:{port}")
let payload = fromArray([1, 2, 3, 255])
await conn.send(payload)
let [msg, _r] = await conn.recv()
print(type(msg))
print(toArray(msg))
conn.close()
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "bytes\n[1, 2, 3, 255]\n");
    }

    #[tokio::test]
    async fn recv_after_close_returns_nil_without_panic() {
        let port = spawn_ws_echo_server().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, _e] = connect("ws://127.0.0.1:{port}")
conn.close()
let [a, _ae] = await conn.recv()
let [b, _be] = await conn.recv()
print(a)
print(b)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\nnil\n");
    }

    #[tokio::test]
    async fn recv_returns_nil_when_peer_closes() {
        // Peer accepts the handshake then immediately drops → client recv sees EOF.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            drop(ws); // close immediately
        });
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, _e] = connect("ws://127.0.0.1:{port}")
let [a, _ae] = await conn.recv()
print(a)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\n");
    }

    #[tokio::test]
    async fn send_after_close_returns_err() {
        let port = spawn_ws_echo_server().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, _e] = connect("ws://127.0.0.1:{port}")
conn.close()
let [_n, serr] = await conn.send("x")
print(serr != nil)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "true\n");
    }

    #[tokio::test]
    async fn connect_to_closed_port_is_err() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, err] = connect("ws://127.0.0.1:{port}")
print(conn)
print(err != nil)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn connection_resource_reclaimed_after_close() {
        let port = spawn_ws_echo_server().await;
        let interp = Interp::new();
        let baseline = interp.resource_count();
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, _e] = connect("ws://127.0.0.1:{port}")
await conn.send("hi")
let [_m, _r] = await conn.recv()
conn.close()
"#
        );
        run_on(&interp, &src).await;
        assert_eq!(
            interp.resource_count(),
            baseline,
            "connection should be reclaimed on close"
        );
    }

    #[tokio::test]
    async fn server_listen_accept_recv_echo_with_raw_client() {
        // AScript binds + accepts + echoes; a raw tungstenite CLIENT connects to
        // wsListener.port, sends a message, and verifies the echo. Uses the
        // reserve-port pattern from net_tcp so the client can retry-connect.
        let interp = Interp::new();
        let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = reserve.local_addr().unwrap().port();
        drop(reserve);

        let client = tokio::spawn(async move {
            let url = format!("ws://127.0.0.1:{}", port);
            let mut ws = loop {
                match tokio_tungstenite::connect_async(url.as_str()).await {
                    Ok((ws, _)) => break ws,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
                }
            };
            ws.send(Message::Text("ping".to_string())).await.unwrap();
            let reply = ws.next().await.unwrap().unwrap();
            ws.close(None).await.ok();
            match reply {
                Message::Text(t) => t,
                _ => panic!("expected text reply"),
            }
        });

        let src = format!(
            r#"
import {{ listen }} from "std/net/ws"
let [server, err] = listen("127.0.0.1", {port})
print(err)
print(server.port == {port})
let [conn, aerr] = await server.accept()
print(aerr)
let [msg, _r] = await conn.recv()
print(msg)
await conn.send(msg)
conn.close()
server.close()
"#
        );
        run_on(&interp, &src).await;
        let reply = client.await.unwrap();
        assert_eq!(reply, "ping");
        assert_eq!(interp.output(), "nil\ntrue\nnil\nping\n");
    }

    #[tokio::test]
    async fn listen_port_zero_assigns_real_port() {
        let out = run(r#"
import { listen } from "std/net/ws"
let [server, err] = listen("127.0.0.1", 0)
print(err)
print(server.port > 0)
server.close()
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn ws_echo_round_trip_e2e() {
        let port = spawn_ws_echo_server().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/ws"
let [conn, _e] = connect("ws://127.0.0.1:{port}")
await conn.send("one")
let [a, _ae] = await conn.recv()
await conn.send("two")
let [b, _be] = await conn.recv()
print(a)
print(b)
conn.close()
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "one\ntwo\n");
    }
}
