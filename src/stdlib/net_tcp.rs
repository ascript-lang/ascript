//! `std/net/tcp` — TCP listener + stream native handles (feature `net`), spec §11.2.
//!
//! Built on `tokio::net` so it rides the §7 event loop. Two module entry points:
//!
//! - `connect(host, port) -> [stream, err]` — async; opens a client `TcpStream`.
//! - `listen(host, port) -> [listener, err]` — binds a `TcpListener`. The handle's
//!   `fields` carry the bound `port` (so `listen("127.0.0.1", 0)` is usable: read
//!   the OS-assigned port off `listener.port`).
//!
//! A stream is bytes-oriented: `read`/`readToEnd` return BYTES; `readLine` decodes a
//! UTF-8-lossy line (trailing newline stripped) for line protocols. Like M13's
//! process readers, a stream finalizes itself on EOF (`take_resource`), so a read
//! after EOF returns `nil` rather than panicking, and no fd leaks.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value, ValueKind};
use std::rc::Rc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Default chunk size for `stream.read()` with no `n` argument.
const DEFAULT_CHUNK: usize = 64 * 1024;

/// A buffered TCP stream: a `BufReader` wraps the socket so `readLine` works.
/// Reads and writes both go through this single handle (BufReader derefs to the
/// inner stream for writing via `get_mut`/`write_all` on the inner half).
pub struct TcpStreamState {
    reader: BufReader<TcpStream>,
}

impl TcpStreamState {
    fn new(stream: TcpStream) -> Self {
        TcpStreamState {
            reader: BufReader::new(stream),
        }
    }

    async fn read_upto(&mut self, n: usize, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        // `read_buf` over a `take(n)` adapter appends only the bytes actually
        // available, capped at `n` — bounding the read at `n` with NO 64KB zero-fill
        // on every small read (the old `resize(n, 0)` + `truncate` did). `reserve`
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

    async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.reader.get_mut().write_all(data).await
    }
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("connect", bi("net_tcp.connect")),
        ("listen", bi("net_tcp.listen")),
    ]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg)))
}

fn bytes_value(b: Vec<u8>) -> Value {
    Value::bytes(b)
}

