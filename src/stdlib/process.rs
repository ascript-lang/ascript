//! `std/process` — subprocess execution (feature `sys`), spec §11.4.
//!
//! Built on `tokio::process` so it rides the §7 event loop. Two entry points share
//! one options object:
//!
//! - `run(cmd, args, opts?) -> [result, err]` — one-shot: spawn, await completion,
//!   capture output. **A non-zero exit is NOT an err** — it comes back as a normal
//!   `result` with `success == false`. Only a **spawn failure** (binary not found,
//!   permission denied, timeout) is the `err`; `check: true` flips a non-zero exit
//!   into an err. `result = {stdout, stderr, stderrText, code, signal, success}`.
//! - `spawn(cmd, args, opts?) -> [child, err]` — streaming: returns a
//!   `Value::Native(kind=ChildProcess)` with `fields = {pid}` and methods `stdin`
//!   (→ a Writer native), `stdout`/`stderr` (→ Reader natives), `wait()`, `kill(sig?)`.
//!   The child + its piped stdio live in the interp `resources` table.
//!
//! Portability: a program plus an explicit argument list is passed straight to the
//! OS — no shell unless `opts.shell`. Signals: `kill()`/`"KILL"` is forceful
//! everywhere; `"TERM"`/`"INT"`/`"HUP"` map to the POSIX signal on unix and to
//! forceful termination on Windows (documented caveat — Windows has no POSIX signals).

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, NativeMethod, Value};
use std::cell::RefCell;
use std::process::Stdio;
use std::rc::Rc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout, Command};

/// How a child's output is captured/decoded — also decides reader chunk type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capture {
    /// Lossy-UTF-8 strings (default).
    Str,
    /// Raw bytes.
    Bytes,
    /// Child shares our stdio; nothing captured.
    Inherit,
    /// Discard.
    Null,
}

impl Capture {
    fn parse(s: &str, span: Span) -> Result<Capture, Control> {
        Ok(match s {
            "string" => Capture::Str,
            "bytes" => Capture::Bytes,
            "inherit" => Capture::Inherit,
            "null" => Capture::Null,
            other => {
                return Err(AsError::at(
                    format!("process capture must be \"string\"/\"bytes\"/\"inherit\"/\"null\", got {:?}", other),
                    span,
                )
                .into())
            }
        })
    }

    /// The `Stdio` to use for a captured stream (stdout/stderr).
    fn stdio(self) -> Stdio {
        match self {
            Capture::Str | Capture::Bytes => Stdio::piped(),
            Capture::Inherit => Stdio::inherit(),
            Capture::Null => Stdio::null(),
        }
    }
}

/// A buffered reader over one of a spawned child's output pipes. Unifies stdout and
/// stderr so the resource table holds a single type.
pub enum ProcReader {
    Out(BufReader<ChildStdout>),
    Err(BufReader<ChildStderr>),
}

impl ProcReader {
    async fn read_upto(&mut self, n: usize, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        // `read_buf` over a `take(n)` adapter appends only the bytes actually
        // available, capped at `n` — bounding the read at `n` with NO 64KB zero-fill
        // on every small read (the old `resize(n, 0)` + `truncate` did). `reserve`
        // alone is insufficient: it can over-allocate, and `read_buf` fills to the
        // vec's full spare capacity, so a hard `take(n)` cap is required.
        buf.clear();
        buf.reserve(n);
        let got = match self {
            ProcReader::Out(r) => (&mut *r).take(n as u64).read_buf(buf).await?,
            ProcReader::Err(r) => (&mut *r).take(n as u64).read_buf(buf).await?,
        };
        Ok(got)
    }

    async fn read_line_bytes(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        match self {
            ProcReader::Out(r) => r.read_until(b'\n', buf).await,
            ProcReader::Err(r) => r.read_until(b'\n', buf).await,
        }
    }

    async fn read_to_end_bytes(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        match self {
            ProcReader::Out(r) => r.read_to_end(buf).await,
            ProcReader::Err(r) => r.read_to_end(buf).await,
        }
    }
}

