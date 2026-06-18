//! `std/net/unix` — Unix-domain-socket listener + stream native handles (feature
//! `net`), spec CNTR §3.1.
//!
//! The structural mirror of [`net_tcp`](super::net_tcp) over `tokio::net::UnixStream`/
//! `UnixListener` instead of TCP. Two module entry points:
//!
//! - `connect(path) -> [stream, err]` — async; opens a client `UnixStream` to `path`.
//! - `listen(path) -> [listener, err]` — binds a `UnixListener` at the filesystem
//!   `path`. The bound `Listener` UNLINKS the socket file it created on last-drop /
//!   `close()` (a Unix socket leaves a stale inode otherwise). The handle's `fields`
//!   carry the bound `path` (`listener.path`).
//!
//! A stream is bytes-oriented exactly like a TCP stream: `read`/`readToEnd` return
//! BYTES; `readLine` decodes a UTF-8-lossy line (trailing newline stripped). Like a TCP
//! stream, a UDS stream finalizes itself on EOF (`take_resource`), so a read after EOF
//! returns `nil` rather than panicking, and no fd leaks.
//!
//! Unix-domain sockets are a POSIX concept; the non-`unix` arms of `connect`/`listen`
//! raise a Tier-2 panic (`Unix-domain sockets are not supported on this platform`).

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp};
use crate::span::Span;
use crate::value::Value;
#[cfg(unix)]
use crate::value::ValueKind;

#[cfg(unix)]
use crate::interp::ResourceState;
#[cfg(unix)]
use crate::value::{NativeKind, NativeMethod};
#[cfg(unix)]
use std::rc::Rc;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

/// Default chunk size for `stream.read()` with no `n` argument.
#[cfg(unix)]
const DEFAULT_CHUNK: usize = 64 * 1024;

/// A buffered Unix-domain stream: a `BufReader` wraps the socket so `readLine` works.
/// Mirrors [`super::net_tcp::TcpStreamState`] byte-for-byte over `UnixStream`.
#[cfg(unix)]
pub struct UnixStreamState {
    reader: BufReader<UnixStream>,
}

#[cfg(unix)]
impl UnixStreamState {
    fn new(stream: UnixStream) -> Self {
        UnixStreamState {
            reader: BufReader::new(stream),
        }
    }

    async fn read_upto(&mut self, n: usize, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        // COPY of TcpStreamState::read_upto — `read_buf` over a `take(n)` adapter appends
        // only the bytes actually available, capped at `n`, with NO 64KB zero-fill on
        // every small read. A hard `take(n)` cap is required (`reserve` alone over-fills
        // to the vec's full spare capacity).
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

/// A bound Unix-domain listener that best-effort-unlinks the socket path it created
/// when the handle is dropped (`close()` / last-drop). A Unix socket leaves a stale
/// inode in the filesystem otherwise. Only the path WE bound is unlinked — a listen
/// that failed (EADDRINUSE on an existing path) never constructs this, so a path we did
/// NOT bind is never removed.
#[cfg(unix)]
pub struct UnixListenerState {
    listener: UnixListener,
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Drop for UnixListenerState {
    fn drop(&mut self) {
        // Best-effort: ignore the error (the file may already be gone, or unlinkable —
        // either way teardown must not panic).
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("connect", bi("net_unix.connect")),
        ("listen", bi("net_unix.listen")),
    ]
}

#[cfg(unix)]
fn err_pair(msg: String) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg)))
}

#[cfg(unix)]
fn bytes_value(b: Vec<u8>) -> Value {
    Value::bytes(b)
}

/// Pull a string/bytes value into raw bytes (for `stream.write`).
#[cfg(unix)]
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