/// Pull a string/bytes value into raw bytes (for `stream.write`).
fn data_to_bytes(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v.kind() {
        ValueKind::Str(s) => Ok(s.as_bytes().to_vec()),
        ValueKind::Bytes(b) => Ok(b.borrow().clone()),
        _ => Err(AsError::at(
            format!(
                "{} expects a string or bytes, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

/// Resolve `(host, port)` args into an address string `host:port`.
fn want_addr(args: &[Value], span: Span, ctx: &str) -> Result<String, Control> {
    let host = want_string(&arg(args, 0), span, &format!("{} host", ctx))?;
    let port = super::want_number(&arg(args, 1), span, &format!("{} port", ctx))?;
    if !(0.0..=65535.0).contains(&port) || port.fract() != 0.0 {
        return Err(AsError::at(format!("{} port must be an integer 0..=65535", ctx), span).into());
    }
    Ok(format!("{}:{}", host, port as u16))
}

impl Interp {
    /// Module-level dispatch for `std/net/tcp` (`connect`/`listen`).
    pub(crate) async fn call_net_tcp(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "connect" => self.tcp_connect(args, span).await,
            "listen" => self.tcp_listen(args, span).await,
            _ => Err(AsError::at(format!("std/net/tcp has no function '{}'", func), span).into()),
        }
    }

    async fn tcp_connect(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let addr = want_addr(args, span, "net/tcp.connect")?;
        // FFI §4.4 stage-2 (net carve-out): re-check the resolved host at connect
        // time. Gate-12: no carve-out → immediate `Ok` with no comparison.
        let host = want_string(&arg(args, 0), span, "net/tcp.connect host")?;
        self.check_net_host(&host, span)?;
        match TcpStream::connect(&addr).await {
            Ok(stream) => {
                let handle = self.register_resource(
                    NativeKind::TcpStream,
                    indexmap::IndexMap::new(),
                    ResourceState::TcpStream(TcpStreamState::new(stream)),
                );
                Ok(make_pair(handle, Value::nil()))
            }
            Err(e) => Ok(err_pair(format!(
                "net/tcp.connect to {} failed: {}",
                addr, e
            ))),
        }
    }

    async fn tcp_listen(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let addr = want_addr(args, span, "net/tcp.listen")?;
        // FFI §4.4 stage-2 (net carve-out): re-check the bind host. Gate-12:
        // no carve-out → immediate `Ok` with no comparison.
        let host = want_string(&arg(args, 0), span, "net/tcp.listen host")?;
        self.check_net_host(&host, span)?;
        match TcpListener::bind(&addr).await {
            Ok(listener) => {
                // Expose the bound port (the OS-assigned one when binding port 0) so
                // AScript can read `listener.port` — makes `listen(host, 0)` usable.
                let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                let mut fields = indexmap::IndexMap::new();
                // NUM §4: a port is an `Int`.
                fields.insert("port".to_string(), Value::int(i64::from(port)));
                let handle = self.register_resource(
                    NativeKind::TcpListener,
                    fields,
                    ResourceState::TcpListener(listener),
                );
                Ok(make_pair(handle, Value::nil()))
            }
            Err(e) => Ok(err_pair(format!(
                "net/tcp.listen on {} failed: {}",
                addr, e
            ))),
        }
    }

    /// Dispatch a method on a TCP stream / listener handle.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_tcp_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.receiver.kind {
            NativeKind::TcpStream => self.tcp_stream_method(id, &m.method, &args, span).await,
            NativeKind::TcpListener => self.tcp_listener_method(id, &m.method, &args, span).await,
            _ => {
                Err(AsError::at(format!("native handle has no method '{}'", m.method), span).into())
            }
        }
    }

    async fn tcp_stream_method(
        &self,
        id: u64,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "read" => {
                let n = match args.first() {
                    None => DEFAULT_CHUNK,
                    Some(v) if matches!(v.kind(), ValueKind::Nil) => DEFAULT_CHUNK,
                    // Guard before the cast: an `Inf`/`NaN`/out-of-range `n` would cast
                    // to `usize::MAX` and abort the host via `buf.reserve(n)`.
                    Some(v) => super::want_count(v, span, "stream.read", super::MAX_ALLOC_COUNT)?,
                };
                // read(0) is a no-op: return empty bytes WITHOUT touching the resource.
                // (Otherwise an empty read buffer yields Ok(0), which the match below
                // treats as EOF and would finalize a still-open socket.)
                if n == 0 {
                    return Ok(bytes_value(Vec::new()));
                }
                // A closed/EOF'd stream degrades to nil rather than panicking.
                // Take the stream OUT so no table borrow is held across the await.
                let mut stream = match self.take_resource(id) {
                    Some(ResourceState::TcpStream(s)) => s,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(Value::nil());
                    }
                };
                let mut buf = Vec::new();
                match stream.read_upto(n, &mut buf).await {
                    Ok(0) => {
                        // EOF: drop the stream so its socket fd is reclaimed now.
                        Ok(Value::nil())
                    }
                    Ok(_) => {
                        self.return_resource(id, ResourceState::TcpStream(stream));
                        Ok(bytes_value(buf))
                    }
                    Err(e) => Err(AsError::at(format!("stream.read failed: {}", e), span).into()),
                }
            }
            "readLine" => {
                let mut stream = match self.take_resource(id) {
                    Some(ResourceState::TcpStream(s)) => s,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(Value::nil()); // gone → EOF
                    }
                };
                let mut buf = Vec::new();
                match stream.read_line_bytes(&mut buf).await {
                    Ok(0) => {
                        // EOF: drop the stream.
                        Ok(Value::nil())
                    }
                    Ok(_) => {
                        self.return_resource(id, ResourceState::TcpStream(stream));
                        // Strip a single trailing '\n' and an optional preceding '\r'.
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                            if buf.last() == Some(&b'\r') {
                                buf.pop();
                            }
                        }
                        Ok(Value::str(
                            String::from_utf8_lossy(&buf).into_owned(),
                        ))
                    }
                    Err(e) => {
                        Err(AsError::at(format!("stream.readLine failed: {}", e), span).into())
                    }
                }
            }
            "readToEnd" => {
                // readToEnd is type-stable: it ALWAYS returns bytes (empty if the
                // stream was already drained / finalized), matching `readToEnd()→bytes`.
                let mut stream = match self.take_resource(id) {
                    Some(ResourceState::TcpStream(s)) => s,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(bytes_value(Vec::new())); // gone → empty bytes
                    }
                };
                let mut buf = Vec::new();
                // readToEnd consumes the whole stream; we drop it either way.
                match stream.read_to_end_bytes(&mut buf).await {
                    Ok(_) => Ok(bytes_value(buf)),
                    Err(e) => {
                        Err(AsError::at(format!("stream.readToEnd failed: {}", e), span).into())
                    }
                }
            }
            "write" => {
                let data = data_to_bytes(&arg(args, 0), span, "stream.write")?;
                let mut stream = match self.take_resource(id) {
                    Some(ResourceState::TcpStream(s)) => s,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(err_pair("stream.write: stream is closed".to_string()));
                    }
                };
                let r = stream.write_all(&data).await;
                self.return_resource(id, ResourceState::TcpStream(stream));
                match r {
                    Ok(_) => Ok(make_pair(Value::nil(), Value::nil())),
                    Err(e) => Ok(err_pair(format!("stream.write failed: {}", e))),
                }
            }
            "close" => {
                // Dropping the stream closes the socket.
                self.take_resource(id);
                Ok(Value::nil())
            }
            other => Err(AsError::at(format!("tcpStream has no method '{}'", other), span).into()),
        }
    }

    async fn tcp_listener_method(
        &self,
        id: u64,
        method: &str,
        _args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "accept" => {
                let listener = match self.take_resource(id) {
                    Some(ResourceState::TcpListener(l)) => l,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(err_pair("listener.accept: listener is closed".to_string()));
                    }
                };
                let accepted = listener.accept().await;
                // The listener keeps accepting: put it back.
                self.return_resource(id, ResourceState::TcpListener(listener));
                match accepted {
                    Ok((stream, _peer)) => {
                        let handle = self.register_resource(
                            NativeKind::TcpStream,
                            indexmap::IndexMap::new(),
                            ResourceState::TcpStream(TcpStreamState::new(stream)),
                        );
                        Ok(make_pair(handle, Value::nil()))
                    }
                    Err(e) => Ok(err_pair(format!("listener.accept failed: {}", e))),
                }
            }
            "close" => {
                // Dropping the listener stops accepting.
                self.take_resource(id);
                Ok(Value::nil())
            }
            other => {
                Err(AsError::at(format!("tcpListener has no method '{}'", other), span).into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::interp::Interp;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

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

    /// Spawn a one-shot TCP echo peer on 127.0.0.1:0; return its bound port. The
    /// peer accepts ONE connection, echoes everything it reads back, then closes.
    /// This is the reusable in-process fixture pattern for the whole milestone.
    async fn spawn_echo_peer() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).await.expect("read");
                if n == 0 {
                    break;
                }
                sock.write_all(&buf[..n]).await.expect("write");
            }
        });
        port
    }

    #[tokio::test]
    async fn connect_write_readline_against_echo_peer() {
        let port = spawn_echo_peer().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/tcp"
let [stream, err] = connect("127.0.0.1", {port})
print(err)
let [_w, werr] = await stream.write("hello\n")
print(werr)
let line = await stream.readLine()
print(line)
stream.close()
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\nnil\nhello\n");
    }

    #[tokio::test]
    async fn read_zero_returns_empty_bytes_without_finalizing() {
        // read(0) must return empty bytes WITHOUT closing the socket; a subsequent
        // read() must still see real echoed data (proves the stream wasn't finalized).
        let port = spawn_echo_peer().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/tcp"
let [stream, _e] = connect("127.0.0.1", {port})
await stream.write("abc")
let empty = await stream.read(0)
print(type(empty))
print(len(empty))
let chunk = await stream.read()
print(len(chunk))
stream.close()
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "bytes\n0\n3\n");
    }

    #[tokio::test]
    async fn read_returns_bytes() {
        let port = spawn_echo_peer().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/tcp"
let [stream, _e] = connect("127.0.0.1", {port})
await stream.write("abc")
let chunk = await stream.read()
print(type(chunk))
print(len(chunk))
stream.close()
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "bytes\n3\n");
    }

    #[tokio::test]
    async fn connect_to_closed_port_is_err() {
        // Bind then drop a listener to get a port nobody is listening on.
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };
        let src = format!(
            r#"
import {{ connect }} from "std/net/tcp"
let [stream, err] = connect("127.0.0.1", {port})
print(stream)
print(err != nil)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn read_after_eof_returns_nil_repeatedly() {
        // A peer that immediately closes after accepting → client sees EOF.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            drop(sock); // close immediately
        });
        let src = format!(
            r#"
import {{ connect }} from "std/net/tcp"
let [stream, _e] = connect("127.0.0.1", {port})
let a = await stream.read()
let b = await stream.read()
print(a)
print(b)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "nil\nnil\n");
    }

    #[tokio::test]
    async fn listen_accept_reads_from_raw_client() {
        // AScript binds + accepts; a raw tokio client connects to listener.port and
        // sends a line. Verifies the AScript listen/accept side end-to-end.
        let interp = Interp::new();
        // Reserve a free port (bind+drop), hand it to the AScript `listen`, and have
        // the raw client retry-connect until AScript's listener is up — deterministic
        // without needing to interleave reading `listener.port` across executions.
        let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = reserve.local_addr().unwrap().port();
        drop(reserve);

        // Raw client: retry-connect until AScript's listener is up, then send a line.
        let client = tokio::spawn(async move {
            loop {
                match TcpStream::connect(("127.0.0.1", port)).await {
                    Ok(mut s) => {
                        s.write_all(b"ping\n").await.unwrap();
                        s.shutdown().await.ok();
                        break;
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
                }
            }
        });

        let src = format!(
            r#"
import {{ listen }} from "std/net/tcp"
let [server, err] = listen("127.0.0.1", {port})
print(err)
print(server.port == {port})
let [conn, aerr] = await server.accept()
print(aerr)
let line = await conn.readLine()
print(line)
conn.close()
server.close()
"#
        );
        run_on(&interp, &src).await;
        client.await.unwrap();
        assert_eq!(interp.output(), "nil\ntrue\nnil\nping\n");
    }

    #[tokio::test]
    async fn listen_port_zero_assigns_real_port() {
        let out = run(r#"
import { listen } from "std/net/tcp"
let [server, err] = listen("127.0.0.1", 0)
print(err)
print(server.port > 0)
server.close()
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn write_to_closed_stream_returns_err_not_panic() {
        let port = spawn_echo_peer().await;
        let src = format!(
            r#"
import {{ connect }} from "std/net/tcp"
let [stream, _e] = connect("127.0.0.1", {port})
stream.close()
let [_n, werr] = await stream.write("x")
print(werr != nil)
"#
        );
        let out = run(&src).await;
        assert_eq!(out, "true\n");
    }

    #[tokio::test]
    async fn stream_resources_reclaimed_after_close() {
        let port = spawn_echo_peer().await;
        let interp = Interp::new();
        let baseline = interp.resource_count();
        let src = format!(
            r#"
import {{ connect }} from "std/net/tcp"
let [stream, _e] = connect("127.0.0.1", {port})
await stream.write("hi\n")
let line = await stream.readLine()
stream.close()
"#
        );
        run_on(&interp, &src).await;
        assert_eq!(
            interp.resource_count(),
            baseline,
            "stream should be reclaimed on close"
        );
    }
}