/// Default chunk size for `reader.read()` with no `n` argument.
const DEFAULT_CHUNK: usize = 64 * 1024;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("run", bi("process.run")), ("spawn", bi("process.spawn"))]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

fn obj(map: indexmap::IndexMap<String, Value>) -> Value {
    Value::Object(Rc::new(RefCell::new(map)))
}

/// Parsed, validated options shared by `run` and `spawn`.
struct Opts {
    cwd: Option<String>,
    /// (key, Some(value)) sets; (key, None) unsets.
    env: Vec<(String, Option<String>)>,
    clear_env: bool,
    stdin: Option<Vec<u8>>,
    shell: bool,
    timeout_ms: Option<u64>,
    check: bool,
    capture: Capture,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            cwd: None,
            env: Vec::new(),
            clear_env: false,
            stdin: None,
            shell: false,
            timeout_ms: None,
            check: false,
            capture: Capture::Str,
        }
    }
}

/// Pull a string/bytes value into raw bytes (for `stdin` / writer `write`).
fn data_to_bytes(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.borrow().clone()),
        other => Err(AsError::at(
            format!("{} expects a string or bytes, got {}", ctx, crate::interp::type_name(other)),
            span,
        )
        .into()),
    }
}

fn parse_opts(v: Option<&Value>, span: Span) -> Result<Opts, Control> {
    let mut opts = Opts::default();
    let map = match v {
        None | Some(Value::Nil) => return Ok(opts),
        Some(Value::Object(o)) => o.clone(),
        Some(other) => {
            return Err(AsError::at(
                format!("process options must be an object, got {}", crate::interp::type_name(other)),
                span,
            )
            .into())
        }
    };
    for (k, val) in map.borrow().iter() {
        match k.as_str() {
            "cwd" => opts.cwd = Some(want_string(val, span, "process cwd")?.to_string()),
            "capture" => opts.capture = Capture::parse(&want_string(val, span, "process capture")?, span)?,
            "shell" => opts.shell = val.is_truthy(),
            "clearEnv" => opts.clear_env = val.is_truthy(),
            "check" => opts.check = val.is_truthy(),
            "stdin" => opts.stdin = Some(data_to_bytes(val, span, "process stdin")?),
            "timeout" => {
                let ms = super::want_number(val, span, "process timeout")?;
                if ms < 0.0 {
                    return Err(AsError::at("process timeout must be non-negative", span).into());
                }
                opts.timeout_ms = Some(ms as u64);
            }
            "env" => {
                let env_obj = super::want_object(val, span, "process env")?;
                for (ek, ev) in env_obj.borrow().iter() {
                    match ev {
                        // A nil-valued key UNSETS the variable.
                        Value::Nil => opts.env.push((ek.clone(), None)),
                        other => opts.env.push((ek.clone(), Some(value_as_env_string(other, span)?))),
                    }
                }
            }
            // Unknown keys are ignored (forward-compatible).
            _ => {}
        }
    }
    Ok(opts)
}

/// env values: accept strings (and numbers/bools, coerced via display) as values.
fn value_as_env_string(v: &Value, span: Span) -> Result<String, Control> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        Value::Number(_) | Value::Bool(_) => Ok(v.to_string()),
        other => Err(AsError::at(
            format!("process env values must be strings, got {}", crate::interp::type_name(other)),
            span,
        )
        .into()),
    }
}

