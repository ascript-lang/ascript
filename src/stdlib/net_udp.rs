//! `std/net/udp` — UDP socket native handles (feature `net`), spec §11.2 (datagram extension).
//!
//! All operations live on a bound `UdpSocket` handle registered in the interpreter's
//! resource table. Follows the same `Value::Native` + take-out-across-await pattern as
//! `std/net/tcp`.
//!
//! Module entry points:
//! - `bind(addr) -> [socket, err]` — bind to `"host:port"` (e.g. `"127.0.0.1:0"` for
//!   an OS-assigned ephemeral port). Tier-1 result.
//!
//! Socket methods (on the returned handle):
//! - `send(data, addr) -> [bytesSent, err]` — async. `data` may be a string (UTF-8) or
//!   bytes. Tier-1 result.
//! - `recv() -> [{data, from}, err]` — async. Returns a datagram as Bytes + sender
//!   address string. Buffer is capped at `MAX_UDP_DATAGRAM` (65 507 bytes). Tier-1.
//! - `localAddr() -> string` — bound local address as `"ip:port"`.
//! - `close()` — release the socket.

use super::{bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value};
use std::cell::RefCell;
use std::rc::Rc;
use tokio::net::UdpSocket;

/// Maximum datagram size for `recv` (max UDP payload over IPv4).
const MAX_UDP_DATAGRAM: usize = 65507;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("bind", bi("net_udp.bind"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

fn bytes_value(b: Vec<u8>) -> Value {
    Value::Bytes(Rc::new(RefCell::new(b)))
}

/// Parse `data` argument (string → UTF-8 bytes, or bytes value).
fn data_to_bytes(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.borrow().clone()),
        other => Err(AsError::at(
            format!(
                "{} expects a string or bytes, got {}",
                ctx,
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

impl Interp {
    /// Module-level dispatch for `std/net/udp` (`bind`).
    pub(crate) async fn call_net_udp(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "bind" => self.udp_bind(args, span).await,
            _ => Err(AsError::at(format!("std/net/udp has no function '{}'", func), span).into()),
        }
    }

    async fn udp_bind(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let addr = want_string(
            args.first().unwrap_or(&Value::Nil),
            span,
            "net/udp.bind addr",
        )?;
        match UdpSocket::bind(&*addr).await {
            Ok(sock) => {
                let handle = self.register_resource(
                    NativeKind::UdpSocket,
                    indexmap::IndexMap::new(),
                    ResourceState::UdpSocket(sock),
                );
                Ok(make_pair(handle, Value::Nil))
            }
            Err(e) => Ok(err_pair(format!("net/udp.bind to {} failed: {}", addr, e))),
        }
    }

    /// Dispatch a method call on a `UdpSocket` handle.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_udp_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            "send" => {
                // send(data, addr) -> [bytesSent, err]
                let data = data_to_bytes(args.first().unwrap_or(&Value::Nil), span, "socket.send")?;
                let addr =
                    want_string(args.get(1).unwrap_or(&Value::Nil), span, "socket.send addr")?;
                let sock = match self.take_resource(id) {
                    Some(ResourceState::UdpSocket(s)) => s,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(err_pair("socket.send: socket is closed".to_string()));
                    }
                };
                let result = sock.send_to(&data, &*addr).await;
                self.return_resource(id, ResourceState::UdpSocket(sock));
                match result {
                    // NUM §4: a byte count is an `Int`.
                    Ok(n) => Ok(make_pair(Value::Int(n as i64), Value::Nil)),
                    Err(e) => Ok(err_pair(format!("socket.send to {} failed: {}", addr, e))),
                }
            }
            "recv" => {
                // recv() -> [{data, from}, err]
                let sock = match self.take_resource(id) {
                    Some(ResourceState::UdpSocket(s)) => s,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(err_pair("socket.recv: socket is closed".to_string()));
                    }
                };
                let mut buf = vec![0u8; MAX_UDP_DATAGRAM];
                let result = sock.recv_from(&mut buf).await;
                self.return_resource(id, ResourceState::UdpSocket(sock));
                match result {
                    Ok((n, from)) => {
                        buf.truncate(n);
                        let mut obj = indexmap::IndexMap::new();
                        obj.insert("data".to_string(), bytes_value(buf));
                        obj.insert("from".to_string(), Value::Str(from.to_string().into()));
                        Ok(make_pair(
                            Value::Object(crate::value::ObjectCell::new(obj)),
                            Value::Nil,
                        ))
                    }
                    Err(e) => Ok(err_pair(format!("socket.recv failed: {}", e))),
                }
            }
            "localAddr" => {
                // localAddr() -> string (non-mutating; just peek the resource)
                let addr = self.with_resource(id, |r| match r {
                    Some(ResourceState::UdpSocket(s)) => {
                        s.local_addr().map(|a| a.to_string()).unwrap_or_default()
                    }
                    _ => String::new(),
                });
                Ok(Value::Str(addr.into()))
            }
            "close" => {
                self.take_resource(id);
                Ok(Value::Nil)
            }
            other => Err(AsError::at(format!("udpSocket has no method '{}'", other), span).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    /// Run an AScript program and return its captured output.
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    #[tokio::test]
    async fn bind_returns_socket_with_local_addr() {
        // Bind two sockets on ephemeral ports; verify localAddr is populated.
        let out = run(r#"
import { bind } from "std/net/udp"
import { contains } from "std/string"
let [sockA, errA] = bind("127.0.0.1:0")
let [sockB, errB] = bind("127.0.0.1:0")
print(errA)
print(errB)
let addrA = sockA.localAddr()
let addrB = sockB.localAddr()
print(contains(addrA, "127.0.0.1"))
print(contains(addrB, "127.0.0.1"))
print(len(addrA) > 0)
sockA.close()
sockB.close()
"#)
        .await;
        assert_eq!(out, "nil\nnil\ntrue\ntrue\ntrue\n");
    }

    #[tokio::test]
    async fn send_recv_datagram() {
        // Send a datagram from sockA → sockB and verify recv delivers it.
        let out = run(r#"
import { bind } from "std/net/udp"
import { utf8Decode } from "std/encoding"
import { contains } from "std/string"
let [sockA, eA] = bind("127.0.0.1:0")
let [sockB, eB] = bind("127.0.0.1:0")
let addrB = sockB.localAddr()
let [sent, sendErr] = await sockA.send("hello udp", addrB)
print(sendErr)
print(sent > 0)
let result = await sockB.recv()
let pkt = result[0]
let recvErr = result[1]
print(recvErr)
let [text, decErr] = utf8Decode(pkt.data)
print(text)
print(contains(pkt.from, "127.0.0.1"))
sockA.close()
sockB.close()
"#)
        .await;
        assert_eq!(out, "nil\ntrue\nnil\nhello udp\ntrue\n");
    }

    #[tokio::test]
    async fn recv_data_is_bytes() {
        // recv().data must be Value::Bytes, not a string.
        let out = run(r#"
import { bind } from "std/net/udp"
let [sockA, eA] = bind("127.0.0.1:0")
let [sockB, eB] = bind("127.0.0.1:0")
let addrB = sockB.localAddr()
await sockA.send("test", addrB)
let result = await sockB.recv()
let pkt = result[0]
print(type(pkt.data))
print(len(pkt.data))
sockA.close()
sockB.close()
"#)
        .await;
        assert_eq!(out, "bytes\n4\n");
    }

    #[tokio::test]
    async fn bind_bad_addr_returns_err() {
        // Binding an invalid address must return [nil, err], not panic.
        let out = run(r#"
import { bind } from "std/net/udp"
let [sock, err] = bind("not-an-addr")
print(sock)
print(err != nil)
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn close_then_send_returns_err() {
        // After close(), send must return an error rather than panic.
        let out = run(r#"
import { bind } from "std/net/udp"
let [sockA, eA] = bind("127.0.0.1:0")
let [sockB, eB] = bind("127.0.0.1:0")
let addrB = sockB.localAddr()
sockA.close()
let result = await sockA.send("x", addrB)
let err = result[1]
print(err != nil)
sockB.close()
"#)
        .await;
        assert_eq!(out, "true\n");
    }

    #[tokio::test]
    async fn send_bytes_value() {
        // send() should also accept a Value::Bytes (not just strings).
        let out = run(r#"
import { bind } from "std/net/udp"
import { utf8Encode, utf8Decode } from "std/encoding"
let [sockA, eA] = bind("127.0.0.1:0")
let [sockB, eB] = bind("127.0.0.1:0")
let addrB = sockB.localAddr()
let payload = utf8Encode("bytes data")
let [sent, sendErr] = await sockA.send(payload, addrB)
print(sendErr)
let result = await sockB.recv()
let pkt = result[0]
let recvErr = result[1]
print(recvErr)
let [text, decErr] = utf8Decode(pkt.data)
print(text)
sockA.close()
sockB.close()
"#)
        .await;
        assert_eq!(out, "nil\nnil\nbytes data\n");
    }
}
