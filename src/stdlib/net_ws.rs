//! `std/net/ws` — WebSocket client + server native handles (feature `net`), spec §11.2.
//!
//! Built on `tokio-tungstenite` so it rides the §7 event loop. Module entry points:
//!
//! - `connect(url, opts?) -> [conn, err]` — async; opens a client WebSocket to a
//!   `ws://` or `wss://` URL. (`wss://` rides tokio-tungstenite's rustls support.)
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

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value};
use futures_util::{SinkExt, StreamExt};
use std::cell::RefCell;
use std::rc::Rc;
use tokio::net::TcpListener;
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
        WsConnState { inner: Box::new(stream) }
    }
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("connect", bi("net_ws.connect")), ("listen", bi("net_ws.listen"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

fn bytes_value(b: Vec<u8>) -> Value {
    Value::Bytes(Rc::new(RefCell::new(b)))
}

impl Interp {
    /// Module-level dispatch for `std/net/ws` (`connect`/`listen`).
    pub(crate) async fn call_net_ws(
        &mut self,
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

    async fn ws_connect(&mut self, args: &[Value], span: Span) -> Result<Value, Control> {
        let url = want_string(&arg(args, 0), span, "net/ws.connect url")?;
        match tokio_tungstenite::connect_async(url.as_ref()).await {
            Ok((stream, _resp)) => {
                let handle = self.register_resource(
                    NativeKind::WsConnection,
                    indexmap::IndexMap::new(),
                    ResourceState::WsConnection(WsConnState::new(stream)),
                );
                Ok(make_pair(handle, Value::Nil))
            }
            Err(e) => Ok(err_pair(format!("net/ws.connect to {} failed: {}", url, e))),
        }
    }

    async fn ws_listen(&mut self, args: &[Value], span: Span) -> Result<Value, Control> {
        let host = want_string(&arg(args, 0), span, "net/ws.listen host")?;
        let port = super::want_number(&arg(args, 1), span, "net/ws.listen port")?;
        if !(0.0..=65535.0).contains(&port) || port.fract() != 0.0 {
            return Err(
                AsError::at("net/ws.listen port must be an integer 0..=65535", span).into()
            );
        }
        let addr = format!("{}:{}", host, port as u16);
        match TcpListener::bind(&addr).await {
            Ok(listener) => {
                let bound = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                let mut fields = indexmap::IndexMap::new();
                fields.insert("port".to_string(), Value::Number(bound as f64));
                let handle = self.register_resource(
                    NativeKind::WsListener,
                    fields,
                    ResourceState::WsListener(listener),
                );
                Ok(make_pair(handle, Value::Nil))
            }
            Err(e) => Ok(err_pair(format!("net/ws.listen on {} failed: {}", addr, e))),
        }
    }

    /// Dispatch a method on a WebSocket connection / listener handle.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_ws_method(
        &mut self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.receiver.kind {
            NativeKind::WsConnection => self.ws_conn_method(id, &m.method, &args, span).await,
            NativeKind::WsListener => self.ws_listener_method(id, &m.method, &args, span).await,
            _ => Err(AsError::at(format!("native handle has no method '{}'", m.method), span).into()),
        }
    }

    async fn ws_conn_method(
        &mut self,
        id: u64,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "send" => {
                let msg = match &arg(args, 0) {
                    Value::Str(s) => Message::Text(s.to_string()),
                    Value::Bytes(b) => Message::Binary(b.borrow().clone()),
                    other => {
                        return Err(AsError::at(
                            format!(
                                "ws.send expects a string or bytes, got {}",
                                crate::interp::type_name(other)
                            ),
                            span,
                        )
                        .into());
                    }
                };
                let conn = match self.ws_conn_mut(id) {
                    Some(c) => c,
                    None => return Ok(err_pair("ws.send: connection is closed".to_string())),
                };
                match conn.inner.send(msg).await {
                    Ok(()) => Ok(make_pair(Value::Nil, Value::Nil)),
                    Err(e) => Ok(err_pair(format!("ws.send failed: {}", e))),
                }
            }
            "recv" => {
                // A closed/finalized connection degrades to nil rather than panicking.
                if self.ws_conn_mut(id).is_none() {
                    return Ok(make_pair(Value::Nil, Value::Nil));
                }
                loop {
                    let conn = match self.ws_conn_mut(id) {
                        Some(c) => c,
                        None => return Ok(make_pair(Value::Nil, Value::Nil)),
                    };
                    match conn.inner.next().await {
                        Some(Ok(Message::Text(s))) => {
                            return Ok(make_pair(Value::Str(s.into()), Value::Nil));
                        }
                        Some(Ok(Message::Binary(b))) => {
                            return Ok(make_pair(bytes_value(b), Value::Nil));
                        }
                        // Control frames are handled by tungstenite (it auto-replies to
                        // Ping); skip them and keep reading for an application message.
                        Some(Ok(Message::Ping(_)))
                        | Some(Ok(Message::Pong(_)))
                        | Some(Ok(Message::Frame(_))) => continue,
                        // Peer sent a Close frame, or the stream ended: finalize and
                        // report end-of-connection as nil.
                        Some(Ok(Message::Close(_))) | None => {
                            self.take_resource(id);
                            return Ok(make_pair(Value::Nil, Value::Nil));
                        }
                        Some(Err(e)) => {
                            // A transport-level reset after the peer is gone is an EOF
                            // for our purposes: finalize and surface nil, not a Tier-1
                            // err (matches the tcp reader's EOF-as-nil contract).
                            self.take_resource(id);
                            if matches!(
                                e,
                                WsError::ConnectionClosed | WsError::AlreadyClosed
                            ) {
                                return Ok(make_pair(Value::Nil, Value::Nil));
                            }
                            return Ok(err_pair(format!("ws.recv failed: {}", e)));
                        }
                    }
                }
            }
            "close" => {
                // Send a Close frame (best-effort), then finalize the handle.
                if let Some(conn) = self.ws_conn_mut(id) {
                    let _ = conn.inner.close().await;
                }
                self.take_resource(id);
                Ok(make_pair(Value::Nil, Value::Nil))
            }
            other => Err(AsError::at(format!("wsConnection has no method '{}'", other), span).into()),
        }
    }

    async fn ws_listener_method(
        &mut self,
        id: u64,
        method: &str,
        _args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "accept" => {
                let listener = match self.ws_listener_mut(id) {
                    Some(l) => l,
                    None => return Ok(err_pair("ws listener.accept: listener is closed".to_string())),
                };
                let (tcp, _peer) = match listener.accept().await {
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
                        Ok(make_pair(handle, Value::Nil))
                    }
                    Err(e) => Ok(err_pair(format!("ws listener.accept handshake failed: {}", e))),
                }
            }
            "close" => {
                self.take_resource(id);
                Ok(Value::Nil)
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
    async fn run_on(interp: &mut Interp, src: &str) {
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
            let mut ws = tokio_tungstenite::accept_async(tcp).await.expect("handshake");
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
        let mut interp = Interp::new();
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
        run_on(&mut interp, &src).await;
        assert_eq!(interp.resource_count(), baseline, "connection should be reclaimed on close");
    }

    #[tokio::test]
    async fn server_listen_accept_recv_echo_with_raw_client() {
        // AScript binds + accepts + echoes; a raw tungstenite CLIENT connects to
        // wsListener.port, sends a message, and verifies the echo. Uses the
        // reserve-port pattern from net_tcp so the client can retry-connect.
        let mut interp = Interp::new();
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
        run_on(&mut interp, &src).await;
        let reply = client.await.unwrap();
        assert_eq!(reply, "ping");
        assert_eq!(interp.output, "nil\ntrue\nnil\nping\n");
    }

    #[tokio::test]
    async fn listen_port_zero_assigns_real_port() {
        let out = run(
            r#"
import { listen } from "std/net/ws"
let [server, err] = listen("127.0.0.1", 0)
print(err)
print(server.port > 0)
server.close()
"#,
        )
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