/// Build a `tokio::process::Command` from cmd/args/opts (no stdio set yet).
fn build_command(cmd: &str, args: &[Value], opts: &Opts, span: Span) -> Result<Command, Control> {
    let arg_strs: Vec<String> = {
        let mut out = Vec::new();
        for a in args {
            out.push(want_string(a, span, "process args entry")?.to_string());
        }
        out
    };

    let mut command = if opts.shell {
        // `shell: true` — non-portable convenience. The whole `cmd` string is passed
        // to the platform shell; extra `args` follow (positional $1.. on unix).
        #[cfg(unix)]
        {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(cmd);
            // sh -c "<cmd>" name arg1..: extra args become $0, $1, ...
            if !arg_strs.is_empty() {
                c.arg(cmd); // $0 placeholder
                c.args(&arg_strs);
            }
            c
        }
        #[cfg(windows)]
        {
            let mut c = Command::new("cmd.exe");
            c.arg("/C").arg(cmd);
            c.args(&arg_strs);
            c
        }
        #[cfg(not(any(unix, windows)))]
        {
            return Err(AsError::at("process shell: unsupported platform", span).into());
        }
    } else {
        let mut c = Command::new(cmd);
        c.args(&arg_strs);
        c
    };

    if let Some(cwd) = &opts.cwd {
        command.current_dir(cwd);
    }
    if opts.clear_env {
        command.env_clear();
    }
    for (k, v) in &opts.env {
        match v {
            Some(val) => {
                command.env(k, val);
            }
            None => {
                command.env_remove(k);
            }
        }
    }
    // Don't leave a zombie if the handle is dropped without wait.
    command.kill_on_drop(true);
    Ok(command)
}

/// Map a `std::process::ExitStatus` into AScript `(code, signal, success)`.
fn status_fields(status: std::process::ExitStatus) -> (Value, Value, bool) {
    let code = status.code();
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(signal_name)
    };
    #[cfg(not(unix))]
    let signal: Option<String> = None;

    let code_v = match code {
        Some(c) => Value::Number(c as f64),
        None => Value::Nil,
    };
    let signal_v = match signal {
        Some(s) => Value::Str(s.into()),
        None => Value::Nil,
    };
    let success = code == Some(0);
    (code_v, signal_v, success)
}

#[cfg(unix)]
fn signal_name(sig: i32) -> String {
    let name = match sig {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        _ => return format!("SIG{}", sig),
    };
    name.to_string()
}

fn status_object(code: Value, signal: Value, success: bool) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("code".to_string(), code);
    m.insert("signal".to_string(), signal);
    m.insert("success".to_string(), Value::Bool(success));
    obj(m)
}

/// Wrap captured bytes as Str (lossy) or Bytes per `capture`.
fn captured_value(bytes: Vec<u8>, capture: Capture) -> Value {
    match capture {
        Capture::Bytes => Value::Bytes(Rc::new(RefCell::new(bytes))),
        // Str/Inherit/Null: Inherit & Null produce empty buffers; decode lossily.
        _ => Value::Str(String::from_utf8_lossy(&bytes).into_owned().into()),
    }
}