impl Interp {
    /// Module-level dispatch for `std/net/unix` (`connect`/`listen`).
    pub(crate) async fn call_net_unix(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "connect" => self.unix_connect(args, span).await,
            "listen" => self.unix_listen(args, span).await,
            _ => {
                Err(AsError::at(format!("std/net/unix has no function '{}'", func), span).into())
            }
        }
    }

    #[cfg(unix)]
    async fn unix_connect(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let path = want_string(&arg(args, 0), span, "net/unix.connect path")?;
        // CNTR §3.1 stage-2 (net carve-out, UDS form): re-check the resolved socket path
        // at connect time. Gate-12: no carve-out → immediate `Ok` with no comparison.
        self.check_unix_path(&path, span)?;
        match UnixStream::connect(&*path).await {
            Ok(stream) => {
                let handle = self.register_resource(
                    NativeKind::UnixStream,
                    indexmap::IndexMap::new(),
                    ResourceState::UnixStream(UnixStreamState::new(stream)),
                );
                Ok(make_pair(handle, Value::nil()))
            }
            Err(e) => Ok(err_pair(format!(
                "net/unix.connect to {} failed: {}",
                path, e
            ))),
        }
    }

    #[cfg(unix)]
    async fn unix_listen(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let path = want_string(&arg(args, 0), span, "net/unix.listen path")?;
        // CNTR §3.1 stage-2 (net carve-out, UDS form): re-check the bind path. Gate-12:
        // no carve-out → immediate `Ok` with no comparison.
        self.check_unix_path(&path, span)?;
        match UnixListener::bind(&*path) {
            Ok(listener) => {
                let mut fields = indexmap::IndexMap::new();
                fields.insert("path".to_string(), Value::str(path.clone()));
                let handle = self.register_resource(
                    NativeKind::UnixListener,
                    fields,
                    ResourceState::UnixListener(UnixListenerState {
                        listener,
                        path: std::path::PathBuf::from(&*path),
                    }),
                );
                Ok(make_pair(handle, Value::nil()))
            }
            Err(e) => Ok(err_pair(format!(
                "net/unix.listen on {} failed: {}",
                path, e
            ))),
        }
    }

    /// Non-Unix stub: Unix-domain sockets are a POSIX concept. A Tier-2 panic naming the
    /// platform limitation (never a silent no-op).
    #[cfg(not(unix))]
    async fn unix_connect(&self, _args: &[Value], span: Span) -> Result<Value, Control> {
        Err(AsError::at(
            "Unix-domain sockets are not supported on this platform".to_string(),
            span,
        )
        .into())
    }

    #[cfg(not(unix))]
    async fn unix_listen(&self, _args: &[Value], span: Span) -> Result<Value, Control> {
        Err(AsError::at(
            "Unix-domain sockets are not supported on this platform".to_string(),
            span,
        )
        .into())
    }

    /// Dispatch a method on a UDS stream / listener handle.
    #[cfg(unix)]
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_unix_method(
        &self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.receiver.kind {
            NativeKind::UnixStream => self.unix_stream_method(id, &m.method, &args, span).await,
            NativeKind::UnixListener => self.unix_listener_method(id, &m.method, &args, span).await,
            _ => {
                Err(AsError::at(format!("native handle has no method '{}'", m.method), span).into())
            }
        }
    }

    #[cfg(unix)]
    async fn unix_stream_method(
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
                if n == 0 {
                    return Ok(bytes_value(Vec::new()));
                }
                // A closed/EOF'd stream degrades to nil rather than panicking.
                // Take the stream OUT so no table borrow is held across the await.
                let mut stream = match self.take_resource(id) {
                    Some(ResourceState::UnixStream(s)) => s,
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
                        self.return_resource(id, ResourceState::UnixStream(stream));
                        Ok(bytes_value(buf))
                    }
                    Err(e) => Err(AsError::at(format!("stream.read failed: {}", e), span).into()),
                }
            }
            "readLine" => {
                let mut stream = match self.take_resource(id) {
                    Some(ResourceState::UnixStream(s)) => s,
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
                        self.return_resource(id, ResourceState::UnixStream(stream));
                        // Strip a single trailing '\n' and an optional preceding '\r'.
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                            if buf.last() == Some(&b'\r') {
                                buf.pop();
                            }
                        }
                        Ok(Value::str(String::from_utf8_lossy(&buf).into_owned()))
                    }
                    Err(e) => {
                        Err(AsError::at(format!("stream.readLine failed: {}", e), span).into())
                    }
                }
            }
            "readToEnd" => {
                // readToEnd is type-stable: it ALWAYS returns bytes (empty if the
                // stream was already drained / finalized).
                let mut stream = match self.take_resource(id) {
                    Some(ResourceState::UnixStream(s)) => s,
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
                    Some(ResourceState::UnixStream(s)) => s,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(err_pair("stream.write: stream is closed".to_string()));
                    }
                };
                let r = stream.write_all(&data).await;
                self.return_resource(id, ResourceState::UnixStream(stream));
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
            other => {
                Err(AsError::at(format!("unixStream has no method '{}'", other), span).into())
            }
        }
    }

    #[cfg(unix)]
    async fn unix_listener_method(
        &self,
        id: u64,
        method: &str,
        _args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "accept" => {
                let listener = match self.take_resource(id) {
                    Some(ResourceState::UnixListener(l)) => l,
                    other => {
                        if let Some(o) = other {
                            self.return_resource(id, o);
                        }
                        return Ok(err_pair("listener.accept: listener is closed".to_string()));
                    }
                };
                let accepted = listener.listener.accept().await;
                // The listener keeps accepting: put it back.
                self.return_resource(id, ResourceState::UnixListener(listener));
                match accepted {
                    Ok((stream, _peer)) => {
                        let handle = self.register_resource(
                            NativeKind::UnixStream,
                            indexmap::IndexMap::new(),
                            ResourceState::UnixStream(UnixStreamState::new(stream)),
                        );
                        Ok(make_pair(handle, Value::nil()))
                    }
                    Err(e) => Ok(err_pair(format!("listener.accept failed: {}", e))),
                }
            }
            "close" => {
                // Dropping the listener stops accepting AND unlinks the socket path (Drop).
                self.take_resource(id);
                Ok(Value::nil())
            }
            other => {
                Err(AsError::at(format!("unixListener has no method '{}'", other), span).into())
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use crate::interp::Interp;
    use crate::value::{NativeKind, Value};
    use std::rc::Rc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    /// A unique temp-dir socket path that is cleaned up when the returned guard drops.
    /// Mirrors the per-test isolation of net_tcp's bind-to-port-0.
    struct UdsTempPath {
        dir: std::path::PathBuf,
        path: std::path::PathBuf,
    }

    impl Drop for UdsTempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_dir(&self.dir);
        }
    }

    fn uds_temp_path(tag: &str) -> UdsTempPath {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("ascript-uds-{}-{}-{}", tag, pid, n));
        std::fs::create_dir_all(&dir).expect("mkdir temp");
        // Keep the socket basename SHORT: sun_path is ~104 bytes on macOS, ~108 on Linux.
        let path = dir.join("s.sock");
        UdsTempPath { dir, path }
    }

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

    /// Spawn a one-shot UDS echo peer bound at `path`; accepts ONE connection, echoes
    /// everything it reads back, then closes. Mirrors net_tcp's `spawn_echo_peer`.
    fn spawn_uds_echo_peer(path: &std::path::Path) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("bind uds");
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
        })
    }

    #[tokio::test]
    async fn connect_write_readline_against_uds_echo_peer() {
        let tmp = uds_temp_path("echo");
        let path = tmp.path.to_str().unwrap().to_string();
        let _peer = spawn_uds_echo_peer(&tmp.path);
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [stream, err] = await unix.connect("{path}")
print(err)
let [_w, werr] = await stream.write("hello\n")
print(werr)
print(await stream.readLine())
stream.close()
"#
        );
        assert_eq!(run(&src).await, "nil\nnil\nhello\n");
    }

    #[tokio::test]
    async fn read_zero_returns_empty_bytes_without_finalizing() {
        let tmp = uds_temp_path("readzero");
        let path = tmp.path.to_str().unwrap().to_string();
        let _peer = spawn_uds_echo_peer(&tmp.path);
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [stream, _e] = await unix.connect("{path}")
await stream.write("abc")
let empty = await stream.read(0)
print(type(empty))
print(len(empty))
let chunk = await stream.read()
print(len(chunk))
stream.close()
"#
        );
        assert_eq!(run(&src).await, "bytes\n0\n3\n");
    }

    #[tokio::test]
    async fn read_returns_bytes() {
        let tmp = uds_temp_path("readbytes");
        let path = tmp.path.to_str().unwrap().to_string();
        let _peer = spawn_uds_echo_peer(&tmp.path);
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [stream, _e] = await unix.connect("{path}")
await stream.write("abc")
let chunk = await stream.read()
print(type(chunk))
print(len(chunk))
stream.close()
"#
        );
        assert_eq!(run(&src).await, "bytes\n3\n");
    }

    #[tokio::test]
    async fn connect_to_nonexistent_path_is_err() {
        let tmp = uds_temp_path("missing");
        // Do not create a peer: the path has no listener.
        let path = tmp.path.to_str().unwrap().to_string();
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [stream, err] = await unix.connect("{path}")
print(stream)
print(err != nil)
"#
        );
        assert_eq!(run(&src).await, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn read_after_eof_returns_nil_repeatedly() {
        let tmp = uds_temp_path("eof");
        let path = tmp.path.to_str().unwrap().to_string();
        // A peer that immediately closes after accepting → client sees EOF.
        let listener = UnixListener::bind(&tmp.path).expect("bind");
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            drop(sock);
        });
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [stream, _e] = await unix.connect("{path}")
let a = await stream.read()
let b = await stream.read()
print(a)
print(b)
"#
        );
        assert_eq!(run(&src).await, "nil\nnil\n");
    }

    #[tokio::test]
    async fn listen_accept_reads_from_raw_client() {
        let interp = Interp::new();
        let tmp = uds_temp_path("listen");
        let path = tmp.path.to_str().unwrap().to_string();
        let cpath = tmp.path.clone();
        // Raw client: retry-connect until AScript's listener is up, then send a line.
        let client = tokio::spawn(async move {
            loop {
                match UnixStream::connect(&cpath).await {
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
import * as unix from "std/net/unix"
let [server, err] = await unix.listen("{path}")
print(err)
print(server.path == "{path}")
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
    async fn listen_on_existing_path_is_err_and_preserves_file() {
        let tmp = uds_temp_path("inuse");
        let path = tmp.path.to_str().unwrap().to_string();
        // Bind a real listener at the path; keep it alive for the whole test.
        let _busy = UnixListener::bind(&tmp.path).expect("bind");
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [server, err] = await unix.listen("{path}")
print(server)
print(err != nil)
"#
        );
        assert_eq!(run(&src).await, "nil\ntrue\n");
        // The file we did NOT bind must SURVIVE — a failed listen never unlinks a path
        // it didn't create.
        assert!(tmp.path.exists(), "an existing socket we didn't bind must survive a failed listen");
    }

    #[tokio::test]
    async fn close_unlinks_the_bound_socket_file() {
        let interp = Interp::new();
        let tmp = uds_temp_path("unlink");
        let path = tmp.path.to_str().unwrap().to_string();
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [server, _e] = await unix.listen("{path}")
server.close()
"#
        );
        run_on(&interp, &src).await;
        assert!(!tmp.path.exists(), "the socket file should be gone after close()");
    }

    #[tokio::test]
    async fn write_to_closed_stream_returns_err_not_panic() {
        let tmp = uds_temp_path("writeclosed");
        let path = tmp.path.to_str().unwrap().to_string();
        let _peer = spawn_uds_echo_peer(&tmp.path);
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [stream, _e] = await unix.connect("{path}")
stream.close()
let [_n, werr] = await stream.write("x")
print(werr != nil)
"#
        );
        assert_eq!(run(&src).await, "true\n");
    }

    #[tokio::test]
    async fn stream_resources_reclaimed_after_close() {
        let tmp = uds_temp_path("reclaim");
        let path = tmp.path.to_str().unwrap().to_string();
        let _peer = spawn_uds_echo_peer(&tmp.path);
        let interp = Interp::new();
        let baseline = interp.resource_count();
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [stream, _e] = await unix.connect("{path}")
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

    #[test]
    fn stream_and_listener_handles_are_non_sendable() {
        // A UnixStream/UnixListener handle is a native resource → the worker airlock
        // (`check_sendable`) rejects it (it is `!Send`), so passing one to a `worker fn`
        // raises a field-path panic rather than crossing the boundary.
        for kind in [NativeKind::UnixStream, NativeKind::UnixListener] {
            let handle = Value::native(Rc::new(crate::value::NativeObject {
                id: 1,
                kind,
                fields: indexmap::IndexMap::new(),
            }));
            let err = crate::worker::serialize::check_sendable(&handle)
                .expect_err("a UDS native handle must be non-sendable (worker-airlock)");
            assert_eq!(err.kind, "native");
        }
    }

    #[test]
    fn deny_net_scope_blocks_uds_path() {
        use crate::span::Span;
        use crate::stdlib::caps::{CapSet, NetDeny, NetScope};
        let interp = Interp::new();
        // A net carve-out with NO unix allow entry → the UDS path is denied at stage-2.
        let mut cs = CapSet::all_granted();
        cs.set_net_scope(NetScope {
            deny: NetDeny::All,
            allow: vec![],
        });
        interp.set_caps(cs);
        match interp.check_unix_path("/tmp/some.sock", Span::new(0, 0)) {
            Err(crate::interp::Control::Panic(e)) => {
                // `Control::Panic` carries the AsError; assert the wording.
                assert!(
                    e.message.contains("net") && e.message.contains("/tmp/some.sock"),
                    "{}",
                    e.message
                );
            }
            other => panic!("expected unix-path denial, got {other:?}"),
        }
    }

    #[test]
    fn allow_unix_carveout_admits_uds_path() {
        use crate::span::Span;
        use crate::stdlib::caps::{CapSet, NetDeny, NetScope};
        let interp = Interp::new();
        // The carve-out allow-list names the canonicalized `unix:<path>`.
        let canon = interp.unix_scope_key("/tmp/some.sock");
        let mut cs = CapSet::all_granted();
        cs.set_net_scope(NetScope {
            deny: NetDeny::All,
            allow: vec![canon],
        });
        interp.set_caps(cs);
        assert!(interp.check_unix_path("/tmp/some.sock", Span::new(0, 0)).is_ok());
        // Gate-12: no carve-out → immediate Ok.
        let plain = Interp::new();
        assert!(plain.check_unix_path("/anything", Span::new(0, 0)).is_ok());
    }

    #[tokio::test]
    async fn allow_unix_carveout_admits_uds_end_to_end() {
        use crate::stdlib::caps::{CapSet, NetDeny, NetScope};
        let tmp = uds_temp_path("allowed");
        let path = tmp.path.to_str().unwrap().to_string();
        let _peer = spawn_uds_echo_peer(&tmp.path);
        let interp = Interp::new();
        let canon = interp.unix_scope_key(&path);
        let mut cs = CapSet::all_granted();
        cs.set_net_scope(NetScope {
            deny: NetDeny::All,
            allow: vec![canon],
        });
        interp.set_caps(cs);
        let src = format!(
            r#"
import * as unix from "std/net/unix"
let [stream, err] = await unix.connect("{path}")
print(err)
stream.close()
"#
        );
        run_on(&interp, &src).await;
        assert_eq!(interp.output(), "nil\n");
    }
}