impl Interp {
    /// Module-level dispatch for `std/process` (`run`/`spawn`).
    pub(crate) async fn call_process(
        &mut self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "run" => self.process_run(args, span).await,
            "spawn" => self.process_spawn(args, span).await,
            _ => Err(AsError::at(format!("std/process has no function '{}'", func), span).into()),
        }
    }

    async fn process_run(&mut self, args: &[Value], span: Span) -> Result<Value, Control> {
        let cmd = want_string(&arg(args, 0), span, "process.run")?;
        let arg_list = match arg(args, 1) {
            Value::Nil => Vec::new(),
            Value::Array(a) => a.borrow().clone(),
            other => {
                return Err(AsError::at(
                    format!("process.run args must be an array, got {}", crate::interp::type_name(&other)),
                    span,
                )
                .into())
            }
        };
        let opts = parse_opts(args.get(2), span)?;

        let mut command = build_command(&cmd, &arg_list, &opts, span)?;
        // stdout/stderr stdio per capture; stdin piped only if we have input to send.
        command.stdout(opts.capture.stdio());
        command.stderr(opts.capture.stdio());
        command.stdin(if opts.stdin.is_some() { Stdio::piped() } else { Stdio::null() });

        let mut child = match command.spawn() {
            Ok(c) => c,
            // SPAWN FAILURE (binary not found / permission) → the err.
            Err(e) => return Ok(err_pair(format!("process.run failed to spawn '{}': {}", cmd, e))),
        };

        // Write stdin (then drop to close), if provided.
        if let Some(input) = &opts.stdin {
            if let Some(mut sin) = child.stdin.take() {
                if let Err(e) = sin.write_all(input).await {
                    return Ok(err_pair(format!("process.run failed writing stdin: {}", e)));
                }
                // Drop closes the pipe so the child sees EOF.
                drop(sin);
            }
        }

        // Await completion with output, honoring an optional timeout.
        let wait_fut = child.wait_with_output();
        let output = match opts.timeout_ms {
            Some(ms) => match tokio::time::timeout(Duration::from_millis(ms), wait_fut).await {
                Ok(res) => res,
                Err(_elapsed) => {
                    // Timeout: the future dropped, kill_on_drop reaps the child.
                    return Ok(err_pair(format!(
                        "process.run timed out after {}ms running '{}'",
                        ms, cmd
                    )));
                }
            },
            None => wait_fut.await,
        };

        let output = match output {
            Ok(o) => o,
            Err(e) => return Ok(err_pair(format!("process.run failed: {}", e))),
        };

        let (code, signal, success) = status_fields(output.status);

        // `check: true` flips a non-zero exit into a Tier-1 err.
        if opts.check && !success {
            let stderr_text = String::from_utf8_lossy(&output.stderr).into_owned();
            let code_desc = match &code {
                Value::Number(n) => format!("exit code {}", *n as i64),
                _ => "a signal".to_string(),
            };
            return Ok(err_pair(format!(
                "process.run: '{}' failed with {}: {}",
                cmd,
                code_desc,
                stderr_text.trim_end()
            )));
        }

        let stdout_v = captured_value(output.stdout.clone(), opts.capture);
        let stderr_v = captured_value(output.stderr.clone(), opts.capture);
        let stderr_text = String::from_utf8_lossy(&output.stderr).into_owned();

        let mut result = indexmap::IndexMap::new();
        result.insert("stdout".to_string(), stdout_v);
        result.insert("stderr".to_string(), stderr_v);
        result.insert("stderrText".to_string(), Value::Str(stderr_text.into()));
        result.insert("code".to_string(), code);
        result.insert("signal".to_string(), signal);
        result.insert("success".to_string(), Value::Bool(success));
        Ok(make_pair(obj(result), Value::Nil))
    }

    async fn process_spawn(&mut self, args: &[Value], span: Span) -> Result<Value, Control> {
        let cmd = want_string(&arg(args, 0), span, "process.spawn")?;
        let arg_list = match arg(args, 1) {
            Value::Nil => Vec::new(),
            Value::Array(a) => a.borrow().clone(),
            other => {
                return Err(AsError::at(
                    format!("process.spawn args must be an array, got {}", crate::interp::type_name(&other)),
                    span,
                )
                .into())
            }
        };
        let opts = parse_opts(args.get(2), span)?;
        let capture = opts.capture;

        let mut command = build_command(&cmd, &arg_list, &opts, span)?;
        command.stdin(Stdio::piped());
        command.stdout(capture.stdio());
        command.stderr(capture.stdio());

        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => return Ok(err_pair(format!("process.spawn failed to spawn '{}': {}", cmd, e))),
        };
        let pid = child.id();

        // Register stdin/stdout/stderr as their own resources up front, recording
        // ids in the child's fields so the `stdin`/`stdout`/`stderr` accessors can
        // hand back the matching handle. Taking them off the child also means the
        // child handle holds only the process itself.
        let stdin_id = child.stdin.take().map(|w| {
            self.register_resource(NativeKind::Writer, indexmap::IndexMap::new(), ResourceState::Writer(w))
        });
        let stdout_id = child.stdout.take().map(|r| {
            self.register_resource(
                NativeKind::Reader,
                indexmap::IndexMap::new(),
                ResourceState::Reader { reader: ProcReader::Out(BufReader::new(r)), capture },
            )
        });
        let stderr_id = child.stderr.take().map(|r| {
            self.register_resource(
                NativeKind::Reader,
                indexmap::IndexMap::new(),
                ResourceState::Reader { reader: ProcReader::Err(BufReader::new(r)), capture },
            )
        });

        let mut fields = indexmap::IndexMap::new();
        fields.insert("pid".to_string(), pid.map(|p| Value::Number(p as f64)).unwrap_or(Value::Nil));
        // Stash the child-stream handles so `child.stdin`/`stdout`/`stderr` return them.
        if let Some(h) = stdin_id {
            fields.insert("stdin".to_string(), h);
        }
        if let Some(h) = stdout_id {
            fields.insert("stdout".to_string(), h);
        }
        if let Some(h) = stderr_id {
            fields.insert("stderr".to_string(), h);
        }

        let handle = self.register_resource(NativeKind::ChildProcess, fields, ResourceState::ChildProcess(child));
        Ok(make_pair(handle, Value::Nil))
    }

    /// Dispatch a method on a process child / reader / writer handle.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_process_method(
        &mut self,
        m: &Rc<NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.receiver.kind {
            NativeKind::ChildProcess => self.child_method(m, &args, span).await,
            NativeKind::Reader => self.reader_method(id, &m.method, &args, span).await,
            NativeKind::Writer => self.writer_method(id, &m.method, &args, span).await,
            _ => Err(AsError::at(format!("native handle has no method '{}'", m.method), span).into()),
        }
    }

    async fn child_method(
        &mut self,
        m: &Rc<NativeMethod>,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let id = m.receiver.id;
        match m.method.as_str() {
            // `stdin`/`stdout`/`stderr` are accessor methods returning the stream
            // handle recorded in fields at spawn (nil if that stream wasn't piped).
            "stdin" | "stdout" | "stderr" => {
                Ok(m.receiver.fields.get(m.method.as_str()).cloned().unwrap_or(Value::Nil))
            }
            "wait" => {
                // `wait` consumes the child: take it so the resource is gone afterward.
                let mut child = match self.take_resource(id) {
                    Some(ResourceState::ChildProcess(c)) => c,
                    _ => return Err(use_after_close(span)),
                };
                let result = child.wait().await;
                // After `wait()` reaps the process, finalize its stream resources
                // (stdin/stdout/stderr, whose ids are stashed in the handle's fields)
                // so any still-open pipe fds drop. Done AFTER wait so we don't close
                // the child's stdout/stderr out from under it (which would deliver
                // SIGPIPE). After `wait()` the streams are gone, so reading a stream
                // afterward returns nil (drain BEFORE wait — the normal pattern);
                // a writer used after wait degrades to a use-after-close panic.
                for name in ["stdin", "stdout", "stderr"] {
                    if let Some(Value::Native(n)) = m.receiver.fields.get(name) {
                        self.take_resource(n.id);
                    }
                }
                match result {
                    Ok(status) => {
                        let (code, signal, success) = status_fields(status);
                        Ok(status_object(code, signal, success))
                    }
                    Err(e) => Err(AsError::at(format!("child.wait failed: {}", e), span).into()),
                }
            }
            "kill" => {
                let sig = match args.first() {
                    None | Some(Value::Nil) => "KILL".to_string(),
                    Some(v) => want_string(v, span, "child.kill")?.to_string(),
                };
                let child = match self.process_child_mut(id) {
                    Some(c) => c,
                    None => return Err(use_after_close(span)),
                };
                kill_child(child, &sig, span)?;
                Ok(Value::Nil)
            }
            other => Err(AsError::at(format!("childProcess has no method '{}'", other), span).into()),
        }
    }

    async fn reader_method(
        &mut self,
        id: u64,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "read" => {
                let n = match args.first() {
                    None | Some(Value::Nil) => DEFAULT_CHUNK,
                    Some(v) => {
                        let n = super::want_number(v, span, "reader.read")?;
                        if n < 0.0 {
                            return Err(AsError::at("reader.read n must be non-negative", span).into());
                        }
                        n as usize
                    }
                };
                // read(0) is a no-op: return an empty chunk WITHOUT touching the
                // resource. (An empty read buffer yields Ok(0), which the match below
                // treats as EOF and would finalize a still-open pipe.)
                if n == 0 {
                    let capture = match self.proc_reader_mut(id) {
                        Some((_, c)) => c,
                        None => return Ok(Value::Nil), // gone → EOF
                    };
                    return Ok(captured_value(Vec::new(), capture));
                }
                // A Reader degrades to EOF (nil) once its resource is gone, rather
                // than panicking: EOF is a reader's natural terminal state.
                let (reader, capture) = match self.proc_reader_mut(id) {
                    Some(r) => r,
                    None => return Ok(Value::Nil),
                };
                let mut buf = Vec::new();
                match reader.read_upto(n, &mut buf).await {
                    Ok(0) => {
                        // EOF: finalize the reader so its pipe fd is dropped now.
                        self.take_resource(id);
                        Ok(Value::Nil)
                    }
                    Ok(_) => Ok(captured_value(buf, capture)),
                    Err(e) => Err(AsError::at(format!("reader.read failed: {}", e), span).into()),
                }
            }
            "readLine" => {
                let (reader, capture) = match self.proc_reader_mut(id) {
                    Some(r) => r,
                    None => return Ok(Value::Nil), // gone → EOF
                };
                let mut buf = Vec::new();
                match reader.read_line_bytes(&mut buf).await {
                    Ok(0) => {
                        // EOF: finalize the reader so its pipe fd is dropped now.
                        self.take_resource(id);
                        Ok(Value::Nil)
                    }
                    Ok(_) => {
                        // Strip a single trailing '\n' and an optional preceding '\r'.
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                            if buf.last() == Some(&b'\r') {
                                buf.pop();
                            }
                        }
                        Ok(captured_value(buf, capture))
                    }
                    Err(e) => Err(AsError::at(format!("reader.readLine failed: {}", e), span).into()),
                }
            }
            "readToEnd" => {
                // Note: a gone (already-drained) reader returns nil here, whereas
                // net/tcp's readToEnd returns empty bytes. The divergence is cosmetic;
                // a process reader has no retained `capture` once finalized, so an
                // empty value can't be produced in the right Str/Bytes shape.
                let (reader, capture) = match self.proc_reader_mut(id) {
                    Some(r) => r,
                    None => return Ok(Value::Nil), // gone → EOF
                };
                let mut buf = Vec::new();
                match reader.read_to_end_bytes(&mut buf).await {
                    Ok(_) => {
                        // readToEnd consumes the whole stream: finalize the reader so
                        // its pipe fd is dropped now.
                        self.take_resource(id);
                        Ok(captured_value(buf, capture))
                    }
                    Err(e) => Err(AsError::at(format!("reader.readToEnd failed: {}", e), span).into()),
                }
            }
            other => Err(AsError::at(format!("reader has no method '{}'", other), span).into()),
        }
    }

    async fn writer_method(
        &mut self,
        id: u64,
        method: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "write" => {
                let data = data_to_bytes(&arg(args, 0), span, "writer.write")?;
                let writer = match self.proc_writer_mut(id) {
                    Some(w) => w,
                    None => return Err(use_after_close(span)),
                };
                match writer.write_all(&data).await {
                    Ok(_) => Ok(Value::Nil),
                    Err(e) => Err(AsError::at(format!("writer.write failed: {}", e), span).into()),
                }
            }
            "close" => {
                // Dropping the ChildStdin closes the pipe so the child sees EOF.
                match self.take_resource(id) {
                    Some(ResourceState::Writer(mut w)) => {
                        let _ = w.shutdown().await;
                        Ok(Value::Nil)
                    }
                    _ => Err(use_after_close(span)),
                }
            }
            other => Err(AsError::at(format!("writer has no method '{}'", other), span).into()),
        }
    }
}

/// Send `sig` to a live child. `KILL` is forceful everywhere; `TERM`/`INT`/`HUP`
/// are POSIX signals on unix and map to forceful termination on Windows.
fn kill_child(child: &mut tokio::process::Child, sig: &str, span: Span) -> Result<(), Control> {
    let upper = sig.trim_start_matches("SIG").to_uppercase();
    #[cfg(unix)]
    {
        let signum: i32 = match upper.as_str() {
            "KILL" => 9,
            "TERM" => 15,
            "INT" => 2,
            "HUP" => 1,
            other => {
                return Err(AsError::at(
                    format!("child.kill: unknown signal {:?} (use KILL/TERM/INT/HUP)", other),
                    span,
                )
                .into())
            }
        };
        if let Some(pid) = child.id() {
            // SAFETY: libc::kill is a thin syscall wrapper; pid comes from our own child.
            unsafe {
                libc_kill(pid as i32, signum);
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (upper, span);
        // Windows has no POSIX signals: any kill is forceful TerminateProcess.
        let _ = child.start_kill();
        Ok(())
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

fn use_after_close(span: Span) -> Control {
    AsError::at("use after close: this process handle is closed", span).into()
}

#[cfg(all(test, unix))]
mod tests {
    use crate::interp::Interp;

    /// Lex/parse/exec `src` against a caller-held `Interp` (so the resource table
    /// can be inspected afterward). Returns the interp for `resource_count()` checks.
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

    #[tokio::test]
    async fn run_echo_captures_stdout_success_code_zero() {
        let out = run(r#"
import { run } from "std/process"
let [result, err] = run("echo", ["hi"])
print(err)
print(result.stdout)
print(result.success)
print(result.code)
"#).await;
        assert_eq!(out, "nil\nhi\n\ntrue\n0\n");
    }

    #[tokio::test]
    async fn non_zero_exit_is_not_an_err() {
        let out = run(r#"
import { run } from "std/process"
let [result, err] = run("sh", ["-c", "exit 3"])
print(err)
print(result.success)
print(result.code)
"#).await;
        // err is nil; a non-zero exit is a normal result with success=false.
        assert_eq!(out, "nil\nfalse\n3\n");
    }

    #[tokio::test]
    async fn check_true_flips_non_zero_into_err() {
        let out = run(r#"
import { run } from "std/process"
let [result, err] = run("sh", ["-c", "exit 1"], { check: true })
print(result)
print(err != nil)
"#).await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn spawn_failure_binary_not_found_is_err() {
        let out = run(r#"
import { run } from "std/process"
let [result, err] = run("definitely-not-a-real-binary-xyz", [])
print(result)
print(err != nil)
"#).await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn stdin_is_written_and_echoed_by_cat() {
        let out = run(r#"
import { run } from "std/process"
let [result, err] = run("cat", [], { stdin: "piped input" })
print(err)
print(result.stdout)
"#).await;
        assert_eq!(out, "nil\npiped input\n");
    }

    #[tokio::test]
    async fn capture_bytes_returns_bytes() {
        let out = run(r#"
import { run } from "std/process"
let [result, err] = run("echo", ["x"], { capture: "bytes" })
print(err)
print(type(result.stdout))
print(len(result.stdout))
"#).await;
        // "x\n" → 2 bytes
        assert_eq!(out, "nil\nbytes\n2\n");
    }

    #[tokio::test]
    async fn timeout_kills_child_and_returns_err() {
        let out = run(r#"
import { run } from "std/process"
let [result, err] = run("sleep", ["5"], { timeout: 50 })
print(result)
print(err != nil)
"#).await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn env_var_is_merged_onto_child() {
        let out = run(r#"
import { run } from "std/process"
let [result, err] = run("sh", ["-c", "echo $FOO"], { env: { FOO: "bar" } })
print(err)
print(result.stdout)
"#).await;
        assert_eq!(out, "nil\nbar\n\n");
    }

    #[tokio::test]
    async fn env_nil_key_unsets_variable() {
        let out = run(r#"
import { run } from "std/process"
let [r0, _] = run("sh", ["-c", "echo [$FOO]"], { env: { FOO: "bar" } })
print(r0.stdout)
let [r1, _e1] = run("sh", ["-c", "echo [$FOO]"], { env: { FOO: nil } })
print(r1.stdout)
"#).await;
        assert_eq!(out, "[bar]\n\n[]\n\n");
    }

    #[tokio::test]
    async fn spawn_reader_roundtrip_and_wait() {
        let out = run(r#"
import { spawn } from "std/process"
let [child, err] = spawn("cat", [])
print(err)
print(type(child))
await child.stdin.write("line1\n")
child.stdin.close()
let line = await child.stdout.readLine()
print(line)
let eof = await child.stdout.readLine()
print(eof)
let status = await child.wait()
print(status.success)
"#).await;
        assert_eq!(out, "nil\nchildProcess\nline1\nnil\ntrue\n");
    }

    #[tokio::test]
    async fn spawn_multiple_lines_then_eof() {
        let out = run(r#"
import { spawn } from "std/process"
let [child, err] = spawn("sh", ["-c", "echo a; echo b"])
print(err)
let l1 = await child.stdout.readLine()
let l2 = await child.stdout.readLine()
let l3 = await child.stdout.readLine()
print(l1)
print(l2)
print(l3)
let status = await child.wait()
print(status.success)
"#).await;
        assert_eq!(out, "nil\na\nb\nnil\ntrue\n");
    }

    #[tokio::test]
    async fn spawn_kill_then_wait_reports_signal() {
        let out = run(r#"
import { spawn } from "std/process"
let [child, err] = spawn("sleep", ["10"])
print(err)
print(child.pid != nil)
child.kill("TERM")
let status = await child.wait()
print(status.success)
print(status.code)
print(status.signal)
"#).await;
        assert_eq!(out, "nil\ntrue\nfalse\nnil\nSIGTERM\n");
    }

    #[tokio::test]
    async fn read_to_end_collects_all_output() {
        let out = run(r#"
import { spawn } from "std/process"
let [child, _] = spawn("sh", ["-c", "printf 'abc'"])
let all = await child.stdout.readToEnd()
print(all)
await child.wait()
"#).await;
        assert_eq!(out, "abc\n");
    }

    #[tokio::test]
    async fn read_zero_returns_empty_without_finalizing() {
        // read(0) must return an empty chunk WITHOUT finalizing the reader; a later
        // read must still drain the real output (proves the reader wasn't closed).
        let out = run(r#"
import { spawn } from "std/process"
let [child, _] = spawn("sh", ["-c", "printf 'abc'"])
let empty = await child.stdout.read(0)
print(len(empty))
let rest = await child.stdout.readToEnd()
print(rest)
await child.wait()
"#).await;
        assert_eq!(out, "0\nabc\n");
    }

    #[tokio::test]
    async fn read_after_wait_returns_nil_not_panic() {
        // `wait()` finalizes the child's streams; reading a stream afterward must
        // gracefully return nil (EOF), not panic with use-after-close.
        let out = run(r#"
import { spawn } from "std/process"
let [child, _] = spawn("sh", ["-c", "echo hi"])
let status = await child.wait()
print(status.success)
let after = await child.stdout.readLine()
print(after)
"#).await;
        assert_eq!(out, "true\nnil\n");
    }

    #[tokio::test]
    async fn spawn_drain_wait_does_not_accumulate_resources() {
        // Spawn + fully drain (readToEnd) + wait K children in a loop; afterward the
        // resource table must return to its baseline (every child + its stdin/stdout/
        // stderr stream resource reclaimed). Proves no fd accumulation.
        let mut interp = Interp::new();
        let baseline = interp.resource_count();
        run_on(
            &mut interp,
            r#"
import { spawn } from "std/process"
let i = 0
while (i < 5) {
  let [child, _] = spawn("sh", ["-c", "printf 'hello'"])
  await child.stdin.close()
  let all = await child.stdout.readToEnd()
  let drained = await child.stderr.readToEnd()
  let status = await child.wait()
  i = i + 1
}
"#,
        )
        .await;
        assert_eq!(
            interp.resource_count(),
            baseline,
            "all spawned children + streams should be reclaimed"
        );
    }
}
