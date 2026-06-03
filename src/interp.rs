//! Async tree-walking evaluator. `eval_expr`/`exec` are async to establish
//! the event-loop seam from spec §7, even though the skeleton never suspends.

use crate::ast::{BinOp, Expr, ExprKind, Stmt, UnOp};
use crate::env::{AssignError, Environment};
use crate::error::AsError;
use crate::span::Span;
use crate::value::Value;
use crate::{lexer, parser};
use async_recursion::async_recursion;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The callable parts shared by plain functions and methods: parameter list,
/// optional return contract, and the body to execute.
struct BodySpec<'a> {
    params: &'a [crate::ast::Param],
    ret: &'a Option<crate::ast::Type>,
    body: &'a [Stmt],
}

/// Non-local control-flow signal produced while executing statements.
#[derive(Debug)]
pub enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}

/// Non-local exit from expression/statement evaluation.
#[derive(Debug, Clone)]
pub enum Control {
    /// An unrecoverable programmer error (spec §6 Tier 2). Aborts unless caught
    /// by `recover`. Carries the diagnostic.
    Panic(AsError),
    /// A `?`-operator early return: the enclosing function should return this
    /// `[nil, err]` Result pair.
    Propagate(Value),
    /// `exit(code)` — unwinds to the entry point so destructors run; NOT
    /// catchable by `recover`. The i32 carries the requested exit code (0..=255
    /// validated at the call site).
    Exit(i32),
}

impl From<AsError> for Control {
    fn from(e: AsError) -> Self {
        Control::Panic(e)
    }
}

/// A fresh global environment with the built-in functions installed.
/// The bare (unqualified) builtin names installed in every program's global env.
/// Shared with the checker (`undefined-variable`) so they cannot drift.
pub const BUILTIN_NAMES: &[&str] = &[
    "print", "Ok", "Err", "assert", "recover", "test", "len", "type", "range", "exit",
];

pub fn global_env() -> Environment {
    let env = Environment::global();
    for &name in BUILTIN_NAMES {
        env.define(name, Value::Builtin(name.into()), false)
            .expect("global env starts empty");
    }
    env
}

/// Build a `[value, err]` Result pair.
// pub(crate): used by std/* modules (std/convert) later in M10.
pub(crate) fn make_pair(value: Value, err: Value) -> Value {
    Value::Array(Rc::new(RefCell::new(vec![value, err])))
}

/// Build an error object `{ message: <msg> }`.
// pub(crate): used by std/* modules (std/convert) later in M10.
pub(crate) fn make_error(msg: Value) -> Value {
    let mut map = indexmap::IndexMap::new();
    map.insert("message".to_string(), msg);
    Value::Object(Rc::new(RefCell::new(map)))
}

#[derive(Clone)]
pub struct ModuleEntry {
    pub env: Environment,
    pub exports: Rc<RefCell<HashSet<String>>>,
}

/// The non-`Clone` OS resource behind a `Value::Native` handle. Real variants are
/// feature-gated (added by sqlite/process tasks); only `Closed` is always present,
/// so under `--no-default-features` the enum has just the one variant.
pub(crate) enum ResourceState {
    #[cfg(feature = "sql")]
    SqliteConnection(rusqlite::Connection),
    // A prepared statement can't be stored directly: rusqlite's `Statement<'conn>`
    // borrows its `Connection`, which the resource table (owning both) can't model.
    // Instead we store the SQL text + the owning connection's id, and re-resolve via
    // `Connection::prepare_cached` on each `run`/`all` (rusqlite caches the parsed
    // statement internally, so there's no re-parse cost). See std/sqlite.
    #[cfg(feature = "sql")]
    SqliteStatement {
        conn_id: u64,
        sql: String,
    },
    // The process child handle requires tokio's `process` feature, which `sys`
    // enables (M13 Task 7, `std/process`). `spawn` registers the live child plus
    // its piped stdout/stderr (as `Reader`s) and stdin (as a `Writer`).
    #[cfg(feature = "sys")]
    ChildProcess(tokio::process::Child),
    // A streaming reader over one of a spawned child's pipes. `capture` is the
    // child's capture mode, which decides whether chunks come back as Str or Bytes.
    #[cfg(feature = "sys")]
    Reader {
        reader: crate::stdlib::process::ProcReader,
        capture: crate::stdlib::process::Capture,
    },
    // A streaming writer over a spawned child's stdin. `close()`/EOF takes it.
    #[cfg(feature = "sys")]
    Writer(tokio::process::ChildStdin),
    // M14 std/net/tcp: a bound listener and a buffered client/accepted stream. The
    // stream carries a `BufReader` so `readLine` works (mirrors the process Reader).
    #[cfg(feature = "net")]
    TcpListener(tokio::net::TcpListener),
    #[cfg(feature = "net")]
    TcpStream(crate::stdlib::net_tcp::TcpStreamState),
    // M14 std/net/http: a received HTTP response whose body has not yet been read.
    // `reqwest::Response::text()/bytes()/json()` consume `self` by value, so the
    // response is stored here and `take_resource`'d by the first body accessor; a
    // second body accessor on the same handle is a use-after-consume Tier-2 panic.
    #[cfg(feature = "net")]
    HttpResponse(reqwest::Response),
    // M14 std/net/http: a streaming response body (`opts.stream:true`). Wraps the
    // response's chunked byte stream in a `BufReader` so the §11.4 reader idiom
    // (`read(n?)`/`readLine()`/`readToEnd()`) applies verbatim. Finalized on EOF.
    #[cfg(feature = "net")]
    HttpBody(crate::stdlib::net_http::HttpBodyState),
    // M14 std/net/http: a cancellation token shared between a `CancelHandle` and any
    // in-flight requests passed `opts.cancel`. `cancel()` calls `notify_one()` (which
    // stores a permit); each request `tokio::select!`s its send against `notified()`.
    // The permit means a cancel issued before the request starts still aborts it,
    // which matters on the single-threaded interp where `cancel()` and the awaited
    // request run sequentially.
    #[cfg(feature = "net")]
    CancelToken(std::sync::Arc<tokio::sync::Notify>),
    // M14 std/net/http: a first-class Server-Sent Events client stream
    // (`http.sse`). Holds the live event-stream body reader, the in-progress
    // event parse buffer, the current `lastEventId`/`retry`, and the reconnect
    // template (url/headers/config) used to re-issue the GET on disconnect.
    // Boxed: `SseState` carries a sizeable BufReader + reconnect template; boxing it
    // keeps the `ResourceState` enum compact (clippy::large_enum_variant).
    #[cfg(feature = "net")]
    SseStream(Box<crate::stdlib::net_http::SseState>),
    // M14 std/http/server: a server handle's registered routes + middleware and,
    // after `bind`, the live listener. `serve` runs the sequential accept loop.
    #[cfg(feature = "net")]
    HttpServer(crate::stdlib::http_server::HttpServerState),
    // M14 std/http/server: the continuation state behind a `next` callable handed
    // to a middleware. Holds the remaining middleware chain, the index to resume
    // at, the terminal route handler, and the request. Calling `next` re-enters
    // the chain at this saved point. `Box`ed to keep the enum compact.
    #[cfg(feature = "net")]
    HttpNext(Box<crate::stdlib::http_server::NextState>),
    // M14 std/net/ws: a connected WebSocket. The client (`connect_async`) and the
    // server-accepted (`accept_async`) stream types differ — `WebSocketStream<
    // MaybeTlsStream<TcpStream>>` vs `WebSocketStream<TcpStream>`. We unify them by
    // boxing as a `dyn Sink<Message> + Stream<Item=…>` (see net_ws::WsConnState), so
    // send/recv dispatch is identical regardless of origin. `WsConnState` already
    // holds a single `Box<dyn WsStream>`, so the variant is one pointer wide.
    #[cfg(feature = "net")]
    WsConnection(crate::stdlib::net_ws::WsConnState),
    // M14 std/net/ws: an accept-based WebSocket server listener (a bound TcpListener;
    // `accept()` does the TCP accept + WebSocket handshake → WsConnection).
    #[cfg(feature = "net")]
    WsListener(tokio::net::TcpListener),
    // std/net/udp: a bound UDP datagram socket. Supports send_to/recv_from over the
    // take-out-across-await pattern (take_resource → await on owned socket →
    // return_resource). UdpSocket methods take `&self` so no &mut is needed.
    #[cfg(feature = "net")]
    UdpSocket(tokio::net::UdpSocket),
    // M15 std/tui: a terminal handle's screen buffers + cursor + raw/alt flags.
    // Boxed to keep the `ResourceState` enum compact (the two buffers are sizeable).
    #[cfg(feature = "tui")]
    Terminal(Box<crate::stdlib::tui::TerminalState>),
    // std/io: a lazily-created buffered reader over process stdin. Stored so that
    // multiple `io.readLine()` calls share ONE BufReader and buffered bytes are
    // not lost between calls. Boxed to keep the enum compact (BufReader is sizeable).
    // Created on first `readLine`/`readAll`/`readLines` call; a fixed id
    // (STDIN_RESOURCE_ID = 0xFFFF_FFFF_FFFF_FFFE) is used so the reader is found
    // across calls without scanning the table.
    #[cfg(feature = "sys")]
    StdinReader(Box<tokio::io::BufReader<tokio::io::Stdin>>),
    // std/time: a repeating interval handle. `tick()` (async) drives the
    // tokio Interval to the next tick. Boxed to keep the enum compact (the
    // tokio Interval future is sizeable).
    Interval(Box<tokio::time::Interval>),
    // std/time: a debounce wrapper state. Holds the wrapped function, the
    // window in ms, and an optional AbortHandle for the pending delayed call.
    // Explicitly aborting that AbortHandle on each new call (and in the state's
    // Drop) implements trailing-edge collapse + cancel-on-drop. Mutated via the
    // take-out-across-await pattern (take_resource → mutate → return_resource).
    DebounceWrapper(crate::stdlib::time_timers::DebounceState),
    // std/time: a throttle wrapper state. Holds the wrapped function, the
    // window in ms, and the Instant of the last successful fire.
    ThrottleWrapper(crate::stdlib::time_timers::ThrottleState),
    // std/sync: a FIFO channel backed by VecDeque + Rc<Notify>. Always present
    // (no feature gate) because tokio::sync is core infrastructure. The Channel
    // struct holds the queue inside a RefCell and the Notify handles as separate
    // Rcs so callers can clone them out before awaiting without holding a borrow.
    Channel(crate::stdlib::sync::Channel),
    // std/sync: a counting semaphore backed by RefCell<usize> + Rc<Notify>. Always
    // present (no feature gate). acquire uses the same enable()-before-recheck
    // lost-wakeup-safe park pattern as Channel::recv. The Semaphore struct holds
    // the counter inside a RefCell and the Notify as a separate Rc.
    Semaphore(crate::stdlib::sync::Semaphore),
    // std/sync: a token-bucket rate limiter. count tokens per window_ms ms.
    // Available tokens + window_start live in RefCells; Notify for wakeups.
    RateLimiter(crate::stdlib::sync::RateLimiterState),
    // std/stream: a lazy pull-based stream (a source + a chain of combinator
    // stages), driven by `Interp::pull_next`. Always present (core). Boxed to keep
    // the enum compact (a StreamState carries a Vec of stages + a source).
    Stream(Box<crate::stdlib::stream::StreamState>),
    /// A resource that has been closed/consumed. Also the always-present variant
    /// so the enum is non-empty under `--no-default-features`.
    #[allow(dead_code)]
    Closed,
}

/// Where `print` output goes. `Capture` buffers it (tests, REPL, embedders read
/// it back via `output()`); `Live` streams to stdout as produced (CLI `run`) so a
/// long-running program shows output immediately and output is not lost if the
/// program later panics.
pub enum OutputSink {
    Capture(RefCell<String>),
    Live,
}

/// std/log severity, ordered debug<info<warn<error for level filtering.
#[cfg(feature = "log")]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

/// Parse the initial std/log level from the `ASCRIPT_LOG` env value
/// (case-insensitive `debug`/`info`/`warn`/`error`). Defaults to `Info` when
/// unset or unrecognized. Pure (no env access) so it's race-free to unit-test.
#[cfg(feature = "log")]
fn log_level_from_env_str(v: Option<&str>) -> LogLevel {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("debug") => LogLevel::Debug,
        Some("info") => LogLevel::Info,
        Some("warn") => LogLevel::Warn,
        Some("error") => LogLevel::Error,
        _ => LogLevel::Info,
    }
}
/// std/log output format: `human` (`[WARN] msg key=val`) or `json` (one object/line).
#[cfg(feature = "log")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Human,
    Json,
}

/// All mutable interpreter state lives behind interior mutability (`RefCell`/
/// `Cell`) so the `eval`/`exec`/`call_*` methods take `&self`, not `&mut self`.
/// This lets multiple concurrent eval futures (M17 Phase 2+) share one
/// `Rc<Interp>` while mutating through short-lived borrows. Borrow rule: never
/// hold a `RefCell` guard across an `.await` — take the resource OUT of the table
/// first (`take_resource`) and put it back after (`return_resource`).
pub struct Interp {
    /// Where `print` output goes. See [`OutputSink`].
    output: OutputSink,
    modules: RefCell<HashMap<PathBuf, ModuleEntry>>,
    module_dir: RefCell<PathBuf>,
    current_exports: RefCell<Rc<RefCell<HashSet<String>>>>,
    /// Tests registered via the `test(name, fn)` builtin. Only executed by
    /// `ascript test` (via `run_registered_tests`); a normal `run` just collects them.
    tests: RefCell<Vec<(String, Value)>>,
    /// Live OS resources backing `Value::Native` handles, keyed by handle id.
    resources: RefCell<HashMap<u64, ResourceState>>,
    /// Monotonic id source for newly registered resources.
    next_resource_id: Cell<u64>,
    /// A `Weak` back to the owning `Rc<Interp>`, installed by `install_self`
    /// right after construction. Lets `&self` methods recover an owned
    /// `Rc<Interp>` (`rc()`) so they can `spawn_local` a `'static` task that
    /// re-enters the interpreter — required for M17 Phase 2 async-fn scheduling.
    self_weak: RefCell<std::rc::Weak<Interp>>,
    /// A `Weak` to the bytecode [`Vm`] driving this interpreter, installed by
    /// [`Vm::new`] via [`Interp::set_vm`]. Lets a native higher-order stdlib
    /// function (e.g. `array.map`, `recover`) re-enter the VM to run a
    /// `Value::Closure` callback (see [`Interp::call_value`]'s `Closure` arm).
    /// `None` (an empty `Weak`) on a pure tree-walker run where no VM exists; a
    /// `Value::Closure` can only be produced by the VM, so a VM is always
    /// registered whenever a closure can reach `call_value`.
    vm: RefCell<std::rc::Weak<crate::vm::Vm>>,
    /// Number of eagerly-spawned `async fn`/method body tasks currently alive
    /// (incremented at spawn, decremented when the task future drops — completion
    /// OR cancel-on-drop). Used for cooperative backpressure so a tight un-awaited
    /// loop can't accumulate cancelled-but-unreaped tasks without bound.
    inflight: Cell<u64>,
    /// High-water mark of `inflight` over the program's life. Exposed for tests
    /// that assert async-task memory stays bounded (does not scale with N).
    max_inflight: Cell<u64>,
    /// std/log minimum level (records below it are dropped). Default `Info`.
    #[cfg(feature = "log")]
    log_level: std::cell::Cell<LogLevel>,
    /// std/log output format. Default `Human`.
    #[cfg(feature = "log")]
    log_format: std::cell::Cell<LogFormat>,
    /// std/log capture buffer (used under `OutputSink::Capture`, i.e. tests).
    #[cfg(feature = "log")]
    log_capture: RefCell<String>,
    /// The script's own CLI arguments (`ascript run file.as <args...>`).
    /// Excludes the binary name and the script path — only the trailing args.
    /// Empty unless set by [`Interp::set_cli_args`] (i.e. the REPL and test
    /// runner always see `[]`, which is correct).
    cli_args: RefCell<Vec<Rc<str>>>,
}

/// Above this many in-flight async tasks, an async-fn call cooperatively yields
/// after spawning so the executor can reap finished/cancelled tasks. Keeps a
/// no-await loop of un-awaited async calls bounded instead of growing to N.
const INFLIGHT_YIELD_CAP: u64 = 256;

/// Fixed resource-table id for the lazily-created stdin `BufReader` (std/io).
/// Uses a sentinel near `u64::MAX` so it never collides with auto-incrementing ids
/// (which start at 0 and count up). The `sys` gate matches the StdinReader variant.
#[cfg(feature = "sys")]
pub(crate) const STDIN_RESOURCE_ID: u64 = 0xFFFF_FFFF_FFFF_FFFE;

/// Decrements `Interp::inflight` when dropped. Created at spawn time and moved
/// into the spawned task so the count tracks the task's real lifetime — it
/// decrements whether the task completes, errors, or is aborted (cancel-on-drop).
pub(crate) struct InflightGuard {
    vm: Rc<Interp>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.vm
            .inflight
            .set(self.vm.inflight.get().saturating_sub(1));
    }
}

/// Outcome of running the tests registered on an `Interp`.
#[derive(Debug, Default)]
pub struct TestSummary {
    pub passed: usize,
    pub failed: usize,
    /// `(test name, failure message)` for each failed test.
    pub failures: Vec<(String, String)>,
}

impl Interp {
    pub fn new() -> Self {
        Self::with_sink(OutputSink::Capture(RefCell::new(String::new())))
    }

    /// Like [`Interp::new`] but streams `print` output to stdout immediately
    /// (CLI `run`) instead of buffering it. See [`OutputSink`].
    pub fn new_live() -> Self {
        Self::with_sink(OutputSink::Live)
    }

    fn with_sink(output: OutputSink) -> Self {
        Interp {
            output,
            modules: RefCell::new(HashMap::new()),
            module_dir: RefCell::new(PathBuf::from(".")),
            current_exports: RefCell::new(Rc::new(RefCell::new(HashSet::new()))),
            tests: RefCell::new(Vec::new()),
            resources: RefCell::new(HashMap::new()),
            next_resource_id: Cell::new(0),
            self_weak: RefCell::new(std::rc::Weak::new()),
            vm: RefCell::new(std::rc::Weak::new()),
            inflight: Cell::new(0),
            max_inflight: Cell::new(0),
            #[cfg(feature = "log")]
            log_level: Cell::new(log_level_from_env_str(
                std::env::var("ASCRIPT_LOG").ok().as_deref(),
            )),
            #[cfg(feature = "log")]
            log_format: Cell::new(LogFormat::Human),
            #[cfg(feature = "log")]
            log_capture: RefCell::new(String::new()),
            cli_args: RefCell::new(Vec::new()),
        }
    }

    /// Store the script's trailing CLI arguments so `env.args()` can return them.
    /// Called by `run_file` after construction, before execution.
    pub fn set_cli_args(&self, args: &[String]) {
        *self.cli_args.borrow_mut() = args.iter().map(|s| Rc::from(s.as_str())).collect();
    }

    /// Return the stored CLI args as a `Value::Array` of strings.
    /// Called from `env.args` (sys-gated) and `cli.parse` (always available).
    pub(crate) fn get_cli_args(&self) -> Value {
        let args: Vec<Value> = self
            .cli_args
            .borrow()
            .iter()
            .map(|s| Value::Str(s.clone()))
            .collect();
        Value::Array(Rc::new(RefCell::new(args)))
    }

    /// Register one newly-spawned async task: bump `inflight` (and the high-water
    /// mark) and return a guard that decrements when the task future is dropped.
    pub(crate) fn inflight_guard(&self) -> InflightGuard {
        let n = self.inflight.get() + 1;
        self.inflight.set(n);
        if n > self.max_inflight.get() {
            self.max_inflight.set(n);
        }
        InflightGuard { vm: self.rc() }
    }

    /// Cooperative backpressure: if many async tasks are in flight, yield once so
    /// the executor can drive/reap them. Called by async-fn/method call sites
    /// after spawning. A normal awaiting program reaps continuously and rarely
    /// trips this; a tight un-awaited loop trips it and stays bounded.
    pub(crate) async fn maybe_yield_for_inflight(&self) {
        if self.inflight.get() >= INFLIGHT_YIELD_CAP {
            tokio::task::yield_now().await;
        }
    }

    /// Current number of in-flight async tasks (test/diagnostic hook).
    pub fn inflight_count(&self) -> u64 {
        self.inflight.get()
    }

    /// High-water mark of in-flight async tasks since program start (test hook:
    /// asserts async-task memory stays bounded and does not scale with workload).
    pub fn max_inflight(&self) -> u64 {
        self.max_inflight.get()
    }

    /// Install the self-reference so `&self` methods can obtain an owned
    /// `Rc<Interp>` via `rc()`. MUST be called immediately after `Rc::new(Interp::new())`
    /// at every entry point, before running any program.
    pub(crate) fn install_self(self: &Rc<Interp>) {
        *self.self_weak.borrow_mut() = Rc::downgrade(self);
    }

    /// Register the bytecode [`Vm`] driving this interpreter. Called by
    /// [`Vm::new`] right after the VM is constructed, so that a native
    /// higher-order stdlib function reaching a `Value::Closure` in
    /// [`Interp::call_value`] can re-enter the VM to run it (`native → VM`).
    pub(crate) fn set_vm(&self, vm: std::rc::Weak<crate::vm::Vm>) {
        *self.vm.borrow_mut() = vm;
    }

    /// Recover an owned `Rc<Vm>` from the registered weak, or `None` if no VM is
    /// installed. Upgrading to an owned `Rc` lets callers drop the `RefCell`
    /// borrow before awaiting (`await_holding_refcell_ref` stays clean).
    pub(crate) fn vm(&self) -> Option<Rc<crate::vm::Vm>> {
        self.vm.borrow().upgrade()
    }

    /// Recover an owned `Rc<Interp>` from `&self`. Panics if `install_self` was
    /// never called (an entry-point bug).
    pub(crate) fn rc(&self) -> Rc<Interp> {
        self.self_weak
            .borrow()
            .upgrade()
            .expect("Interp self-ref not installed")
    }

    /// Snapshot of all captured program output so far. Empty under `Live`.
    pub fn output(&self) -> String {
        match &self.output {
            OutputSink::Capture(buf) => buf.borrow().clone(),
            OutputSink::Live => String::new(),
        }
    }

    /// Emit program output (`print`). Buffers under `Capture`, streams to stdout
    /// under `Live`.
    pub(crate) fn push_output(&self, s: &str) {
        match &self.output {
            OutputSink::Capture(buf) => buf.borrow_mut().push_str(s),
            OutputSink::Live => {
                use std::io::Write;
                let mut so = std::io::stdout().lock();
                let _ = so.write_all(s.as_bytes());
                let _ = so.flush();
            }
        }
    }

    /// Emit one std/log record line. Buffers into `log_capture` under `Capture`
    /// (tests read it via `log_output`); writes to stderr under `Live`.
    #[cfg(feature = "log")]
    pub(crate) fn emit_log(&self, line: &str) {
        match &self.output {
            OutputSink::Capture(_) => {
                let mut b = self.log_capture.borrow_mut();
                b.push_str(line);
                b.push('\n');
            }
            OutputSink::Live => {
                use std::io::Write;
                let mut e = std::io::stderr().lock();
                let _ = writeln!(e, "{}", line);
            }
        }
    }

    /// Snapshot of all captured std/log output (test hook). Empty under `Live`.
    #[cfg(feature = "log")]
    pub fn log_output(&self) -> String {
        self.log_capture.borrow().clone()
    }

    /// Set the minimum std/log level.
    #[cfg(feature = "log")]
    pub(crate) fn set_log_level(&self, l: LogLevel) {
        self.log_level.set(l);
    }

    /// Set the std/log output format.
    #[cfg(feature = "log")]
    pub(crate) fn set_log_format(&self, f: LogFormat) {
        self.log_format.set(f);
    }

    /// `std/log` dispatch. `setLevel`/`setFormat` mutate per-interp state;
    /// `debug`/`info`/`warn`/`error` build a record (first string arg → `msg`,
    /// object args merge as fields, auto `level`) and emit it via [`emit_log`],
    /// but only when the level passes the filter — a thunk first arg (a function)
    /// is invoked ONLY then, so a filtered `log.debug(() => expensive())` is free.
    /// Serialization is total (`json::to_json_lossy`) so logging never panics.
    #[cfg(feature = "log")]
    pub(crate) async fn call_log(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let level_of = |f: &str| match f {
            "debug" => Some(LogLevel::Debug),
            "info" => Some(LogLevel::Info),
            "warn" => Some(LogLevel::Warn),
            "error" => Some(LogLevel::Error),
            _ => None,
        };
        match func {
            "setLevel" => {
                let s = match args.first() {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => {
                        return Err(AsError::at("log.setLevel expects a level string", span).into())
                    }
                };
                match level_of(&s) {
                    Some(l) => {
                        self.set_log_level(l);
                        Ok(Value::Nil)
                    }
                    None => Err(AsError::at(format!("unknown log level {:?}", s), span).into()),
                }
            }
            "setFormat" => {
                let s = match args.first() {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => {
                        return Err(AsError::at(
                            "log.setFormat expects \"human\" or \"json\"",
                            span,
                        )
                        .into())
                    }
                };
                match s.as_str() {
                    "human" => {
                        self.set_log_format(LogFormat::Human);
                        Ok(Value::Nil)
                    }
                    "json" => {
                        self.set_log_format(LogFormat::Json);
                        Ok(Value::Nil)
                    }
                    o => Err(AsError::at(format!("unknown log format {:?}", o), span).into()),
                }
            }
            "debug" | "info" | "warn" | "error" => {
                let lvl = level_of(func).unwrap();
                if lvl < self.log_level.get() {
                    return Ok(Value::Nil);
                }
                let mut parts: Vec<String> = Vec::new();
                let mut fields = serde_json::Map::new();
                let mut iter = args.iter();
                // A thunk is only honored as the FIRST arg. It is invoked lazily
                // (after the level filter above) so a filtered call is free.
                if matches!(
                    args.first(),
                    Some(Value::Function(_)) | Some(Value::Builtin(_))
                ) {
                    let r = self.call_value(args[0].clone(), vec![], span).await?;
                    // An `async fn` thunk returns a `Value::Future`; drive it to
                    // completion using the same mechanism as `await` (M17).
                    let r = match r {
                        Value::Future(f) => f.get().await?,
                        other => other,
                    };
                    parts.push(r.to_string());
                    iter.next(); // consume index 0
                }
                for a in iter {
                    match a {
                        Value::Object(o) => {
                            for (k, val) in o.borrow().iter() {
                                fields.insert(
                                    k.clone(),
                                    crate::stdlib::json::to_json_lossy(val, &mut Vec::new()),
                                );
                            }
                        }
                        other => parts.push(other.to_string()),
                    }
                }
                let msg = parts.join(" ");
                let line = match self.log_format.get() {
                    LogFormat::Json => {
                        let mut rec = serde_json::Map::new();
                        // User fields FIRST, then reserved keys, so a user field
                        // named `level`/`msg` can never clobber the authoritative ones.
                        for (k, v) in fields {
                            rec.insert(k, v);
                        }
                        rec.insert("level".into(), serde_json::Value::String(func.into()));
                        rec.insert("msg".into(), serde_json::Value::String(msg));
                        serde_json::Value::Object(rec).to_string()
                    }
                    LogFormat::Human => {
                        let mut s = if msg.is_empty() {
                            format!("[{}]", func.to_uppercase())
                        } else {
                            format!("[{}] {}", func.to_uppercase(), msg)
                        };
                        for (k, v) in &fields {
                            let vs = match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            s.push_str(&format!(" {}={}", k, vs));
                        }
                        s
                    }
                };
                self.emit_log(&line);
                Ok(Value::Nil)
            }
            other => Err(AsError::at(format!("std/log has no function '{}'", other), span).into()),
        }
    }

    /// Is the captured output buffer empty? (REPL flush check.) Always true under
    /// `Live`.
    pub(crate) fn output_is_empty(&self) -> bool {
        match &self.output {
            OutputSink::Capture(buf) => buf.borrow().is_empty(),
            OutputSink::Live => true,
        }
    }

    /// Clear the captured output buffer (REPL flushes after each line). No-op
    /// under `Live`.
    pub(crate) fn clear_output(&self) {
        if let OutputSink::Capture(buf) = &self.output {
            buf.borrow_mut().clear();
        }
    }

    /// Allocate the next monotonic resource id.
    fn next_id(&self) -> u64 {
        let id = self.next_resource_id.get();
        self.next_resource_id.set(id + 1);
        id
    }

    /// Return a resource to the table after a take-out across an `.await`. Pairs
    /// with `take_resource`. Used unconditionally by std/time timers, plus the
    /// feature-gated I/O modules.
    pub(crate) fn return_resource(&self, id: u64, state: ResourceState) {
        self.resources.borrow_mut().insert(id, state);
    }

    /// Register an OS `state` behind a fresh `Value::Native` handle of `kind`,
    /// carrying the plain readable `fields`. Used unconditionally by std/time
    /// timers, plus the feature-gated modules (sqlite/process/net/...).
    pub(crate) fn register_resource(
        &self,
        kind: crate::value::NativeKind,
        fields: indexmap::IndexMap<String, Value>,
        state: ResourceState,
    ) -> Value {
        let id = self.next_id();
        self.resources.borrow_mut().insert(id, state);
        Value::Native(std::rc::Rc::new(crate::value::NativeObject {
            id,
            kind,
            fields,
        }))
    }

    /// Remove and return the resource for `id` (used by `close`/`kill`/EOF, and to
    /// own a resource across an `.await` without holding the table borrow — pair
    /// with `return_resource`). Used unconditionally by std/time timers, plus the
    /// feature-gated modules (sqlite/process/net/...).
    pub(crate) fn take_resource(&self, id: u64) -> Option<ResourceState> {
        self.resources.borrow_mut().remove(&id)
    }

    /// Run `f` with a shared borrow of the resource for `id` (handle methods that
    /// only inspect state, e.g. `conn.query` re-resolving a statement). The closure
    /// must NOT `.await` — the borrow is held for its duration.
    #[allow(dead_code)]
    pub(crate) fn with_resource<R>(
        &self,
        id: u64,
        f: impl FnOnce(Option<&ResourceState>) -> R,
    ) -> R {
        f(self.resources.borrow().get(&id))
    }

    /// Like [`with_resource`], but with a mutable borrow. The closure must NOT
    /// `.await` (the borrow is held for its duration); used by synchronous handle
    /// mutations such as feeding bytes to an SSE parser between async chunk reads.
    #[allow(dead_code)]
    pub(crate) fn with_resource_mut<R>(
        &self,
        id: u64,
        f: impl FnOnce(Option<&mut ResourceState>) -> R,
    ) -> R {
        f(self.resources.borrow_mut().get_mut(&id))
    }

    /// Number of live OS resources in the table. Tests use this to prove that
    /// stream/child resources are reclaimed (no fd accumulation across spawns).
    /// Only exercised by the `sys` process tests, hence dead under other configs.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn resource_count(&self) -> usize {
        self.resources.borrow().len()
    }

    /// A shared borrow of the live `rusqlite::Connection` behind a handle id (as a
    /// `Ref` guard), or `None` if the handle was closed (`take_resource`'d). Sqlite
    /// work is synchronous, so the guard never lives across an `.await`.
    #[cfg(feature = "sql")]
    pub(crate) fn sqlite_conn(&self, id: u64) -> Option<std::cell::Ref<'_, rusqlite::Connection>> {
        std::cell::Ref::filter_map(self.resources.borrow(), |m| match m.get(&id) {
            Some(ResourceState::SqliteConnection(c)) => Some(c),
            _ => None,
        })
        .ok()
    }

    /// Take the live `reqwest::Response` behind a handle id, removing it from the
    /// table. `None` if it was already consumed (a body accessor took it). The
    /// caller turns `None` into the "response body already consumed" Tier-2 panic.
    #[cfg(feature = "net")]
    pub(crate) fn take_http_response(&self, id: u64) -> Option<reqwest::Response> {
        match self.resources.borrow_mut().remove(&id) {
            Some(ResourceState::HttpResponse(r)) => Some(r),
            // Not an HttpResponse (or already gone): nothing to return. If it was a
            // different live resource, put it back is unnecessary — ids are unique
            // per kind by construction, so this branch means "already consumed".
            _ => None,
        }
    }

    /// Drop the un-consumed `HttpNext` continuations belonging to ONE dispatch
    /// (identified by `dispatch_id`). A middleware that short-circuits (returns
    /// without calling `next`) leaves its continuation behind; the server sweeps it
    /// after each request so per-request handles don't accumulate. The sweep is
    /// scoped to the dispatch so concurrent connections (each handled on its own
    /// task) never drop one another's still-pending continuations.
    #[cfg(feature = "net")]
    pub(crate) fn drop_pending_http_next(&self, dispatch_id: u64) {
        self.resources.borrow_mut().retain(|_, s| match s {
            ResourceState::HttpNext(state) => state.dispatch_id != dispatch_id,
            _ => true,
        });
    }

    /// Allocate a fresh monotonic id identifying one `dispatch_request` so its
    /// `HttpNext` continuations can be swept without touching other concurrent
    /// dispatches. Reuses the resource-id counter (ids are unique either way).
    #[cfg(feature = "net")]
    pub(crate) fn next_http_dispatch_id(&self) -> u64 {
        self.next_id()
    }

    /// A mutable borrow of an HTTP server's routes/middleware/listener (as a `RefMut`
    /// guard), or `None` if the handle is gone. Used by the synchronous `route`/`use`
    /// builders; `bind`/`serve` take the listener out (`take_resource`) before
    /// awaiting so no guard is held across an `.await`.
    #[cfg(feature = "net")]
    pub(crate) fn http_server_mut(
        &self,
        id: u64,
    ) -> Option<std::cell::RefMut<'_, crate::stdlib::http_server::HttpServerState>> {
        std::cell::RefMut::filter_map(self.resources.borrow_mut(), |m| match m.get_mut(&id) {
            Some(ResourceState::HttpServer(s)) => Some(s),
            _ => None,
        })
        .ok()
    }

    /// A mutable borrow of a `Terminal` handle's screen state (as a `RefMut` guard),
    /// or `None` once the handle was closed. Crossterm I/O is synchronous, so the
    /// guard never lives across an `.await`.
    #[cfg(feature = "tui")]
    pub(crate) fn terminal_mut(
        &self,
        id: u64,
    ) -> Option<std::cell::RefMut<'_, crate::stdlib::tui::TerminalState>> {
        std::cell::RefMut::filter_map(self.resources.borrow_mut(), |m| match m.get_mut(&id) {
            Some(ResourceState::Terminal(s)) => Some(&mut **s),
            _ => None,
        })
        .ok()
    }

    /// Run every test registered via the `test(name, fn)` builtin. Each test fn
    /// is invoked with no arguments; a `Control::Panic` (e.g. a failed `assert`)
    /// is recorded as a failure, while a clean return or a `?` propagation passes.
    /// Returns `Err(Control::Exit)` if a test calls `exit()` — that unwinds the
    /// test runner rather than being counted as a pass or fail.
    pub async fn run_registered_tests(&self) -> Result<TestSummary, Control> {
        let mut summary = TestSummary::default();
        // Clone out the registrations first so the table borrow is not held across
        // each `call_value` await.
        let tests = self.tests.borrow().clone();
        for (name, func) in tests {
            match self.call_value(func, Vec::new(), Span::new(0, 0)).await {
                Ok(_) | Err(Control::Propagate(_)) => summary.passed += 1,
                Err(Control::Panic(e)) => {
                    summary.failed += 1;
                    summary.failures.push((name, e.message));
                }
                // exit() inside a test function surfaces the exit request; re-propagate
                // so the test runner unwinds rather than recording it as pass/fail.
                Err(Control::Exit(code)) => return Err(Control::Exit(code)),
            }
        }
        Ok(summary)
    }

    /// Load (or fetch from cache) the module at `path`, returning its entry.
    #[async_recursion(?Send)]
    pub async fn load_module(&self, path: &Path) -> Result<ModuleEntry, Control> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(entry) = self.modules.borrow().get(&canon) {
            return Ok(entry.clone()); // cached, or in-progress (circular)
        }
        let src = std::fs::read_to_string(&canon).map_err(|e| {
            Control::Panic(AsError::new(format!(
                "cannot read module {}: {}",
                canon.display(),
                e
            )))
        })?;
        // Child of the global (builtins) env so module-level definitions and
        // imports can shadow builtins (resolution walks up to find builtins).
        let env = global_env().child();
        let exports = Rc::new(RefCell::new(HashSet::new()));
        let entry = ModuleEntry {
            env: env.clone(),
            exports: exports.clone(),
        };
        // Cache BEFORE executing so circular imports resolve to this entry.
        self.modules
            .borrow_mut()
            .insert(canon.clone(), entry.clone());

        let dir = canon
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let prev_dir = self.module_dir.replace(dir);
        let prev_exports = self.current_exports.replace(exports);

        let src_info = Rc::new(crate::error::SourceInfo {
            path: canon.display().to_string(),
            text: src.clone(),
        });

        let tokens =
            lexer::lex(&src).map_err(|e| Control::Panic(e.with_source(src_info.clone())))?;
        let program =
            parser::parse(&tokens).map_err(|e| Control::Panic(e.with_source(src_info.clone())))?;
        let result = self.exec(&program, &env).await;

        *self.module_dir.borrow_mut() = prev_dir;
        *self.current_exports.borrow_mut() = prev_exports;

        if let Err(Control::Panic(e)) = result {
            return Err(Control::Panic(e.with_source(src_info)));
        }
        result?; // propagate any other control flow from the module body
        Ok(entry)
    }

    /// Resolve a `std/*` built-in module to a cached `ModuleEntry`, building it
    /// from the static export registry. Bypasses the filesystem entirely.
    fn load_std_module(&self, source: &str) -> Result<ModuleEntry, Control> {
        let key = PathBuf::from(format!("<std>/{}", &source[4..]));
        if let Some(entry) = self.modules.borrow().get(&key) {
            return Ok(entry.clone());
        }
        let exports_list = crate::stdlib::std_module_exports(source).ok_or_else(|| {
            Control::Panic(AsError::new(format!(
                "unknown standard library module '{}'",
                source
            )))
        })?;
        // Child of the global env so an export whose name collides with a global
        // builtin (e.g. std/regex exports `test`) shadows it rather than erroring.
        let env = global_env().child();
        let exports = Rc::new(RefCell::new(HashSet::new()));
        for (name, value) in exports_list {
            env.define(&name, value, false).map_err(AsError::new)?;
            exports.borrow_mut().insert(name);
        }
        let entry = ModuleEntry { env, exports };
        self.modules.borrow_mut().insert(key, entry.clone());
        Ok(entry)
    }

    fn resolve_import(&self, source: &str) -> PathBuf {
        let mut p = self.module_dir.borrow().join(source);
        if p.extension().is_none() {
            p.set_extension("as");
        }
        p
    }

    #[async_recursion(?Send)]
    pub async fn exec(&self, program: &[Stmt], env: &Environment) -> Result<Flow, Control> {
        for stmt in program {
            match self.exec_stmt(stmt, env).await? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    #[async_recursion(?Send)]
    async fn exec_stmt(&self, stmt: &Stmt, env: &Environment) -> Result<Flow, Control> {
        match stmt {
            Stmt::Expr(e) => {
                self.eval_expr(e, env).await?;
                Ok(Flow::Normal)
            }
            Stmt::Let {
                name,
                ty,
                value,
                mutable,
                ..
            } => {
                let v = match value {
                    Some(value) => {
                        let v = self.eval_expr(value, env).await?;
                        if let Some(ty) = ty {
                            if !check_type(&v, ty) {
                                return Err(contract_panic(ty, &v, value.span));
                            }
                        }
                        v
                    }
                    // `let x` / `let x: T` with no initializer binds nil. The type
                    // annotation is not enforced here: there is no value to check,
                    // and the language does not contract-check later assignments.
                    None => Value::Nil,
                };
                env.define(name, v, *mutable).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::LetDestructure {
                names,
                rest,
                value,
                mutable,
                ..
            } => {
                let v = self.eval_expr(value, env).await?;
                let items = match v {
                    Value::Array(a) => a.borrow().clone(),
                    other => {
                        return Err(AsError::at(
                            format!(
                                "cannot destructure a non-array value of type {}",
                                type_name(&other)
                            ),
                            value.span,
                        )
                        .into())
                    }
                };
                for (i, name) in names.iter().enumerate() {
                    let elem = items.get(i).cloned().unwrap_or(Value::Nil);
                    env.define(name, elem, *mutable).map_err(AsError::new)?;
                }
                if let Some((rest_name, _)) = rest {
                    let tail: Vec<Value> = items.iter().skip(names.len()).cloned().collect();
                    let arr = Value::Array(std::rc::Rc::new(std::cell::RefCell::new(tail)));
                    env.define(rest_name, arr, *mutable).map_err(AsError::new)?;
                }
                Ok(Flow::Normal)
            }
            Stmt::LetDestructureObject {
                bindings,
                rest,
                value,
                mutable,
                ..
            } => {
                let v = self.eval_expr(value, env).await?;
                if !matches!(v, Value::Object(_) | Value::Instance(_)) {
                    return Err(AsError::at(
                        format!(
                            "cannot destructure a non-object value of type {}",
                            type_name(&v)
                        ),
                        value.span,
                    )
                    .into());
                }
                let get = |key: &str| -> Value {
                    match &v {
                        Value::Object(o) => o.borrow().get(key).cloned().unwrap_or(Value::Nil),
                        Value::Instance(i) => {
                            i.borrow().fields.get(key).cloned().unwrap_or(Value::Nil)
                        }
                        _ => Value::Nil,
                    }
                };
                for b in bindings {
                    env.define(&b.binding, get(&b.key), *mutable)
                        .map_err(AsError::new)?;
                }
                if let Some((rest_name, _)) = rest {
                    let bound: std::collections::HashSet<&str> =
                        bindings.iter().map(|b| b.key.as_str()).collect();
                    let mut remaining = indexmap::IndexMap::new();
                    match &v {
                        Value::Object(o) => {
                            for (k, val) in o.borrow().iter() {
                                if !bound.contains(k.as_str()) {
                                    remaining.insert(k.clone(), val.clone());
                                }
                            }
                        }
                        Value::Instance(i) => {
                            for (k, val) in i.borrow().fields.iter() {
                                if !bound.contains(k.as_str()) {
                                    remaining.insert(k.clone(), val.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                    let obj = Value::Object(std::rc::Rc::new(std::cell::RefCell::new(remaining)));
                    env.define(rest_name, obj, *mutable).map_err(AsError::new)?;
                }
                Ok(Flow::Normal)
            }
            Stmt::Block(stmts) => {
                let child = env.child();
                self.exec(stmts, &child).await
            }
            Stmt::If {
                cond,
                then_branch,
                else_branch,
            } => {
                if self.eval_expr(cond, env).await?.is_truthy() {
                    let child = env.child();
                    self.exec(then_branch, &child).await
                } else if let Some(else_stmts) = else_branch {
                    let child = env.child();
                    self.exec(else_stmts, &child).await
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::While { cond, body } => {
                while self.eval_expr(cond, env).await?.is_truthy() {
                    let child = env.child();
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::ForRange {
                var,
                start,
                end,
                body,
            } => {
                let start_v = self.eval_expr(start, env).await?;
                let end_v = self.eval_expr(end, env).await?;
                let (lo, hi) = match (start_v, end_v) {
                    (Value::Number(a), Value::Number(b)) => (a, b),
                    _ => {
                        return Err(
                            AsError::at("for-range bounds must be numbers", start.span).into()
                        )
                    }
                };
                let mut i = lo;
                while i < hi {
                    let child = env.child();
                    child
                        .define(var, Value::Number(i), false)
                        .map_err(AsError::new)?;
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                    i += 1.0;
                }
                Ok(Flow::Normal)
            }
            Stmt::ForOf {
                var,
                iter,
                body,
                for_await,
            } => {
                let iterable = self.eval_expr(iter, env).await?;
                if *for_await {
                    return self
                        .exec_for_await(var, iterable, body, env, iter.span)
                        .await;
                }
                let items: Vec<Value> = match iterable {
                    Value::Array(arr) => arr.borrow().clone(),
                    Value::Str(s) => s
                        .chars()
                        .map(|c| Value::Str(c.to_string().into()))
                        .collect(),
                    other => {
                        return Err(AsError::at(
                            format!("value of type {} is not iterable", type_name(&other)),
                            iter.span,
                        )
                        .into())
                    }
                };
                for item in items {
                    let child = env.child();
                    child.define(var, item, false).map_err(AsError::new)?;
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval_expr(e, env).await?,
                    None => Value::Nil,
                };
                Ok(Flow::Return(v))
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Fn {
                name,
                params,
                ret,
                body,
                is_async,
                is_generator,
                ..
            } => {
                let func = Value::Function(std::rc::Rc::new(crate::value::Function {
                    name: Some(name.clone()),
                    params: params.clone(),
                    ret: ret.clone(),
                    body: body.clone(),
                    closure: env.clone(),
                    is_async: *is_async,
                    is_generator: *is_generator,
                }));
                env.define(name, func, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::Enum { name, variants, .. } => {
                let mut map = indexmap::IndexMap::new();
                for v in variants {
                    let backing = match &v.value {
                        Some(e) => self.eval_expr(e, env).await?,
                        None => Value::Nil,
                    };
                    let variant = Value::EnumVariant(std::rc::Rc::new(crate::value::EnumVariant {
                        enum_name: name.clone(),
                        name: v.name.clone(),
                        value: backing,
                    }));
                    map.insert(v.name.clone(), variant);
                }
                let def = Value::Enum(std::rc::Rc::new(crate::value::EnumDef {
                    name: name.clone(),
                    variants: map,
                }));
                env.define(name, def, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::Class {
                name,
                superclass,
                fields,
                methods,
                ..
            } => {
                let parent = match superclass {
                    Some(sup_name) => match env.get(sup_name) {
                        Some(Value::Class(c)) => Some(c),
                        Some(_) => {
                            return Err(
                                AsError::new(format!("'{}' is not a class", sup_name)).into()
                            )
                        }
                        None => {
                            return Err(AsError::new(format!(
                                "undefined superclass '{}'",
                                sup_name
                            ))
                            .into())
                        }
                    },
                    None => None,
                };
                let mut field_map = indexmap::IndexMap::new();
                for fd in fields {
                    field_map.insert(
                        fd.name.clone(),
                        crate::value::FieldSchema {
                            ty: fd.ty.clone(),
                            default: fd.default.clone(),
                        },
                    );
                }
                let mut method_map = indexmap::IndexMap::new();
                for m in methods {
                    method_map.insert(
                        m.name.clone(),
                        std::rc::Rc::new(crate::value::Method {
                            params: m.params.clone(),
                            ret: m.ret.clone(),
                            body: m.body.clone(),
                            is_async: m.is_async,
                        }),
                    );
                }
                let class = Value::Class(std::rc::Rc::new(crate::value::Class {
                    name: name.clone(),
                    superclass: parent,
                    fields: field_map,
                    methods: method_map,
                    def_env: env.clone(),
                }));
                env.define(name, class, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::Export(inner) => {
                let flow = self.exec_stmt(inner, env).await?;
                for name in exported_names(inner) {
                    self.current_exports.borrow().borrow_mut().insert(name);
                }
                Ok(flow)
            }
            Stmt::Import { names, source } => {
                let entry = if source.starts_with("std/") {
                    self.load_std_module(source)?
                } else {
                    let resolved = self.resolve_import(source);
                    self.load_module(&resolved).await?
                };
                match names {
                    crate::ast::ImportNames::Named(names) => {
                        for name in names {
                            if !entry.exports.borrow().contains(name) {
                                return Err(AsError::new(format!(
                                    "module '{}' has no export '{}'",
                                    source, name
                                ))
                                .into());
                            }
                            let v = entry.env.get(name).unwrap_or(Value::Nil);
                            env.define(name, v, false).map_err(AsError::new)?;
                        }
                    }
                    crate::ast::ImportNames::Namespace(alias) => {
                        let mut map = indexmap::IndexMap::new();
                        for name in entry.exports.borrow().iter() {
                            map.insert(name.clone(), entry.env.get(name).unwrap_or(Value::Nil));
                        }
                        env.define(alias, Value::Object(Rc::new(RefCell::new(map))), false)
                            .map_err(AsError::new)?;
                    }
                }
                Ok(Flow::Normal)
            }
        }
    }

    #[async_recursion(?Send)]
    pub async fn eval_expr(&self, expr: &Expr, env: &Environment) -> Result<Value, Control> {
        match &expr.kind {
            ExprKind::Number(n) => Ok(Value::Number(*n)),
            ExprKind::Str(s) => Ok(Value::Str(s.as_str().into())),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Nil => Ok(Value::Nil),
            ExprKind::Ident(name) => env.get(name).ok_or_else(|| {
                AsError::at(format!("undefined variable '{}'", name), expr.span).into()
            }),
            ExprKind::Assign { target, value } => {
                let v = self.eval_expr(value, env).await?;
                self.assign_to(target, v, value.span, env).await
            }
            ExprKind::Unary { op, expr: operand } => {
                let v = self.eval_expr(operand, env).await?;
                apply_unop(*op, v, operand.span)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinOp::And => {
                        let l = self.eval_expr(lhs, env).await?;
                        return if l.is_truthy() {
                            self.eval_expr(rhs, env).await
                        } else {
                            Ok(l)
                        };
                    }
                    BinOp::Or => {
                        let l = self.eval_expr(lhs, env).await?;
                        return if l.is_truthy() {
                            Ok(l)
                        } else {
                            self.eval_expr(rhs, env).await
                        };
                    }
                    BinOp::Coalesce => {
                        let l = self.eval_expr(lhs, env).await?;
                        return if l == Value::Nil {
                            self.eval_expr(rhs, env).await
                        } else {
                            Ok(l)
                        };
                    }
                    _ => {}
                }

                let l = self.eval_expr(lhs, env).await?;
                let r = self.eval_expr(rhs, env).await?;

                // All non-short-circuit operators (string concat / decimal / range
                // / cross-type equality / numeric) share ONE dispatch with the VM.
                apply_binop(*op, l, r, expr.span)
            }
            ExprKind::Arrow {
                params,
                body,
                is_async,
                is_generator,
            } => {
                let body_stmts = match body.as_ref() {
                    crate::ast::ArrowBody::Block(stmts) => stmts.clone(),
                    crate::ast::ArrowBody::Expr(e) => vec![Stmt::Return(Some((**e).clone()))],
                };
                Ok(Value::Function(std::rc::Rc::new(crate::value::Function {
                    name: None,
                    params: params.clone(),
                    ret: None,
                    body: body_stmts,
                    closure: env.clone(),
                    is_async: *is_async,
                    is_generator: *is_generator,
                })))
            }
            ExprKind::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        crate::ast::ArrayElem::Item(x) => {
                            values.push(self.eval_expr(x, env).await?)
                        }
                        crate::ast::ArrayElem::Spread(x) => {
                            let v = self.eval_expr(x, env).await?;
                            match v {
                                Value::Array(a) => values.extend(a.borrow().iter().cloned()),
                                other => {
                                    return Err(AsError::at(
                                        format!(
                                            "can only spread an array into an array, got {}",
                                            type_name(&other)
                                        ),
                                        x.span,
                                    )
                                    .into())
                                }
                            }
                        }
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(values))))
            }
            ExprKind::Object(entries) => {
                let mut map = indexmap::IndexMap::with_capacity(entries.len());
                for entry in entries {
                    match entry {
                        crate::ast::ObjEntry::KV(k, v) => {
                            let value = self.eval_expr(v, env).await?;
                            map.insert(k.clone(), value);
                        }
                        crate::ast::ObjEntry::Spread(x) => {
                            let v = self.eval_expr(x, env).await?;
                            match v {
                                Value::Object(o) => {
                                    for (k, val) in o.borrow().iter() {
                                        map.insert(k.clone(), val.clone());
                                    }
                                }
                                other => {
                                    return Err(AsError::at(
                                        format!(
                                            "can only spread an object into an object, got {}",
                                            type_name(&other)
                                        ),
                                        x.span,
                                    )
                                    .into())
                                }
                            }
                        }
                    }
                }
                Ok(Value::Object(std::rc::Rc::new(std::cell::RefCell::new(
                    map,
                ))))
            }
            ExprKind::Template { parts } => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        crate::ast::TemplatePart::Lit(s) => out.push_str(s),
                        crate::ast::TemplatePart::Expr(e) => {
                            let v = self.eval_expr(e, env).await?;
                            out.push_str(&v.to_string());
                        }
                    }
                }
                Ok(Value::Str(out.into()))
            }
            ExprKind::Match { subject, arms } => {
                let subj = self.eval_expr(subject, env).await?;
                for arm in arms {
                    for pat in &arm.patterns {
                        let mut bindings: Vec<(Rc<str>, Value)> = Vec::new();
                        if self.match_pattern(pat, &subj, &mut bindings, env).await? {
                            // Bindings (and the guard) live in a fresh child scope.
                            let arm_env = env.child();
                            for (name, val) in bindings {
                                // A pattern may bind the same name once; ignore a
                                // redefine error defensively.
                                let _ = arm_env.define(&name, val, false);
                            }
                            if let Some(guard) = &arm.guard {
                                let g = self.eval_expr(guard, &arm_env).await?;
                                if !g.is_truthy() {
                                    continue;
                                }
                            }
                            return self.eval_expr(&arm.body, &arm_env).await;
                        }
                    }
                }
                Err(AsError::at("no matching arm in match expression", expr.span).into())
            }
            ExprKind::Await(inner) => {
                let v = self.eval_expr(inner, env).await?;
                match v {
                    // Drive the future to completion; a panic/propagation raised in
                    // the spawned task re-surfaces here (cross-task propagation).
                    Value::Future(f) => f.get().await,
                    // `await` on a non-future is identity (back-compat: `await 5` == 5).
                    other => Ok(other),
                }
            }
            ExprKind::Yield(operand) => {
                let v = match operand {
                    Some(e) => self.eval_expr(e, env).await?,
                    None => Value::Nil,
                };
                // The generator currently being polled (top of the current-gen
                // stack). Absent => `yield` was used outside any generator body.
                let g = crate::coro::current_generator()
                    .ok_or_else(|| AsError::at("'yield' outside of a generator", expr.span))?;
                // Hand the value to the consumer and suspend; the resume value the
                // consumer passes to `gen.next(v)` becomes this expression's value.
                Ok(g.yield_(v).await)
            }
            ExprKind::Ternary { cond, then, els } => {
                // Only the selected branch is evaluated (lazy, like `&&`/`||`).
                let c = self.eval_expr(cond, env).await?;
                if c.is_truthy() {
                    self.eval_expr(then, env).await
                } else {
                    self.eval_expr(els, env).await
                }
            }
            ExprKind::Paren(inner) => self.eval_expr(inner, env).await,
            ExprKind::Try(inner) => {
                let v = self.eval_expr(inner, env).await?;
                // Must be a 2-element Result pair [value, err].
                let arr = match &v {
                    Value::Array(a) if a.borrow().len() == 2 => a.clone(),
                    _ => {
                        return Err(AsError::at(
                            "the ? operator requires a Result pair [value, err]",
                            expr.span,
                        )
                        .into())
                    }
                };
                let (value, err) = {
                    let b = arr.borrow();
                    (b[0].clone(), b[1].clone())
                };
                if err == Value::Nil {
                    Ok(value)
                } else {
                    // Early-return [nil, err] from the enclosing function.
                    Err(Control::Propagate(make_pair(Value::Nil, err)))
                }
            }
            ExprKind::Unwrap(inner) => {
                let v = self.eval_expr(inner, env).await?;
                let arr = match &v {
                    Value::Array(a) if a.borrow().len() == 2 => a.clone(),
                    _ => {
                        return Err(AsError::at(
                            "the ! operator requires a Result pair [value, err]",
                            expr.span,
                        )
                        .into())
                    }
                };
                let (value, err) = {
                    let b = arr.borrow();
                    (b[0].clone(), b[1].clone())
                };
                if err == Value::Nil {
                    Ok(value)
                } else {
                    // Promote the Tier-1 error to a Tier-2 panic, preserving the
                    // original error's message so `recover` round-trips it.
                    Err(AsError::at(error_message(&err), expr.span).into())
                }
            }
            ExprKind::OptMember { .. }
            | ExprKind::Member { .. }
            | ExprKind::Index { .. }
            | ExprKind::Call { .. } => {
                let (v, _) = self.eval_chain(expr, env).await?;
                Ok(v)
            }
        }
    }

    /// Try to match `pat` against `subject` (Phase 8a). On success returns `true`
    /// and pushes any captured names onto `bindings`; on a structural mismatch
    /// returns `false` (bindings may be partially filled and must be discarded).
    /// `env` is the enclosing scope, used for Option-C identifier resolution.
    #[async_recursion(?Send)]
    async fn match_pattern(
        &self,
        pat: &crate::ast::Pattern,
        subject: &Value,
        bindings: &mut Vec<(Rc<str>, Value)>,
        env: &Environment,
    ) -> Result<bool, Control> {
        use crate::ast::Pattern;
        match pat {
            Pattern::Wildcard => Ok(true),
            Pattern::Ident(name) => {
                // Option C: defined name → value compare; undefined → bind.
                if let Some(v) = env.get(name) {
                    Ok(v == *subject)
                } else {
                    bindings.push((name.clone(), subject.clone()));
                    Ok(true)
                }
            }
            Pattern::Value(e) => {
                let v = self.eval_expr(e, env).await?;
                Ok(v == *subject)
            }
            Pattern::Range {
                start,
                end,
                inclusive,
            } => {
                let n = match subject {
                    Value::Number(n) => *n,
                    _ => return Ok(false),
                };
                let lo = match self.eval_expr(start, env).await? {
                    Value::Number(x) => x,
                    _ => return Ok(false),
                };
                let hi = match self.eval_expr(end, env).await? {
                    Value::Number(x) => x,
                    _ => return Ok(false),
                };
                Ok(n >= lo && if *inclusive { n <= hi } else { n < hi })
            }
            Pattern::Array(pats, rest) => {
                // Snapshot the subject array (do not hold a borrow across awaits).
                let items: Vec<Value> = match subject {
                    Value::Array(a) => a.borrow().iter().cloned().collect(),
                    _ => return Ok(false),
                };
                match rest {
                    None => {
                        if items.len() != pats.len() {
                            return Ok(false);
                        }
                    }
                    Some(_) => {
                        if items.len() < pats.len() {
                            return Ok(false);
                        }
                    }
                }
                for (p, item) in pats.iter().zip(items.iter()) {
                    if !self.match_pattern(p, item, bindings, env).await? {
                        return Ok(false);
                    }
                }
                if let Some(Some(rest_name)) = rest {
                    let remainder: Vec<Value> = items[pats.len()..].to_vec();
                    bindings.push((
                        rest_name.clone(),
                        Value::Array(Rc::new(RefCell::new(remainder))),
                    ));
                }
                Ok(true)
            }
            Pattern::Object(entries, rest) => {
                // Snapshot the subject's fields (Object or Instance).
                let fields: indexmap::IndexMap<String, Value> = match subject {
                    Value::Object(o) => o.borrow().clone(),
                    Value::Instance(i) => i.borrow().fields.clone(),
                    _ => return Ok(false),
                };
                for entry in entries {
                    let field = match fields.get(entry.key.as_ref()) {
                        Some(v) => v.clone(),
                        None => return Ok(false),
                    };
                    match &entry.pat {
                        // `{key}` shorthand ALWAYS binds (documented Option-C exception).
                        None => bindings.push((entry.key.clone(), field)),
                        Some(p) => {
                            if !self.match_pattern(p, &field, bindings, env).await? {
                                return Ok(false);
                            }
                        }
                    }
                }
                if let Some(Some(rest_name)) = rest {
                    let named: std::collections::HashSet<&str> =
                        entries.iter().map(|e| e.key.as_ref()).collect();
                    let mut remaining = indexmap::IndexMap::new();
                    for (k, v) in fields.iter() {
                        if !named.contains(k.as_str()) {
                            remaining.insert(k.clone(), v.clone());
                        }
                    }
                    bindings.push((
                        rest_name.clone(),
                        Value::Object(Rc::new(RefCell::new(remaining))),
                    ));
                }
                Ok(true)
            }
        }
    }

    /// Evaluate a member/index/call chain, returning (value, short_circuited).
    /// `short_circuited == true` means an earlier `?.` link hit nil and the rest
    /// of the chain must yield nil without being accessed/called.
    #[async_recursion(?Send)]
    async fn eval_chain(&self, expr: &Expr, env: &Environment) -> Result<(Value, bool), Control> {
        match &expr.kind {
            ExprKind::OptMember { object, name } => {
                let (obj, sc) = self.eval_chain(object, env).await?;
                if sc || obj == Value::Nil {
                    return Ok((Value::Nil, true));
                }
                Ok((self.read_member(&obj, name, object.span)?, false))
            }
            ExprKind::Member { object, name } => {
                let (obj, sc) = self.eval_chain(object, env).await?;
                if sc {
                    return Ok((Value::Nil, true));
                }
                Ok((self.read_member(&obj, name, object.span)?, false))
            }
            ExprKind::Index { object, index } => {
                let (obj, sc) = self.eval_chain(object, env).await?;
                if sc {
                    return Ok((Value::Nil, true));
                }
                let idx = self.eval_expr(index, env).await?;
                // Shared with the bytecode VM (`Op::GetIndex`) so the two engines
                // cannot drift on index-read semantics or panic messages.
                let v = index_get(&obj, &idx, object.span, expr.span)?;
                Ok((v, false))
            }
            ExprKind::Call { callee, args } => {
                // Fluent schema method-chaining hook: a Call whose callee is a
                // plain `Member { object, name }` (NOT `OptMember`) where the
                // evaluated `object` is a schema value and `name` is a schema
                // method → route to `call_schema(name, [recv, ...args])` (the
                // SAME ops as the free functions). Otherwise fall back to the
                // EXACT pre-existing behavior: `read_member(recv, name)` then
                // `call_value`. `object` and the args are each evaluated ONCE.
                //
                // This is call-position only: bare `s.minLength` (member
                // access, no call) still reads the stored constraint field via
                // `read_member` — never a bound method (see schema design doc,
                // "Known limitation").
                if let ExprKind::Member { object, name } = &callee.kind {
                    let (recv, sc) = self.eval_chain(object, env).await?;
                    if sc {
                        return Ok((Value::Nil, true));
                    }
                    if crate::stdlib::schema::is_schema_value(&recv)
                        && crate::stdlib::schema::is_schema_method(name)
                    {
                        // Schema path: there is no `read_member` here, so the
                        // args may be evaluated after the schema check — receiver
                        // first, then the call args, into `call_schema`.
                        let values = self.eval_call_args(args, env).await?;
                        let mut sargs = Vec::with_capacity(values.len() + 1);
                        sargs.push(recv);
                        sargs.extend(values);
                        return Ok((self.call_schema(name, &sargs, expr.span).await?, false));
                    }
                    // Fallback — byte-for-byte with the prior
                    // `eval_chain(callee) → eval_args → call_value` path: read
                    // the member FIRST (which can error — nil receiver, bad
                    // enum-variant prop, …), and only THEN evaluate the args, so
                    // a member-read error preempts arg evaluation / side effects.
                    let callee_v = self.read_member(&recv, name, object.span)?;
                    let values = self.eval_call_args(args, env).await?;
                    let v = self.call_value(callee_v, values, expr.span).await;
                    return Ok((v?, false));
                }

                let (callee_v, sc) = self.eval_chain(callee, env).await?;
                if sc {
                    return Ok((Value::Nil, true));
                }
                let values = self.eval_call_args(args, env).await?;
                let v = self.call_value(callee_v, values, expr.span).await;
                Ok((v?, false))
            }
            _ => Ok((self.eval_expr(expr, env).await?, false)),
        }
    }

    /// Evaluate a call-argument list, flattening `...spread` of an array into
    /// positional values. Each argument expression is evaluated exactly once,
    /// left to right (same semantics as the prior inline loop in the `Call`
    /// arm of `eval_chain`).
    #[async_recursion(?Send)]
    async fn eval_call_args(
        &self,
        args: &[crate::ast::CallArg],
        env: &Environment,
    ) -> Result<Vec<Value>, Control> {
        let mut values = Vec::new();
        for a in args {
            match a {
                crate::ast::CallArg::Pos(x) => values.push(self.eval_expr(x, env).await?),
                crate::ast::CallArg::Spread(x) => {
                    let v = self.eval_expr(x, env).await?;
                    match v {
                        Value::Array(arr) => values.extend(arr.borrow().iter().cloned()),
                        other => {
                            return Err(AsError::at(
                                format!(
                                    "can only spread an array as call arguments, got {}",
                                    type_name(&other)
                                ),
                                x.span,
                            )
                            .into())
                        }
                    }
                }
            }
        }
        Ok(values)
    }

    // pub(crate): shared with the bytecode VM (`Op::GetProp`/`Op::GetPropOpt`)
    // so member-access semantics (fields, methods→BoundMethod, enum variants,
    // native handles, nil-receiver errors) have ONE implementation.
    pub(crate) fn read_member(&self, obj: &Value, name: &str, span: Span) -> Result<Value, AsError> {
        match obj {
            Value::Object(map) => Ok(map.borrow().get(name).cloned().unwrap_or(Value::Nil)),
            Value::Enum(e) => e.variants.get(name).cloned().ok_or_else(|| {
                AsError::at(format!("enum {} has no variant '{}'", e.name, name), span)
            }),
            Value::EnumVariant(v) => match name {
                "name" => Ok(Value::Str(v.name.as_str().into())),
                "value" => Ok(v.value.clone()),
                other => Err(AsError::at(
                    format!("enum variant has no property '{}'", other),
                    span,
                )),
            },
            Value::Instance(inst) => {
                let b = inst.borrow();
                if let Some(v) = b.fields.get(name) {
                    return Ok(v.clone());
                }
                match crate::value::find_method(&b.class, name) {
                    Some((method, def_class)) => Ok(Value::BoundMethod(std::rc::Rc::new(
                        crate::value::BoundMethod {
                            receiver: obj.clone(),
                            method,
                            defining_class: def_class,
                            name: name.to_string(),
                        },
                    ))),
                    None => Ok(Value::Nil),
                }
            }
            Value::Super(s) => match &s.start {
                Some(start) => match crate::value::find_method(start, name) {
                    Some((method, def_class)) => Ok(Value::BoundMethod(std::rc::Rc::new(
                        crate::value::BoundMethod {
                            receiver: s.receiver.clone(),
                            method,
                            defining_class: def_class,
                            name: name.to_string(),
                        },
                    ))),
                    None => Err(AsError::at(
                        format!("no superclass method '{}'", name),
                        span,
                    )),
                },
                None => Err(AsError::at(
                    format!("no superclass method '{}' (no superclass)", name),
                    span,
                )),
            },
            Value::Native(n) => {
                // `sse.lastEventId` is a LIVE property: the most recent `id:` seen,
                // which `next()` keeps current on the resource (the handle's `fields`
                // are immutable after minting, so it can't be a static field). Read it
                // straight from the resource state.
                #[cfg(feature = "net")]
                if name == "lastEventId" && n.kind == crate::value::NativeKind::SseStream {
                    let id = self.with_resource(n.id, |r| match r {
                        Some(ResourceState::SseStream(s)) => s.last_event_id().to_string(),
                        _ => String::new(),
                    });
                    return Ok(Value::Str(id.into()));
                }
                if let Some(v) = n.fields.get(name) {
                    return Ok(v.clone());
                }
                // `resp.body` is only a reader when the request used `stream:true`
                // (then `body` is a field set above). On a buffered response it is
                // absent — a bare `resp.body` is a mistake, so surface a clear error
                // directing the caller to text()/bytes()/json() instead of silently
                // returning a `body` NativeMethod that would fail confusingly later.
                #[cfg(feature = "net")]
                if name == "body" && n.kind == crate::value::NativeKind::HttpResponse {
                    return Err(AsError::at(
                        "resp.body is only available on a streaming response (request opts.stream:true); use resp.text()/bytes()/json() for a buffered body",
                        span,
                    ));
                }
                Ok(Value::NativeMethod(std::rc::Rc::new(
                    crate::value::NativeMethod {
                        receiver: n.clone(),
                        method: name.to_string(),
                    },
                )))
            }
            Value::Generator(g) => match name {
                // `gen.next` / `gen.close` are bound generator methods.
                "next" => Ok(Value::GeneratorMethod(g.clone(), "next")),
                "close" => Ok(Value::GeneratorMethod(g.clone(), "close")),
                other => Err(AsError::at(
                    format!("generator has no property '{}' (try 'next')", other),
                    span,
                )),
            },
            Value::Class(c) => match name {
                "from" => Ok(Value::ClassMethod(c.clone(), "from")),
                other => Err(AsError::at(
                    format!("class {} has no static member '{}'", c.name, other),
                    span,
                )),
            },
            Value::Nil => Err(AsError::at(
                format!("cannot read property '{}' of nil", name),
                span,
            )),
            _ => Err(AsError::at(
                format!("cannot read property '{}' of this value", name),
                span,
            )),
        }
    }

    // pub(crate): used by std/* modules (std/array callbacks) later in M10.
    #[async_recursion(?Send)]
    pub(crate) async fn call_value(
        &self,
        callee: Value,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match callee {
            // A VM closure (`native → VM` bridge): a native higher-order stdlib
            // function (e.g. `array.map`, a sort comparator, `recover`) is calling
            // a user callback that the VM produced. Re-enter the VM to run it on a
            // fresh Fiber. Upgrade the registered `vm` weak to an owned `Rc<Vm>`
            // FIRST so no `RefCell` borrow is held across the await. A
            // `Value::Closure` can only exist if the VM created it, so the VM is
            // always registered here; a missing VM is a wiring bug (clear panic,
            // not UB).
            Value::Closure(_) => {
                let vm = self
                    .vm()
                    .expect("VM not registered for closure call (Interp::set_vm not called)");
                vm.call_value(callee, args, span).await
            }
            Value::Builtin(name) => self.call_builtin(&name, &args, span).await,
            Value::Function(func) => self.call_function(func, args, span).await,
            Value::Class(class) => self.construct(class, args, span).await,
            Value::BoundMethod(bm) => self.invoke_method(&bm, args, span).await,
            Value::NativeMethod(m) => self.call_native_method(m, args, span).await,
            Value::GeneratorMethod(g, method) => {
                self.call_generator_method(&g, method, args, span).await
            }
            Value::ClassMethod(c, "from") => {
                let obj = args.first().cloned().unwrap_or(Value::Nil);
                let strict = matches!(args.get(1), Some(Value::Bool(true)));
                self.validate_into(&c, &obj, strict, "", span)
                    .await
                    .map_err(Control::from)
            }
            Value::ClassMethod(c, other) => Err(AsError::at(
                format!("class {} has no static member '{}'", c.name, other),
                span,
            )
            .into()),
            _ => Err(AsError::at("value is not callable", span).into()),
        }
    }

    /// Dispatch a bound generator method. `next(v?)` resumes the body with `v`
    /// (`nil` if omitted) and returns the next yielded value, or `nil` when the
    /// generator is done; a body panic/propagation surfaces here as `Err`.
    /// `close()` drops the body future (subsequent `next` returns `nil`).
    #[async_recursion(?Send)]
    pub(crate) async fn call_generator_method(
        &self,
        g: &std::rc::Rc<crate::coro::GeneratorHandle>,
        method: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match method {
            "next" => {
                let input = args.into_iter().next().unwrap_or(Value::Nil);
                // resume drives the body to its next yield; Err surfaces a body
                // panic to the consumer, None is the done sentinel (→ nil).
                match g.resume(input).await? {
                    Some(v) => Ok(v),
                    None => Ok(Value::Nil),
                }
            }
            "close" => {
                // Drop the body future: no further values; `next` now returns nil.
                g.close();
                Ok(Value::Nil)
            }
            other => Err(AsError::at(format!("generator has no method '{}'", other), span).into()),
        }
    }

    /// Dispatch a `NativeMethod` (e.g. `conn.query`, `child.wait`) to the handler
    /// for its receiver's kind. Async + recursive because handlers (added by
    /// sqlite/process tasks) re-enter the interpreter via `call_value`. For now
    /// every kind falls through to the "no such method" error — the feature-gated
    /// arms are added by Tasks 6/7.
    #[async_recursion(?Send)]
    pub(crate) async fn call_native_method(
        &self,
        m: std::rc::Rc<crate::value::NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let _ = &args;
        #[cfg(feature = "sql")]
        {
            use crate::value::NativeKind::*;
            if matches!(m.receiver.kind, SqliteConnection | SqliteStatement) {
                return self.call_sqlite_method(&m, args, span).await;
            }
        }
        #[cfg(feature = "sys")]
        {
            use crate::value::NativeKind::*;
            if matches!(m.receiver.kind, ChildProcess | Reader | Writer) {
                return self.call_process_method(&m, args, span).await;
            }
        }
        #[cfg(feature = "net")]
        {
            use crate::value::NativeKind::*;
            if matches!(m.receiver.kind, TcpListener | TcpStream) {
                return self.call_tcp_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, HttpResponse) {
                return self.call_http_response_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, HttpBody) {
                return self.call_http_body_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, CancelHandle) {
                return self.call_cancel_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, SseStream) {
                return self.call_sse_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, HttpServer) {
                return self.call_http_server_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, HttpNext) {
                return self.call_http_next(&m, args, span).await;
            }
            if matches!(m.receiver.kind, WsConnection | WsListener) {
                return self.call_ws_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, UdpSocket) {
                return self.call_udp_method(&m, args, span).await;
            }
        }
        #[cfg(feature = "tui")]
        {
            if matches!(m.receiver.kind, crate::value::NativeKind::Terminal) {
                return self.call_terminal_method(&m, args, span).await;
            }
        }
        {
            use crate::value::NativeKind::*;
            if matches!(m.receiver.kind, Interval) {
                return self.call_interval_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, DebounceWrapper) {
                return self.call_debounce_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, ThrottleWrapper) {
                return self.call_throttle_method(&m, args, span).await;
            }
            if matches!(m.receiver.kind, RateLimiter) {
                return self.call_rate_limiter_method(&m, args, span).await;
            }
        }
        Err(AsError::at(format!("native handle has no method '{}'", m.method), span).into())
    }

    /// Drive a `for await (x in e)` loop. `e` must be async-iterable: a script
    /// generator (driven via its channel) or a native stream handle whose recv/
    /// next method yields a `[value, err]` pair ending in a `nil` value (WebSocket
    /// `recv`, SSE `next`). Each item binds `var` in a fresh child scope; the body
    /// honours `break`/`continue`/`return`. A generator body error re-surfaces here.
    #[async_recursion(?Send)]
    async fn exec_for_await(
        &self,
        var: &str,
        iterable: Value,
        body: &[Stmt],
        env: &Environment,
        span: Span,
    ) -> Result<Flow, Control> {
        match iterable {
            Value::Generator(g) => {
                loop {
                    // resume drives the body to its next yield; Err surfaces a body
                    // panic, None ends iteration.
                    let item = match g.resume(Value::Nil).await? {
                        Some(v) => v,
                        None => break,
                    };
                    let child = env.child();
                    child.define(var, item, false).map_err(AsError::new)?;
                    match self.exec(body, &child).await? {
                        // `break` / early return abandon the generator: `close` it
                        // (drops the body future). There is no task to reclaim — a
                        // consumer-driven generator just stops being polled — but
                        // closing frees the body promptly rather than at scope end.
                        Flow::Break => {
                            g.close();
                            break;
                        }
                        Flow::Return(v) => {
                            g.close();
                            return Ok(Flow::Return(v));
                        }
                        Flow::Continue | Flow::Normal => {}
                    }
                }
                Ok(Flow::Normal)
            }
            // A native stream handle: iterate its recv/next method until the value
            // is nil (end-of-stream). Both WS (`recv`) and SSE (`next`) follow the
            // `[value, err]` contract where a nil value marks end-of-stream.
            Value::Native(ref n) => {
                let method = native_stream_method(n.kind).ok_or_else(|| {
                    AsError::at(
                        format!(
                            "value of type {} is not async-iterable",
                            type_name(&iterable)
                        ),
                        span,
                    )
                })?;
                loop {
                    let bound = Value::NativeMethod(std::rc::Rc::new(crate::value::NativeMethod {
                        receiver: n.clone(),
                        method: method.to_string(),
                    }));
                    let pair = self.call_value(bound, Vec::new(), span).await?;
                    // The recv/next contract returns a `[value, err]` pair.
                    let (value, err) = match &pair {
                        Value::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        // Defensive: a non-pair return ends iteration.
                        _ => break,
                    };
                    if err != Value::Nil {
                        // Surface a stream error as a Tier-2 panic at the loop site.
                        let msg = error_message(&err);
                        return Err(
                            AsError::at(format!("for await stream error: {}", msg), span).into(),
                        );
                    }
                    if value == Value::Nil {
                        break; // end-of-stream
                    }
                    let child = env.child();
                    child.define(var, value, false).map_err(AsError::new)?;
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                }
                Ok(Flow::Normal)
            }
            other => Err(AsError::at(
                format!("value of type {} is not async-iterable", type_name(&other)),
                span,
            )
            .into()),
        }
    }

    /// Bind params (with contracts), run a body in `call_env`, apply the return
    /// contract. Shared by plain functions and methods.
    #[async_recursion(?Send)]
    async fn run_body<'s: 'async_recursion>(
        &self,
        spec: BodySpec<'s>,
        args: Vec<Value>,
        call_env: &Environment,
        span: Span,
        what: &str,
    ) -> Result<Value, Control> {
        let BodySpec { params, ret, body } = spec;
        // Arity + parameter contracts + rest collection. Shared verbatim with the
        // bytecode VM (`src/vm/run.rs` CALL) so both engines bind args identically.
        let bound = check_call_args(params, args, span, what)?;
        for (p, a) in params.iter().zip(bound.into_iter()) {
            call_env.define(&p.name, a, true).map_err(AsError::new)?;
        }
        let result = match self.exec(body, call_env).await {
            Ok(Flow::Return(v)) => v,
            Ok(Flow::Normal) => Value::Nil,
            Ok(Flow::Break) => return Err(AsError::at("'break' outside of a loop", span).into()),
            Ok(Flow::Continue) => {
                return Err(AsError::at("'continue' outside of a loop", span).into())
            }
            Err(Control::Propagate(v)) => v,
            Err(Control::Panic(e)) => return Err(Control::Panic(e)),
            // exit() unwinds through function calls unchanged — re-propagate.
            Err(Control::Exit(code)) => return Err(Control::Exit(code)),
        };
        if let Some(ty) = ret {
            if !check_type(&result, ty) {
                return Err(contract_panic(ty, &result, span));
            }
        }
        Ok(result)
    }

    #[async_recursion(?Send)]
    async fn call_function(
        &self,
        func: Rc<crate::value::Function>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let call_env = func.closure.child();
        let what = func.name.as_deref().unwrap_or("function").to_string();
        // A generator (`fn*` / `async fn*`) is NOT run inline and is NOT spawned as
        // a task. Its body is built into a boxed future stored on a
        // `GeneratorHandle` and driven *synchronously by the consumer* via
        // `gen.next(v)` / `for await` (see `src/coro.rs`). Making it consumer-driven
        // (rather than a `spawn_local` task) is what prevents an abandoned
        // generator from parking a zombie task that would hang the exit drain: an
        // un-driven generator is just an unpolled future that drops cleanly.
        //
        // The body uses `run_function_body`, which already owns all its captures, so
        // the future is `'static`. The body's `yield` finds this generator via the
        // current-generator stack that `resume` maintains while polling. Both sync
        // and async generators take this path (the body may itself `await`).
        if func.is_generator {
            let vm = self.rc();
            let func = func.clone();
            let body: std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Control>>>> =
                Box::pin(
                    async move { vm.run_function_body(func, args, call_env, span, what).await },
                );
            return Ok(Value::Generator(Rc::new(
                crate::coro::GeneratorHandle::new(body),
            )));
        }
        // A script `async fn` is scheduled eagerly: build the body future, spawn it
        // onto the current-thread LocalSet, and hand back a `Value::Future`
        // immediately. `await` later drives it; the top-level drain ensures even an
        // unawaited call runs to completion. Non-async functions run inline.
        if func.is_async {
            let vm = self.rc();
            let func = func.clone();
            let fut = crate::task::SharedFuture::new();
            // The task resolves the *cell* (not a `SharedFuture` clone) so it never
            // keeps the handle alive — letting the handle's `Drop` cancel the task
            // once the last `Value::Future` is dropped (structured concurrency).
            let cell = fut.cell();
            // Track this task's lifetime for backpressure; the guard moves into the
            // task and decrements on completion OR cancel-on-drop.
            let guard = self.inflight_guard();
            let handle = tokio::task::spawn_local(async move {
                let _g = guard;
                // The owned `func`/`call_env`/`what` live in `run_function_body`'s
                // frame, so the `BodySpec` borrow never escapes this `'static` task.
                let r = vm.run_function_body(func, args, call_env, span, what).await;
                cell.resolve(r);
            });
            // Cancel-on-drop: dropping the last handle aborts this task.
            fut.set_abort(handle.abort_handle());
            // Cooperatively yield if many tasks are in flight, so cancelled/finished
            // ones get reaped (bounds memory in a tight un-awaited loop).
            self.maybe_yield_for_inflight().await;
            return Ok(Value::Future(fut));
        }
        self.run_function_body(func, args, call_env, span, what)
            .await
    }

    /// Run a (already-prepared) function body, owning the `Rc<Function>` for the
    /// whole frame so the `BodySpec` borrow stays local. Used both inline (sync
    /// functions) and from a spawned `'static` task (async functions).
    #[async_recursion(?Send)]
    async fn run_function_body(
        &self,
        func: Rc<crate::value::Function>,
        args: Vec<Value>,
        call_env: Environment,
        span: Span,
        what: String,
    ) -> Result<Value, Control> {
        let spec = BodySpec {
            params: &func.params,
            ret: &func.ret,
            body: &func.body,
        };
        self.run_body(spec, args, &call_env, span, &what).await
    }

    #[async_recursion(?Send)]
    async fn construct(
        &self,
        class: std::rc::Rc<crate::value::Class>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let instance = std::rc::Rc::new(std::cell::RefCell::new(crate::value::Instance {
            class: class.clone(),
            fields: indexmap::IndexMap::new(),
        }));
        let inst_val = Value::Instance(instance.clone());
        // Pre-populate declared-field defaults (merged base-class first so a
        // subclass default overrides). `init` may then override; `.from` (Task 4)
        // handles its own defaults. Each default evals lazily in the def env of
        // the class that declared it.
        for (fname, (schema, def_class)) in crate::value::merged_field_schema(&class) {
            if let Some(def) = &schema.default {
                // Eval into a local first (never hold the instance borrow across
                // `.await`).
                let dv = self.eval_expr(def, &def_class.def_env).await?;
                if !check_type(&dv, &schema.ty) {
                    return Err(contract_panic(&schema.ty, &dv, span));
                }
                instance.borrow_mut().fields.insert(fname.clone(), dv);
            }
        }
        match crate::value::find_method(&class, "init") {
            Some((method, def_class)) => {
                let bm = crate::value::BoundMethod {
                    receiver: inst_val.clone(),
                    method,
                    defining_class: def_class,
                    name: "init".to_string(),
                };
                self.invoke_method(&bm, args, span).await?;
            }
            None => {
                if !args.is_empty() {
                    return Err(AsError::at(
                        format!(
                            "{} has no init but was given {} argument(s)",
                            class.name,
                            args.len()
                        ),
                        span,
                    )
                    .into());
                }
            }
        }
        Ok(inst_val)
    }

    /// Validate a raw object against a class's declared fields, producing a
    /// checked instance. Recurses into nested class / array<Class> / map<K,Class>
    /// fields. Does NOT run `init`. Non-panicking: returns Err on mismatch.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn validate_into(
        &self,
        class: &std::rc::Rc<crate::value::Class>,
        obj: &Value,
        strict: bool,
        path: &str,
        span: Span,
    ) -> Result<Value, AsError> {
        let map = match obj {
            Value::Object(m) => m.clone(),
            _ => {
                return Err(AsError::at(
                    format!(
                        "{} expects an object, got {}",
                        field_owner_label(path, &class.name),
                        type_name(obj)
                    ),
                    span,
                ))
            }
        };
        // Declared fields merged base-class first (subclass overrides on clash).
        let schema = crate::value::merged_field_schema(class);

        let mut inst_fields = indexmap::IndexMap::new();
        for (fname, (fs, def_class)) in &schema {
            let field_path = if path.is_empty() {
                format!("{}.{}", class.name.to_lowercase(), fname)
            } else {
                format!("{}.{}", path, fname)
            };
            let raw = map.borrow().get(fname).cloned();
            let mut val = raw.unwrap_or(Value::Nil);
            if val == Value::Nil {
                if let Some(def) = &fs.default {
                    // Resolve the default in the DECLARING class's def env (the
                    // scope where the field was written), consistent with
                    // `construct`. Using the leaf class's env would diverge for an
                    // inherited field whose default references a module-scoped name
                    // visible only in the base class's module.
                    val = self
                        .eval_expr(def, &def_class.def_env)
                        .await
                        .map_err(|c| control_to_aserror(c, span))?;
                }
            }
            // Same scoping principle for the field's declared type: a nested class
            // name resolves in the env of the class that declared the field.
            val = self
                .coerce_field(&fs.ty, val, &def_class.def_env, strict, &field_path, span)
                .await?;
            if !check_type(&val, &fs.ty) {
                return Err(AsError::at(
                    format!(
                        "type contract violated at {}: expected {}, got {}",
                        field_path,
                        fs.ty,
                        type_name(&val)
                    ),
                    span,
                ));
            }
            inst_fields.insert(fname.clone(), val);
        }

        if strict {
            for k in map.borrow().keys() {
                if !schema.contains_key(k) {
                    return Err(AsError::at(
                        format!(
                            "unexpected key '{}' for {} (strict)",
                            k,
                            field_owner_label(path, &class.name)
                        ),
                        span,
                    ));
                }
            }
        }

        Ok(Value::Instance(std::rc::Rc::new(std::cell::RefCell::new(
            crate::value::Instance {
                class: class.clone(),
                fields: inst_fields,
            },
        ))))
    }

    /// Recursively coerce a raw value to match a declared field type: a raw
    /// Object whose field type is a class becomes that class's validated
    /// instance; arrays/maps of a class recurse element/value-wise; Optional
    /// passes non-nil through to the inner type. Everything else is unchanged.
    #[async_recursion::async_recursion(?Send)]
    async fn coerce_field(
        &self,
        ty: &crate::ast::Type,
        val: Value,
        env: &Environment,
        strict: bool,
        path: &str,
        span: Span,
    ) -> Result<Value, AsError> {
        use crate::ast::Type;
        match ty {
            Type::Optional(inner) => {
                if val == Value::Nil {
                    Ok(Value::Nil)
                } else {
                    self.coerce_field(inner, val, env, strict, path, span).await
                }
            }
            Type::Named(name) => match (&val, env.get(name)) {
                (Value::Object(_), Some(Value::Class(c))) => {
                    self.validate_into(&c, &val, strict, path, span).await
                }
                _ => Ok(val),
            },
            Type::Array(elem) => match &val {
                Value::Array(a) => {
                    let items: Vec<Value> = a.borrow().clone();
                    let mut out = Vec::with_capacity(items.len());
                    for (i, it) in items.into_iter().enumerate() {
                        let p = format!("{}[{}]", path, i);
                        out.push(self.coerce_field(elem, it, env, strict, &p, span).await?);
                    }
                    Ok(Value::Array(std::rc::Rc::new(std::cell::RefCell::new(out))))
                }
                _ => Ok(val),
            },
            Type::Map(_, vty) => match &val {
                Value::Map(m) => {
                    let entries: Vec<(crate::value::MapKey, Value)> = m
                        .borrow()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    let out = std::rc::Rc::new(std::cell::RefCell::new(indexmap::IndexMap::new()));
                    for (k, v) in entries {
                        let p = format!("{}[{}]", path, k.to_value());
                        let cv = self.coerce_field(vty, v, env, strict, &p, span).await?;
                        out.borrow_mut().insert(k, cv);
                    }
                    Ok(Value::Map(out))
                }
                // A raw Object (e.g. a JSON dictionary) coerces into a Map at the
                // `.from` boundary: each string key becomes a `MapKey::Str` and
                // each value is recursively coerced through the declared value
                // type. Insertion order is preserved. This closes the gap where a
                // parsed-JSON `map<K, Class>` field would otherwise be an Object
                // and fail the `map<K,V>` contract.
                Value::Object(o) => {
                    let entries: Vec<(String, Value)> = o
                        .borrow()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    let out = std::rc::Rc::new(std::cell::RefCell::new(indexmap::IndexMap::new()));
                    for (k, v) in entries {
                        let p = format!("{}[{}]", path, k);
                        let cv = self.coerce_field(vty, v, env, strict, &p, span).await?;
                        out.borrow_mut()
                            .insert(crate::value::MapKey::Str(k.as_str().into()), cv);
                    }
                    Ok(Value::Map(out))
                }
                _ => Ok(val),
            },
            _ => Ok(val),
        }
    }

    #[async_recursion(?Send)]
    async fn invoke_method(
        &self,
        bm: &crate::value::BoundMethod,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let call_env = bm.defining_class.def_env.child();
        call_env
            .define("self", bm.receiver.clone(), false)
            .map_err(AsError::new)?;
        // `super` lookup begins at the defining class's superclass.
        let super_ref = Value::Super(std::rc::Rc::new(crate::value::SuperRef {
            receiver: bm.receiver.clone(),
            start: bm.defining_class.superclass.clone(),
        }));
        call_env
            .define("super", super_ref, false)
            .map_err(AsError::new)?;
        // An async method, like an async free function, is scheduled eagerly and
        // returns a `Value::Future`. We move owned copies (the `Rc<Method>`, name,
        // and prepared `call_env`) into the spawned task so the body can outlive
        // this `&self` call.
        if bm.method.is_async {
            let vm = self.rc();
            let method = bm.method.clone();
            let name = bm.name.clone();
            let fut = crate::task::SharedFuture::new();
            // Resolve the cell, not a handle clone, so cancel-on-drop works.
            let cell = fut.cell();
            let guard = self.inflight_guard();
            let handle = tokio::task::spawn_local(async move {
                let _g = guard;
                // Owned `method`/`call_env`/`name` keep the `BodySpec` borrow inside
                // `run_method_body`'s frame, so nothing escapes the `'static` task.
                let r = vm.run_method_body(method, args, call_env, span, name).await;
                cell.resolve(r);
            });
            fut.set_abort(handle.abort_handle());
            self.maybe_yield_for_inflight().await;
            return Ok(Value::Future(fut));
        }
        let spec = BodySpec {
            params: &bm.method.params,
            ret: &bm.method.ret,
            body: &bm.method.body,
        };
        self.run_body(spec, args, &call_env, span, &bm.name).await
    }

    /// Run a method body owning the `Rc<Method>` for the whole frame (so the
    /// `BodySpec` borrow stays local). Used by the async-method spawn path.
    #[async_recursion(?Send)]
    async fn run_method_body(
        &self,
        method: Rc<crate::value::Method>,
        args: Vec<Value>,
        call_env: Environment,
        span: Span,
        what: String,
    ) -> Result<Value, Control> {
        let spec = BodySpec {
            params: &method.params,
            ret: &method.ret,
            body: &method.body,
        };
        self.run_body(spec, args, &call_env, span, &what).await
    }

    #[async_recursion(?Send)]
    async fn call_builtin(&self, name: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        match name {
            "print" => {
                let mut line = args
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                line.push('\n');
                self.push_output(&line);
                Ok(Value::Nil)
            }
            "Ok" => {
                let value = args.first().cloned().unwrap_or(Value::Nil);
                Ok(make_pair(value, Value::Nil))
            }
            "Err" => {
                let msg = args.first().cloned().unwrap_or(Value::Nil);
                Ok(make_pair(Value::Nil, make_error(msg)))
            }
            "assert" => {
                let cond = args.first().cloned().unwrap_or(Value::Nil);
                if cond.is_truthy() {
                    Ok(Value::Nil)
                } else {
                    let msg = match args.get(1) {
                        Some(Value::Str(s)) => s.to_string(),
                        Some(v) => v.to_string(),
                        None => "assertion failed".to_string(),
                    };
                    Err(AsError::at(msg, span).into())
                }
            }
            "recover" => {
                let callee = args.first().cloned().unwrap_or(Value::Nil);
                match self.call_value(callee, Vec::new(), span).await {
                    Ok(v) => Ok(make_pair(v, Value::Nil)),
                    Err(Control::Panic(e)) => Ok(make_pair(
                        Value::Nil,
                        make_error(Value::Str(e.message.into())),
                    )),
                    // A `?` propagation inside `fn` is already converted to fn's return
                    // value by call_function, so this is unreachable in practice; pass it through.
                    Err(Control::Propagate(v)) => Err(Control::Propagate(v)),
                    // exit() is NOT catchable by recover — pass it through unchanged.
                    Err(Control::Exit(code)) => Err(Control::Exit(code)),
                }
            }
            "exit" => {
                // exit(code?) — default 0; code must be an integer in 0..=255.
                let code: i32 = match args.first() {
                    None => 0,
                    Some(Value::Number(n)) => {
                        let n = *n;
                        if n.fract() != 0.0 || !(0.0..=255.0).contains(&n) {
                            return Err(AsError::at(
                                format!("exit code must be an integer in 0..=255, got {}", n),
                                span,
                            )
                            .into());
                        }
                        n as i32
                    }
                    Some(v) => {
                        return Err(AsError::at(
                            format!(
                                "exit code must be an integer in 0..=255, got {}",
                                type_name(v)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                Err(Control::Exit(code))
            }
            "test" => {
                let name = match args.first() {
                    Some(Value::Str(s)) => s.to_string(),
                    Some(v) => v.to_string(),
                    None => "<unnamed>".to_string(),
                };
                let func = args.get(1).cloned().unwrap_or(Value::Nil);
                // Register only; `ascript test` runs these via run_registered_tests.
                self.tests.borrow_mut().push((name, func));
                Ok(Value::Nil)
            }
            "len" => {
                let v = args.first().cloned().unwrap_or(Value::Nil);
                let n = match &v {
                    Value::Str(s) => s.chars().count(),
                    Value::Array(a) => a.borrow().len(),
                    Value::Object(o) => o.borrow().len(),
                    Value::Map(m) => m.borrow().len(),
                    Value::Set(s) => s.borrow().len(),
                    Value::Bytes(b) => b.borrow().len(),
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "len() expects a string, array, object, map, set, or bytes, got {}",
                                type_name(&v)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                Ok(Value::Number(n as f64))
            }
            "type" => {
                let v = args.first().cloned().unwrap_or(Value::Nil);
                Ok(Value::Str(type_name(&v).into()))
            }
            "range" => {
                let want_num = |i: usize| -> Result<f64, Control> {
                    match args.get(i) {
                        Some(Value::Number(n)) => Ok(*n),
                        Some(v) => Err(AsError::at(
                            format!("range() expects number arguments, got {}", type_name(v)),
                            span,
                        )
                        .into()),
                        None => Ok(0.0),
                    }
                };
                let (start, end, step) = match args.len() {
                    1 => (0.0, want_num(0)?, 1.0),
                    2 => (want_num(0)?, want_num(1)?, 1.0),
                    3 => (want_num(0)?, want_num(1)?, want_num(2)?),
                    n => {
                        return Err(AsError::at(
                            format!("range() expects 1 to 3 arguments, got {}", n),
                            span,
                        )
                        .into())
                    }
                };
                if step == 0.0 {
                    return Err(AsError::at("range() step must not be zero", span).into());
                }
                let mut out = Vec::new();
                let mut i = start;
                if step > 0.0 {
                    while i < end {
                        out.push(Value::Number(i));
                        i += step;
                    }
                } else {
                    while i > end {
                        out.push(Value::Number(i));
                        i += step;
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(out))))
            }
            other => {
                if let Some((module, func)) = other.split_once('.') {
                    self.call_stdlib(module, func, args, span).await
                } else {
                    Err(AsError::at(format!("'{}' is not a function", other), span).into())
                }
            }
        }
    }

    #[async_recursion(?Send)]
    async fn assign_to(
        &self,
        target: &Expr,
        value: Value,
        value_span: Span,
        env: &Environment,
    ) -> Result<Value, Control> {
        match &target.kind {
            ExprKind::Ident(name) => match env.assign(name, value.clone()) {
                Ok(()) => Ok(value),
                Err(AssignError::Undefined) => Err(AsError::at(
                    format!("cannot assign to undefined variable '{}'", name),
                    target.span,
                )
                .into()),
                Err(AssignError::Immutable) => Err(AsError::at(
                    format!("cannot assign to immutable binding '{}'", name),
                    target.span,
                )
                .into()),
            },
            ExprKind::Index { object, index } => {
                let obj = self.eval_expr(object, env).await?;
                let idx = self.eval_expr(index, env).await?;
                match obj {
                    Value::Array(arr) => {
                        let i = array_index(&idx, target.span)?;
                        let mut arr = arr.borrow_mut();
                        if i >= arr.len() {
                            return Err(AsError::at(
                                format!("index {} out of bounds (len {})", i, arr.len()),
                                target.span,
                            )
                            .into());
                        }
                        arr[i] = value.clone();
                        Ok(value)
                    }
                    Value::Object(map) => match idx {
                        Value::Str(key) => {
                            map.borrow_mut().insert(key.to_string(), value.clone());
                            Ok(value)
                        }
                        _ => Err(AsError::at("object index must be a string", target.span).into()),
                    },
                    _ => Err(
                        AsError::at("cannot index-assign a non-array value", object.span).into(),
                    ),
                }
            }
            ExprKind::Member { object, name } => {
                let obj = self.eval_expr(object, env).await?;
                self.set_member(&obj, name, value, object.span, value_span)
            }
            _ => Err(AsError::at("invalid assignment target", target.span).into()),
        }
    }

    /// Set a member `obj.<name> = value`, applying a declared field-type contract
    /// on an `Instance` field. Shared by the tree-walker's `assign_to` `Member` arm
    /// and the bytecode VM's `Op::SetProp` so the two engines apply the field
    /// contract and panic byte-identically. Returns the assigned value (assignment
    /// is an expression). `value_span` anchors the contract panic exactly where the
    /// tree-walker's does; `obj_span` anchors the "cannot set property" error.
    pub(crate) fn set_member(
        &self,
        obj: &Value,
        name: &str,
        value: Value,
        obj_span: Span,
        value_span: Span,
    ) -> Result<Value, Control> {
        match obj {
            Value::Object(map) => {
                map.borrow_mut().insert(name.to_string(), value.clone());
                Ok(value)
            }
            Value::Instance(inst) => {
                let class = inst.borrow().class.clone();
                if let Some(schema) = lookup_field_schema(&class, name) {
                    if !check_type(&value, &schema.ty) {
                        return Err(contract_panic(&schema.ty, &value, value_span));
                    }
                }
                inst.borrow_mut()
                    .fields
                    .insert(name.to_string(), value.clone());
                Ok(value)
            }
            _ => Err(AsError::at(
                format!("cannot set property '{}' on this value", name),
                obj_span,
            )
            .into()),
        }
    }
}

/// Pure unary-operator dispatch shared by the tree-walker (`ExprKind::Unary`) and
/// the bytecode VM (`Op::Neg`/`Op::Not`). `span` anchors the Tier-2 panic so both
/// engines emit byte-identical diagnostics.
pub(crate) fn apply_unop(op: UnOp, v: Value, span: Span) -> Result<Value, Control> {
    match op {
        UnOp::Neg => match v {
            Value::Number(n) => Ok(Value::Number(-n)),
            Value::Decimal(d) => Ok(Value::Decimal(-d)),
            _ => Err(AsError::at("cannot negate a non-number", span).into()),
        },
        UnOp::Not => Ok(Value::Bool(!v.is_truthy())),
    }
}

/// Pure binary-operator dispatch shared by the tree-walker (`ExprKind::Binary`)
/// and the bytecode VM. Both engines evaluate the operands first, then call this
/// with the two values; `span` anchors every Tier-2 panic so diagnostics stay
/// byte-identical.
///
/// `And`/`Or`/`Coalesce` are NOT handled here — they short-circuit and so must be
/// evaluated by each engine before either operand is forced (the tree-walker
/// inlines them above the operand evals; the VM lowers them to jumps). Passing one
/// of those ops here is a programmer error (`unreachable!`).
///
/// Dispatch order mirrors `eval_expr`'s `ExprKind::Binary` arm exactly:
/// Eq/Ne (cross-type decimal equality) → Range (eager `array<number>`) → string
/// concat (`+` on two `Str`) → decimal arithmetic/ordering (either operand a
/// `Decimal`) → the two-`Number` path → the generic "requires two numbers" error.
pub(crate) fn apply_binop(
    op: BinOp,
    l: Value,
    r: Value,
    span: Span,
) -> Result<Value, Control> {
    // Eq/Ne: cross-type Decimal↔Number comparison before generic `==`.
    match op {
        BinOp::Eq => {
            let eq = decimal_cross_eq(&l, &r, span)?;
            return Ok(Value::Bool(eq));
        }
        BinOp::Ne => {
            let eq = decimal_cross_eq(&l, &r, span)?;
            return Ok(Value::Bool(!eq));
        }
        _ => {}
    }

    // Range `a..b`: eager, half-open `array<number>` with step 1, matching
    // ForRange and the `range()` builtin. Returns an Array, so it must be handled
    // before the generic "two numbers → Number" path below.
    if let BinOp::Range = op {
        let (start, end) = match (&l, &r) {
            (Value::Number(a), Value::Number(b)) => (*a, *b),
            _ => return Err(AsError::at("range bounds must be numbers", span).into()),
        };
        let mut items = Vec::new();
        let mut i = start;
        while i < end {
            items.push(Value::Number(i));
            i += 1.0;
        }
        return Ok(Value::Array(Rc::new(RefCell::new(items))));
    }

    // String concatenation: `+` joins two strings.
    if let BinOp::Add = op {
        if let (Value::Str(a), Value::Str(b)) = (&l, &r) {
            return Ok(Value::Str(format!("{}{}", a, b).into()));
        }
    }

    // Decimal arithmetic/comparison: triggered when either operand is Decimal.
    // The other side is coerced (Number→Decimal; non-finite→Tier-2 panic;
    // non-number/non-decimal → fall through to error).
    if matches!((&l, &r), (Value::Decimal(_), _) | (_, Value::Decimal(_))) {
        use crate::stdlib::decimal::coerce_to_decimal;
        let da = coerce_to_decimal(&l, span)?;
        let db = coerce_to_decimal(&r, span)?;
        if let (Some(a), Some(b)) = (da, db) {
            let result = match op {
                BinOp::Add => Value::Decimal(a + b),
                BinOp::Sub => Value::Decimal(a - b),
                BinOp::Mul => Value::Decimal(a * b),
                BinOp::Div => {
                    if b.is_zero() {
                        return Err(AsError::at("decimal division by zero", span).into());
                    }
                    Value::Decimal(a / b)
                }
                BinOp::Mod => {
                    if b.is_zero() {
                        return Err(AsError::at("decimal remainder by zero", span).into());
                    }
                    Value::Decimal(a % b)
                }
                // Ordering: both operands are already finite Decimals here
                // (coerce_to_decimal above Tier-2-panics on a non-finite Number).
                // This is the INTENTIONAL asymmetry vs equality: `decimal ==
                // Infinity` is a lenient `false` (decimal_cross_eq), but `decimal
                // < Infinity` panics — there is no sensible order. See
                // decimal_cross_eq's doc.
                BinOp::Lt => Value::Bool(a < b),
                BinOp::Le => Value::Bool(a <= b),
                BinOp::Gt => Value::Bool(a > b),
                BinOp::Ge => Value::Bool(a >= b),
                // Pow: not defined for Decimal — Tier-2 panic.
                BinOp::Pow => {
                    return Err(AsError::at(
                        "exponentiation (**) is not supported for decimal; use math.pow or convert to number",
                        span,
                    )
                    .into())
                }
                BinOp::Eq | BinOp::Ne | BinOp::Range => unreachable!("handled above"),
                BinOp::And | BinOp::Or | BinOp::Coalesce => {
                    unreachable!("short-circuit ops are not dispatched through apply_binop")
                }
            };
            return Ok(result);
        }
        // One operand was not a number or decimal — fall through to the generic
        // "operator requires two numbers or decimals" error.
    }

    let (a, b) = match (&l, &r) {
        (Value::Number(a), Value::Number(b)) => (*a, *b),
        _ => {
            return Err(AsError::at(
                "operator requires two numbers (or two decimals, or number and decimal)",
                span,
            )
            .into())
        }
    };
    let result = match op {
        BinOp::Add => Value::Number(a + b),
        BinOp::Sub => Value::Number(a - b),
        BinOp::Mul => Value::Number(a * b),
        BinOp::Div => Value::Number(a / b),
        BinOp::Mod => Value::Number(a % b),
        BinOp::Pow => Value::Number(a.powf(b)),
        BinOp::Lt => Value::Bool(a < b),
        BinOp::Le => Value::Bool(a <= b),
        BinOp::Gt => Value::Bool(a > b),
        BinOp::Ge => Value::Bool(a >= b),
        BinOp::Eq | BinOp::Ne | BinOp::Range => unreachable!("handled above"),
        BinOp::And | BinOp::Or | BinOp::Coalesce => {
            unreachable!("short-circuit ops are not dispatched through apply_binop")
        }
    };
    Ok(result)
}

/// Validate that a value is a usable array index (a non-negative integer).
/// Equality comparison that handles cross-type Decimal↔Number cases.
/// For all other pairs falls back to Value's PartialEq.
///
/// INTENTIONAL ASYMMETRY vs non-finite numbers: `decimal == Infinity`/`NaN`
/// returns `false` (lenient — a finite decimal simply isn't equal to it), but
/// `decimal < Infinity` (the ordering arm in the Binary evaluator) Tier-2 panics
/// because there is no sensible total order between a finite decimal and ±Inf/NaN.
/// Do not "unify" these: equality has a well-defined `false` answer; ordering does
/// not. (Mirrors IEEE-754, where NaN compares false for `==` and `<` alike, but we
/// additionally choose to hard-error on the nonsensical ordering.)
fn decimal_cross_eq(l: &Value, r: &Value, span: Span) -> Result<bool, Control> {
    match (l, r) {
        // Decimal vs Decimal: use the inner value's own equality.
        (Value::Decimal(a), Value::Decimal(b)) => Ok(a == b),
        // Decimal vs Number (or vice-versa): coerce the number to decimal.
        (Value::Decimal(a), Value::Number(n)) | (Value::Number(n), Value::Decimal(a)) => {
            if !n.is_finite() {
                // A non-finite float can never equal a finite decimal (lenient
                // false; the ordering path panics instead — see fn doc comment).
                return Ok(false);
            }
            use rust_decimal::prelude::FromPrimitive;
            let b = rust_decimal::Decimal::from_f64(*n).ok_or_else(|| {
                AsError::at("cannot convert number to decimal for comparison", span)
            })?;
            Ok(*a == b)
        }
        // All other pairs: generic structural equality.
        _ => Ok(l == r),
    }
}

fn array_index(v: &Value, span: Span) -> Result<usize, AsError> {
    match v {
        Value::Number(n) if n.fract() == 0.0 && *n >= 0.0 => Ok(*n as usize),
        Value::Number(_) => Err(AsError::at(
            "array index must be a non-negative integer",
            span,
        )),
        _ => Err(AsError::at("array index must be a number", span)),
    }
}

/// Pure index-read dispatch (`obj[idx]`) shared by the tree-walker
/// (`ExprKind::Index` read path in `eval_chain`) and the bytecode VM
/// (`Op::GetIndex`) so the two engines cannot drift on index semantics or panic
/// messages. There is one implementation.
///
/// Semantics (mirroring the original inline `eval_chain` arm exactly):
/// - `Array`: the index must be a non-negative integer `Number` (via
///   [`array_index`], anchored at `index_span`); an out-of-bounds index is a
///   Tier-2 panic (NOT nil), `"index {i} out of bounds (len {n})"` at `index_span`.
/// - `Object`: the index must be a `Str` key; a missing key yields `nil` (never a
///   panic); a non-string index panics `"object index must be a string"` at
///   `index_span`.
/// - anything else: `"cannot index this value"` at `obj_span`.
///
/// `obj_span` is the receiver's span (the tree-walker's `object.span`);
/// `index_span` is the whole index-expression's span (the tree-walker's
/// `expr.span`). The VM passes its single instruction span for both.
pub(crate) fn index_get(
    obj: &Value,
    idx: &Value,
    obj_span: Span,
    index_span: Span,
) -> Result<Value, AsError> {
    match obj {
        Value::Array(arr) => {
            let i = array_index(idx, index_span)?;
            let arr = arr.borrow();
            arr.get(i).cloned().ok_or_else(|| {
                AsError::at(
                    format!("index {} out of bounds (len {})", i, arr.len()),
                    index_span,
                )
            })
        }
        Value::Object(map) => match idx {
            Value::Str(key) => Ok(map.borrow().get(key.as_ref()).cloned().unwrap_or(Value::Nil)),
            _ => Err(AsError::at("object index must be a string", index_span)),
        },
        _ => Err(AsError::at("cannot index this value", obj_span)),
    }
}

/// The recv/next method name a native handle exposes for `for await` async
/// iteration, or `None` if the handle kind is not an async-iterable stream.
/// Both methods follow the `[value, err]` contract ending in a `nil` value.
#[allow(unused_variables)]
pub(crate) fn native_stream_method(kind: crate::value::NativeKind) -> Option<&'static str> {
    #[cfg(feature = "net")]
    {
        use crate::value::NativeKind::*;
        match kind {
            WsConnection => Some("recv"),
            SseStream => Some("next"),
            _ => None,
        }
    }
    #[cfg(not(feature = "net"))]
    {
        None
    }
}

/// Human-readable type name for diagnostics.
/// Human-readable message for a Tier-1 error value. If `err` is an Object with a
/// `message` field, that field's value is rendered; otherwise the whole value is.
/// Single source of truth shared by `expr!` (Unwrap) and `for await` error paths.
pub(crate) fn error_message(err: &Value) -> String {
    match err {
        Value::Object(o) => o
            .borrow()
            .get("message")
            .map(|m| m.to_string())
            .unwrap_or_else(|| err.to_string()),
        other => other.to_string(),
    }
}

pub(crate) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Nil => "nil",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::Decimal(_) => "decimal",
        Value::Str(_) => "string",
        Value::Builtin(_) | Value::Function(_) | Value::Closure(_) => "function",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Map(_) => "map",
        Value::Set(_) => "set",
        Value::Bytes(_) => "bytes",
        #[cfg(feature = "data")]
        Value::Regex(_) => "regex",
        Value::Native(n) => n.kind.type_name(),
        Value::NativeMethod(_) => "function",
        Value::Enum(_) => "enum",
        Value::EnumVariant(_) => "enum variant",
        Value::Class(_) => "class",
        Value::Instance(_) => "instance",
        Value::BoundMethod(_) | Value::Super(_) => "function",
        Value::Future(_) => "future",
        Value::Generator(_) => "generator",
        Value::GeneratorMethod(..) => "function",
        Value::ClassMethod(..) => "function",
    }
}

fn exported_names(stmt: &Stmt) -> Vec<String> {
    match stmt {
        Stmt::Let { name, .. } => vec![name.clone()],
        Stmt::Fn { name, .. } => vec![name.clone()],
        Stmt::Class { name, .. } => vec![name.clone()],
        Stmt::Enum { name, .. } => vec![name.clone()],
        Stmt::LetDestructure { names, rest, .. } => {
            let mut v = names.clone();
            if let Some((r, _)) = rest {
                v.push(r.clone());
            }
            v
        }
        Stmt::LetDestructureObject { bindings, rest, .. } => {
            let mut v: Vec<String> = bindings.iter().map(|b| b.binding.clone()).collect();
            if let Some((r, _)) = rest {
                v.push(r.clone());
            }
            v
        }
        _ => Vec::new(),
    }
}

/// Look up the declared schema for `field` on `class` or any superclass.
fn lookup_field_schema(
    class: &std::rc::Rc<crate::value::Class>,
    field: &str,
) -> Option<crate::value::FieldSchema> {
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        if let Some(s) = c.fields.get(field) {
            return Some(s.clone());
        }
        cur = c.superclass.clone();
    }
    None
}

/// The owner label for a `.from` validation diagnostic: at the root (empty
/// path) this is the class header `"{ClassName}.from"`; once recursion has
/// descended into a field, it echoes that field path (e.g. `u.addr`).
fn field_owner_label(path: &str, class_name: &str) -> String {
    if path.is_empty() {
        format!("{}.from", class_name)
    } else {
        path.to_string()
    }
}

fn control_to_aserror(c: Control, span: Span) -> AsError {
    match c {
        Control::Panic(e) => e,
        // Defensive fallback: `?`-propagation cannot escape a field-default
        // initializer through current surface syntax (a default is an expression,
        // not a fn body that could early-return a Result), so this arm is not
        // reachable today; kept to keep the conversion total.
        Control::Propagate(_) => AsError::at("unexpected ? propagation in a field default", span),
        // An exit() inside a field default expression is unreachable in normal
        // usage; convert defensively rather than silently swallowing it.
        Control::Exit(code) => AsError::at(
            format!("exit({}) called during field default init", code),
            span,
        ),
    }
}

/// Runtime contract check (spec §5). Eagerly checks parametric types to full depth.
/// Validate call arguments against a parameter list (exact arity OR rest), apply
/// each declared parameter type contract, and return the values to bind into the
/// callee's parameter slots in declaration order. For a rest parameter the
/// returned slot holds the collected `Value::Array` of the trailing arguments.
///
/// This is the single source of truth for function-call argument checking; it is
/// shared by the tree-walker (`run_body`) and the bytecode VM (`vm::run` CALL) so
/// arity/contract/rest behavior — message wording AND span — is byte-identical
/// across both engines. `span` is the CALL-site span; `what` is the callee's
/// name/description (e.g. the function name, `"function"`, or a method name).
pub(crate) fn check_call_args(
    params: &[crate::ast::Param],
    args: Vec<Value>,
    span: Span,
    what: &str,
) -> Result<Vec<Value>, Control> {
    let has_rest = params.last().is_some_and(|p| p.rest);
    if !has_rest {
        // Exact arity.
        if args.len() != params.len() {
            return Err(AsError::at(
                format!(
                    "{} expected {} argument(s), got {}",
                    what,
                    params.len(),
                    args.len()
                ),
                span,
            )
            .into());
        }
        let mut bound = Vec::with_capacity(params.len());
        for (p, a) in params.iter().zip(args.into_iter()) {
            if let Some(ty) = &p.ty {
                if !check_type(&a, ty) {
                    return Err(contract_panic(ty, &a, span));
                }
            }
            bound.push(a);
        }
        Ok(bound)
    } else {
        let n_fixed = params.len() - 1;
        if args.len() < n_fixed {
            return Err(AsError::at(
                format!(
                    "{} expected at least {} argument(s), got {}",
                    what,
                    n_fixed,
                    args.len()
                ),
                span,
            )
            .into());
        }
        let mut bound = Vec::with_capacity(params.len());
        let mut it = args.into_iter();
        for p in &params[..n_fixed] {
            let a = it.next().unwrap();
            if let Some(ty) = &p.ty {
                if !check_type(&a, ty) {
                    return Err(contract_panic(ty, &a, span));
                }
            }
            bound.push(a);
        }
        let rest_p = &params[n_fixed];
        let elem_ty = match &rest_p.ty {
            Some(crate::ast::Type::Array(inner)) => Some(inner.as_ref()),
            Some(other) => {
                return Err(AsError::at(
                    format!(
                        "a rest parameter type must be an array type (array<T>), got {}",
                        other
                    ),
                    span,
                )
                .into())
            }
            None => None,
        };
        let mut rest_vals = Vec::new();
        for a in it {
            if let Some(t) = elem_ty {
                if !check_type(&a, t) {
                    return Err(contract_panic(t, &a, span));
                }
            }
            rest_vals.push(a);
        }
        bound.push(Value::Array(std::rc::Rc::new(std::cell::RefCell::new(
            rest_vals,
        ))));
        Ok(bound)
    }
}

pub(crate) fn check_type(value: &Value, ty: &crate::ast::Type) -> bool {
    use crate::ast::Type;
    match ty {
        Type::Any => true,
        Type::Number => matches!(value, Value::Number(_)),
        Type::String => matches!(value, Value::Str(_)),
        Type::Bool => matches!(value, Value::Bool(_)),
        Type::Nil => matches!(value, Value::Nil),
        Type::Object => matches!(value, Value::Object(_)),
        // A VM-produced `Closure` is the bytecode analog of a tree-walker
        // `Function`; both are first-class callables, so `: fn` typing accepts
        // either. (The tree-walker never produces a `Closure`, so adding it here
        // is behavior-preserving for the tree-walker and closes a real contract
        // gap for the VM, which routes through this shared `check_type`.)
        Type::Fn => matches!(
            value,
            Value::Function(_) | Value::Closure(_) | Value::Builtin(_)
        ),
        Type::Error => matches!(value, Value::Object(_) | Value::Nil),
        Type::Array(elem) => match value {
            Value::Array(a) => a.borrow().iter().all(|v| check_type(v, elem)),
            _ => false,
        },
        Type::Result(inner) => match value {
            Value::Array(a) => {
                let b = a.borrow();
                b.len() == 2
                    && (check_type(&b[0], inner) || matches!(b[0], Value::Nil))
                    && check_type(&b[1], &Type::Error)
            }
            _ => false,
        },
        Type::Tuple(types) => match value {
            Value::Array(a) => {
                let b = a.borrow();
                b.len() == types.len() && b.iter().zip(types.iter()).all(|(v, t)| check_type(v, t))
            }
            _ => false,
        },
        Type::Union(a, b) => check_type(value, a) || check_type(value, b),
        Type::Named(name) => match value {
            Value::Instance(inst) => {
                let mut cur = Some(inst.borrow().class.clone());
                while let Some(c) = cur {
                    if &c.name == name {
                        return true;
                    }
                    cur = c.superclass.clone();
                }
                false
            }
            Value::EnumVariant(v) => &v.enum_name == name,
            _ => false,
        },
        Type::Map(k, v) => match value {
            Value::Map(m) => m
                .borrow()
                .iter()
                .all(|(mk, val)| check_type(&mk.to_value(), k) && check_type(val, v)),
            _ => false,
        },
        // A value satisfies `future<T>` iff it is a future. The inner `T` is the
        // type the future *resolves to*, which cannot be inspected until it is
        // awaited, so it is advisory/erased at the binding site.
        Type::Future(_) => matches!(value, Value::Future(_)),
        // `T?` ≡ `T | nil`.
        Type::Optional(inner) => check_type(value, inner) || matches!(value, Value::Nil),
    }
}

/// Build a contract-violation panic.
pub(crate) fn contract_panic(ty: &crate::ast::Type, value: &Value, span: Span) -> Control {
    AsError::at(
        format!(
            "type contract violated: expected {}, got {} ({})",
            ty,
            type_name(value),
            value
        ),
        span,
    )
    .into()
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;

    /// Extract the panic's AsError from a Control (test helper).
    fn panic_of(c: Control) -> AsError {
        match c {
            Control::Panic(e) => e,
            Control::Propagate(_) => panic!("expected a panic, got a `?` propagation"),
            Control::Exit(code) => panic!("expected a panic, got exit({code})"),
        }
    }

    /// Lex+parse+exec a program string, returning its captured `print` output.
    /// Panics (test failure) on a lex/parse error or a runtime panic. Runs under a
    /// `LocalSet` (and drains it) so M17 async-fn tasks behave like a real program.
    async fn run(src: &str) -> String {
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async { interp.exec(&stmts, &env).await.expect("program panicked") })
            .await;
        local.await; // drain spawned async-fn tasks
        interp.output()
    }

    /// Like `run`, but returns the captured std/log output (not `print` output).
    #[cfg(feature = "log")]
    async fn run_logs(src: &str) -> String {
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async { interp.exec(&stmts, &env).await.expect("program panicked") })
            .await;
        local.await;
        interp.log_output()
    }

    #[cfg(feature = "log")]
    #[tokio::test]
    async fn log_records_human_and_filtering() {
        let logs = run_logs(
            r#"
import * as log from "std/log"
log.setLevel("warn")
log.info("ignored", {a: 1})
log.warn("disk low", {pct: 92})
log.error("boom")
"#,
        )
        .await;
        assert!(!logs.contains("ignored"));
        assert!(logs.contains("[WARN]") && logs.contains("disk low") && logs.contains("pct=92"));
        assert!(logs.contains("[ERROR]") && logs.contains("boom"));
    }

    #[cfg(feature = "log")]
    #[tokio::test]
    async fn log_json_format_and_thunk() {
        let logs = run_logs(
            r#"
import * as log from "std/log"
log.setFormat("json")
log.info("saved", {userId: 5})
log.debug(() => "expensive")
"#,
        )
        .await;
        assert!(
            logs.contains("\"level\":\"info\"")
                && logs.contains("\"msg\":\"saved\"")
                && logs.contains("\"userId\":5")
        );
        assert!(!logs.contains("expensive"));
    }

    #[cfg(feature = "log")]
    #[test]
    fn log_level_from_env_parsing() {
        assert_eq!(log_level_from_env_str(Some("warn")), LogLevel::Warn);
        assert_eq!(log_level_from_env_str(Some("DEBUG")), LogLevel::Debug);
        assert_eq!(log_level_from_env_str(None), LogLevel::Info);
        assert_eq!(log_level_from_env_str(Some("nonsense")), LogLevel::Info);
    }

    #[cfg(feature = "log")]
    #[tokio::test]
    async fn log_reserved_keys_win_and_no_silent_drop() {
        let logs = run_logs(
            r#"
import * as log from "std/log"
log.setFormat("json")
log.info("saved", {level: "HACK", userId: 5})
"#,
        )
        .await;
        assert!(
            logs.contains("\"level\":\"info\""),
            "auto level must win: {logs}"
        );
        assert!(logs.contains("\"userId\":5"));
        assert!(!logs.contains("HACK"));
    }

    #[cfg(feature = "log")]
    #[tokio::test]
    async fn log_non_object_args_append_to_msg() {
        let logs = run_logs("import * as log from \"std/log\"\nlog.info(\"a\", \"b\", 3)").await;
        assert!(logs.contains("[INFO] a b 3"), "got: {logs}");
    }

    #[cfg(feature = "log")]
    #[tokio::test]
    async fn log_empty_msg_no_trailing_space() {
        let logs = run_logs("import * as log from \"std/log\"\nlog.warn()").await;
        assert!(logs.lines().any(|l| l == "[WARN]"), "got: {logs:?}");
    }

    #[tokio::test]
    async fn non_rest_arity_error_message_unchanged() {
        let e = run_err("fn f(a, b) {}\nf(1)").await;
        assert!(
            e.message.contains("expected 2 argument(s), got 1"),
            "got: {}",
            e.message
        );
    }

    #[tokio::test]
    async fn rest_param_collects_trailing_args_as_array() {
        let out = run("fn f(a, ...rest) { print(a)\n print(rest) }\nf(1)\nf(1, 2, 3)").await;
        assert_eq!(out, "1\n[]\n1\n[2, 3]\n");
    }

    #[tokio::test]
    async fn rest_param_too_few_fixed_args_panics() {
        let e = run_err("fn f(a, b, ...r) {}\nf(1)").await;
        assert!(e.message.contains("at least 2"), "got: {}", e.message);
    }

    #[tokio::test]
    async fn typed_rest_checks_each_element() {
        let e = run_err("fn f(...rest: array<number>) {}\nf(1, \"x\", 3)").await;
        assert!(
            e.message.to_lowercase().contains("number"),
            "got: {}",
            e.message
        );
        let out = run("fn f(...rest: array<number>) { print(rest) }\nf(1, 2)").await;
        assert_eq!(out, "[1, 2]\n");
    }

    /// Like `run`, but expects a runtime panic and returns its `AsError`.
    async fn run_err(src: &str) -> AsError {
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        let r = local
            .run_until(async { interp.exec(&stmts, &env).await })
            .await;
        local.await;
        match r {
            Err(Control::Panic(e)) => e,
            Ok(_) => panic!("expected a runtime panic, but the program succeeded"),
            Err(Control::Propagate(_)) => panic!("expected a panic, got a `?` propagation"),
            Err(Control::Exit(code)) => panic!("expected a panic, got exit({code})"),
        }
    }

    #[tokio::test]
    async fn spread_array_object_call_eval() {
        let out = run(r#"
let a = [1, 2]
let b = [...a, 3]
print(b)
let o = {x: 1}
let p = {...o, y: 2, x: 9}
print(p)
fn add(a, b, c) { return a + b + c }
print(add(...[1, 2, 3]))
"#)
        .await;
        assert_eq!(out, "[1, 2, 3]\n{x: 9, y: 2}\n6\n");
    }

    #[tokio::test]
    async fn spread_wrong_type_panics() {
        assert!(run_err("let a = [...5]")
            .await
            .message
            .contains("can only spread an array"));
        assert!(run_err("let o = {...5}")
            .await
            .message
            .contains("can only spread an object"));
    }

    #[tokio::test]
    async fn spread_non_array_as_call_args_panics() {
        let e = run_err("fn f(a) { return a }\nf(...5)").await;
        assert!(
            e.message
                .contains("can only spread an array as call arguments"),
            "got: {}",
            e.message
        );
    }

    #[tokio::test]
    async fn object_destructuring_binds_from_object_and_instance() {
        let out = run(r#"
let {a, b as local, missing} = {a: 1, b: 2}
print(a)
print(local)
print(missing)
class P { x: number
 y: number }
let p = P.from({x: 10, y: 20})
let {x, y} = p
print(x)
print(y)
"#)
        .await;
        assert_eq!(out, "1\n2\nnil\n10\n20\n");
    }

    #[tokio::test]
    async fn object_destructuring_on_non_object_panics() {
        let err = run_err(r#"let {a} = 5"#).await;
        assert!(err.message.contains("cannot destructure a non-object"));
    }

    #[tokio::test]
    async fn array_rest_destructuring() {
        let out = run("let [first, ...others] = [1, 2, 3, 4]\nprint(first)\nprint(others)\nlet [only, ...none] = [9]\nprint(none)").await;
        assert_eq!(out, "1\n[2, 3, 4]\n[]\n");
    }

    #[tokio::test]
    async fn object_rest_destructuring_excludes_source_keys() {
        let out = run("let {a, b as local, ...rest} = {a: 1, b: 2, c: 3, d: 4}\nprint(a)\nprint(local)\nprint(rest)").await;
        assert_eq!(out, "1\n2\n{c: 3, d: 4}\n");
    }

    #[tokio::test]
    async fn native_handle_fields_and_methods() {
        let interp = Interp::new();
        let mut fields = indexmap::IndexMap::new();
        fields.insert("pid".to_string(), Value::Number(42.0));
        let h = interp.register_resource(
            crate::value::NativeKind::ChildProcess,
            fields,
            ResourceState::Closed,
        );
        assert_eq!(type_name(&h), "childProcess");
        assert_eq!(
            interp.read_member(&h, "pid", Span::new(0, 0)).unwrap(),
            Value::Number(42.0)
        );
        let m = interp.read_member(&h, "wait", Span::new(0, 0)).unwrap();
        assert!(matches!(m, Value::NativeMethod(_)));
        assert_eq!(h.to_string(), format!("<native childProcess #{}>", 0));
        // The resource is in the table until taken.
        assert!(matches!(
            interp.take_resource(0),
            Some(ResourceState::Closed)
        ));
        assert!(interp.take_resource(0).is_none());
    }

    #[tokio::test]
    async fn string_escapes_and_single_quotes() {
        assert_eq!(run("print('hello')").await, "hello\n");
        assert_eq!(run("print(\"a\\tb\")").await, "a\tb\n");
        assert_eq!(run("print(\"quote: \\\"x\\\"\")").await, "quote: \"x\"\n");
        assert_eq!(run("print('it\\'s')").await, "it's\n");
        // a string with an escaped newline prints across two lines
        assert_eq!(run("print(\"line1\\nline2\")").await, "line1\nline2\n");
    }

    #[tokio::test]
    async fn ternary_operator() {
        assert_eq!(run("print(true ? 1 : 2)").await, "1\n");
        assert_eq!(run("print(1 > 2 ? \"a\" : \"b\")").await, "b\n");
        // Right-associative chain.
        assert_eq!(
            run("let x = 0\nprint(x < 0 ? \"neg\" : x == 0 ? \"zero\" : \"pos\")").await,
            "zero\n"
        );
        // Only the selected branch runs — the untaken branch would panic.
        assert_eq!(
            run("let a = [1]\nprint(len(a) > 5 ? a[99] : \"safe\")").await,
            "safe\n"
        );
        // Only nil/false are falsy: 0 and "" are truthy conditions.
        assert_eq!(run("print(0 ? \"t\" : \"f\")").await, "t\n");
        assert_eq!(run("print(nil ? \"t\" : \"f\")").await, "f\n");
    }

    #[tokio::test]
    async fn ternary_does_not_break_postfix_try() {
        // The `?` propagation operator still works in the presence of ternary.
        let src = "fn half(n) { if (n % 2 != 0) { return Err(\"odd\") }\nreturn Ok(n / 2) }\n\
                   fn run() { let x = half(10)?\nreturn Ok(x) }\n\
                   let [v, e] = run()\nprint(v)";
        assert_eq!(run(src).await, "5\n");
    }

    #[tokio::test]
    async fn template_interpolation_nested_string_literals() {
        // A bare string literal inside `${...}`.
        assert_eq!(run("print(`x=${\"hi\"}`)").await, "x=hi\n");
        // A nullish-coalescing default that is a string literal.
        assert_eq!(run("let a = nil\nprint(`v=${a ?? \"-\"}`)").await, "v=-\n");
        // A function call passing a string literal argument.
        assert_eq!(
            run("fn f(s) { return s }\nprint(`r=${f(\"yo\")}`)").await,
            "r=yo\n"
        );
        // Braces and `${` inside the nested string stay literal.
        assert_eq!(run("print(`${\"a}b{c ${d}\"}`)").await, "a}b{c ${d}\n");
        // A template nested inside another template's interpolation.
        assert_eq!(
            run("let n = \"Ada\"\nprint(`outer ${`inner ${n}`}`)").await,
            "outer inner Ada\n"
        );
    }

    #[tokio::test]
    async fn std_map_end_to_end() {
        let src = "import * as map from \"std/map\"\n\
                   let m = map.new()\n\
                   map.set(m, \"x\", 10)\n\
                   map.set(m, \"y\", 20)\n\
                   print(map.get(m, \"x\"))\n\
                   print(len(m))\n\
                   print(map.keys(m))\n\
                   print(map.values(m))";
        assert_eq!(run(src).await, "10\n2\n[\"x\", \"y\"]\n[10, 20]\n");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn std_json_end_to_end() {
        // The JSON source is written as a backtick template so the inner double
        // quotes can be written literally (a `"..."` literal would also work
        // now that `\"` escapes are supported, but the template reads cleaner).
        let src = "import * as json from \"std/json\"\n\
                   let [v, err] = json.parse(`{\"x\": 10, \"ys\": [1, 2]}`)\n\
                   print(v.x)\n\
                   print(v.ys[1])\n\
                   let [s, e2] = json.stringify({ a: 1, b: \"hi\" })\n\
                   print(s)";
        assert_eq!(run(src).await, "10\n2\n{\"a\":1,\"b\":\"hi\"}\n");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn std_encoding_end_to_end() {
        let src = "import * as encoding from \"std/encoding\"\n\
                   print(encoding.base64Encode(\"hi\"))\n\
                   print(encoding.hexEncode(\"AB\"))\n\
                   let [raw, e] = encoding.base64Decode(\"aGVsbG8=\")\n\
                   let [text, e2] = encoding.utf8Decode(raw)\n\
                   print(text)\n\
                   print(encoding.urlEncode(\"a b&c\"))";
        assert_eq!(run(src).await, "aGk=\n4142\nhello\na%20b%26c\n");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn std_regex_end_to_end() {
        let src = "import * as regex from \"std/regex\"\n\
                   let [re, err] = regex.compile(\"\\\\d+\")\n\
                   print(regex.test(re, \"abc123\"))\n\
                   print(regex.findAll(re, \"a1 b22 c333\"))\n\
                   print(regex.replace(re, \"x9y\", \"#\"))\n\
                   let m = regex.find(re, \"ab42cd\")\n\
                   print(m.text)\n\
                   print(m.index)\n\
                   print(type(re))";
        assert_eq!(
            run(src).await,
            "true\n[\"1\", \"22\", \"333\"]\nx#y\n42\n2\nregex\n"
        );
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn std_uuid_end_to_end() {
        assert_eq!(
            run("import * as uuid from \"std/uuid\"\nprint(len(uuid.v4()))").await,
            "36\n"
        );
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn std_csv_end_to_end() {
        let src = "import * as csv from \"std/csv\"\n\
                   let [rows, err] = csv.parse(\"name,age\\nAda,36\\nAlan,41\")\n\
                   print(rows[1][0])\n\
                   print(rows[2][1])\n\
                   let [text, e2] = csv.stringify([[\"a\", \"b\"], [1, 2]])\n\
                   print(text)";
        assert_eq!(run(src).await, "Ada\n41\na,b\n1,2\n\n");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn std_toml_end_to_end() {
        let src = "import * as toml from \"std/toml\"\n\
                   let [cfg, err] = toml.parse(\"name = \\\"ascript\\\"\\nversion = 11\")\n\
                   print(cfg.name)\n\
                   print(cfg.version)";
        assert_eq!(run(src).await, "ascript\n11\n");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn std_yaml_end_to_end() {
        let src = "import * as yaml from \"std/yaml\"\n\
                   let [doc, err] = yaml.parse(\"a: 1\\nb:\\n  - x\\n  - y\")\n\
                   print(doc.a)\n\
                   print(doc.b[1])";
        assert_eq!(run(src).await, "1\ny\n");
    }

    #[cfg(feature = "sys")]
    #[tokio::test]
    async fn std_env_end_to_end() {
        let src = "import * as env from \"std/env\"\n\
                   env.set(\"ASCRIPT_E2E_ENV_d4a1\", \"world\")\n\
                   print(env.get(\"ASCRIPT_E2E_ENV_d4a1\"))\n\
                   env.unset(\"ASCRIPT_E2E_ENV_d4a1\")\n\
                   print(env.get(\"ASCRIPT_E2E_ENV_d4a1\"))";
        assert_eq!(run(src).await, "world\nnil\n");
    }

    #[tokio::test]
    async fn user_can_shadow_builtins() {
        assert_eq!(run("let len = 5\nprint(len)").await, "5\n");
        assert_eq!(
            run("fn type(x) { return 99 }\nprint(type(1))").await,
            "99\n"
        );
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn named_import_colliding_with_builtin() {
        // regex exports `test`; importing it shadows the global test() builtin in this scope
        let out = run("import { test, compile } from \"std/regex\"\nlet [re, e] = compile(\"\\\\d+\")\nprint(test(re, \"a1\"))").await;
        assert_eq!(out, "true\n");
    }

    #[tokio::test]
    async fn range_as_general_expression() {
        assert_eq!(run("let r = 0..5\nprint(r)").await, "[0, 1, 2, 3, 4]\n");
        assert_eq!(run("print(2..2)").await, "[]\n");
        assert_eq!(
            run("import * as array from \"std/array\"\nprint(array.contains(1..4, 2))").await,
            "true\n"
        );
        // for-in over a non-literal range value (array)
        assert_eq!(
            run("let r = 0..3\nlet s = 0\nfor (i in r) { s = s + i }\nprint(s)").await,
            "3\n"
        );
        // common literal for-in still works (lazy ForRange path)
        assert_eq!(
            run("let s = 0\nfor (i in 0..4) { s = s + i }\nprint(s)").await,
            "6\n"
        );
        // precedence: .. tighter than comparison, looser than +
        assert_eq!(run("print(1+1..5)").await, "[2, 3, 4]\n");
    }

    #[tokio::test]
    async fn range_bounds_must_be_numbers() {
        let err = run_err("print(\"a\"..3)").await;
        assert!(err.message.contains("range bounds must be numbers"));
    }

    #[tokio::test]
    async fn let_without_initializer() {
        assert_eq!(run("let x\nx = 5\nprint(x)").await, "5\n");
        assert_eq!(run("let y: number\ny = 3\nprint(y)").await, "3\n");
        // uninitialized reads as nil
        assert_eq!(run("let z\nprint(z)").await, "nil\n");
        // const still requires initializer
        assert!(parse(&lex("const c").unwrap()).is_err());
    }

    #[tokio::test]
    async fn number_literals_hex_binary_scientific_underscore() {
        assert_eq!(
            run("print(0xFF)\nprint(0b1010)\nprint(1e3)\nprint(1_000)\nprint(0xFF_FF)").await,
            "255\n10\n1000\n1000\n65535\n"
        );
    }

    #[tokio::test]
    async fn map_type_contract_enforced() {
        let ok = run("import * as map from \"std/map\"\nlet m: map<string, number> = map.new()\nmap.set(m, \"a\", 1)\nprint(len(m))").await;
        assert_eq!(ok, "1\n");
        let err = run_err("let m: map<string, number> = 5").await;
        assert!(err.message.contains("type contract violated"));
    }

    #[tokio::test]
    async fn future_type_annotation_checks() {
        // Calling an async fn yields a future; the binding annotated `future<T>`
        // accepts it, and awaiting it produces the resolved value.
        let ok =
            run("async fn f(): number { return 1 }\nlet x: future<number> = f()\nprint(await x)")
                .await;
        assert_eq!(ok, "1\n");
        // A non-future violates the contract; the message names `future`.
        let err = run_err("let y: future<number> = 5").await;
        assert!(
            err.message.contains("future"),
            "message was {:?}",
            err.message
        );
        assert_eq!(
            err.message,
            "type contract violated: expected future<number>, got number (5)"
        );
    }

    #[test]
    fn check_type_fn_accepts_closure() {
        use crate::ast::Type;
        // The shared `check_type` is used by BOTH the tree-walker and the VM (via
        // `check_call_args`). A `: fn` contract must accept every first-class
        // callable: the tree-walker's `Function`/`Builtin` AND the VM's `Closure`
        // (the bytecode analog of a `Function`). Before the fix the `Type::Fn` arm
        // matched only `Function | Builtin`, so a VM-produced `Closure` passed to
        // an `fn`-typed binding was WRONGLY rejected by the contract.
        let proto = std::rc::Rc::new(crate::vm::chunk::FnProto {
            chunk: crate::vm::chunk::Chunk::new(),
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        });
        let closure = Value::Closure(crate::vm::value_ext::Closure::new(proto));
        assert!(
            check_type(&closure, &Type::Fn),
            "a VM Closure must satisfy a `: fn` contract"
        );
        // The tree-walker callables still satisfy `: fn`.
        assert!(check_type(&Value::Builtin("len".into()), &Type::Fn));
        // A non-callable still fails the `: fn` contract (behavior preserved).
        assert!(!check_type(&Value::Number(7.0), &Type::Fn));
        assert!(!check_type(&Value::Str("x".into()), &Type::Fn));
    }

    #[tokio::test]
    async fn future_type_display_round_trips() {
        use crate::ast::Type;
        assert_eq!(
            Type::Future(Box::new(Type::Number)).to_string(),
            "future<number>"
        );
        // Nested generic types Display correctly.
        let ty = Type::Future(Box::new(Type::Array(Box::new(Type::Number))));
        assert_eq!(ty.to_string(), "future<array<number>>");
    }

    #[tokio::test]
    async fn future_type_annotations_parse_in_positions() {
        // A function return-type annotation `: future<string>` parses (the body
        // would itself have to return a future to satisfy it at runtime).
        assert!(parse(&lex("fn g(): future<string> { return wrap() }").unwrap()).is_ok());
        // Nested `future<array<number>>` parses as a binding annotation.
        assert!(parse(&lex("let z: future<array<number>> = w").unwrap()).is_ok());
        // As a parameter type.
        assert!(parse(&lex("fn h(p: future<number>) { return p }").unwrap()).is_ok());
    }

    #[tokio::test]
    async fn map_self_reference_display_is_cycle_guarded() {
        // A self-referencing map must print without infinite recursion.
        let out = run("import * as map from \"std/map\"\nlet m = map.new()\nmap.set(m, \"self\", m)\nprint(len(m))\nprint(m)").await;
        assert_eq!(out, "1\nmap {\"self\": map {...}}\n");
    }

    #[tokio::test]
    async fn map_number_key_contract_and_canonicalization() {
        // map<number, string> with a string value and number key passes
        let ok = run("import * as map from \"std/map\"\nlet m: map<number, string> = map.new()\nmap.set(m, 1, \"a\")\nprint(len(m))").await;
        assert_eq!(ok, "1\n");
        // -0.0 and 0.0 collide → len stays 1
        let coll = run("import * as map from \"std/map\"\nlet m = map.new()\nmap.set(m, 0, \"x\")\nmap.set(m, -0.0, \"y\")\nprint(len(m))\nprint(map.get(m, 0))").await;
        assert_eq!(coll, "1\ny\n");
    }

    #[tokio::test]
    async fn core_len_type_range() {
        assert_eq!(run("print(len([1,2,3]))").await, "3\n");
        assert_eq!(run("print(len(\"hello\"))").await, "5\n");
        assert_eq!(run("print(len({a:1, b:2}))").await, "2\n");
        assert_eq!(run("print(type(1))").await, "number\n");
        assert_eq!(run("print(type(\"x\"))").await, "string\n");
        assert_eq!(run("print(type([1]))").await, "array\n");
        assert_eq!(run("print(type(nil))").await, "nil\n");
        assert_eq!(run("print(range(3))").await, "[0, 1, 2]\n");
        assert_eq!(run("print(range(2, 5))").await, "[2, 3, 4]\n");
        assert_eq!(run("print(range(0, 10, 3))").await, "[0, 3, 6, 9]\n");
        assert_eq!(run("print(range(5, 0, -2))").await, "[5, 3, 1]\n");
    }

    #[tokio::test]
    async fn len_of_wrong_type_panics() {
        let err = run_err("len(5)").await;
        assert!(err.message.contains("len() expects"));
    }

    #[tokio::test]
    async fn len_accepts_set() {
        assert_eq!(
            run("import * as set from \"std/set\"\nprint(len(set.from([1,2,3])))").await,
            "3\n"
        );
        assert_eq!(
            run("import * as set from \"std/set\"\nprint(len(set.new()))").await,
            "0\n"
        );
    }

    #[tokio::test]
    async fn range_error_paths_and_fractional() {
        // zero step → panic
        assert!(run_err("range(0, 5, 0)")
            .await
            .message
            .contains("step must not be zero"));
        // too many args → panic
        assert!(run_err("range(1, 2, 3, 4)")
            .await
            .message
            .contains("1 to 3 arguments"));
        // non-number arg → panic
        assert!(run_err("range(\"x\")")
            .await
            .message
            .contains("number arguments"));
        // zero args → panic (0 falls into the >3/other arm)
        assert!(run_err("range()")
            .await
            .message
            .contains("1 to 3 arguments"));
        // fractional step: pin the IEEE-754 accumulation behavior (end-exclusive).
        // The 4th element is 0.3+0.3+0.3 = 0.8999999999999999 (< 1, so included);
        // the next accumulation exceeds 1 and is excluded. Accumulation drift is expected.
        assert_eq!(
            run("print(range(0, 1, 0.3))").await,
            "[0, 0.3, 0.6, 0.8999999999999999]\n"
        );
    }

    #[tokio::test]
    async fn destructures_array_into_bindings() {
        let out =
            run("let [a, b] = [1, 2]\nprint(a)\nprint(b)\nlet [x, y] = [9]\nprint(x)\nprint(y)")
                .await;
        assert_eq!(out, "1\n2\n9\nnil\n");
    }

    #[tokio::test]
    async fn destructuring_non_array_panics() {
        let err = run_err("let [a, b] = 5").await;
        assert!(err.message.contains("cannot destructure"));
    }

    #[tokio::test]
    async fn enum_variants_access_and_equality() {
        let src = "enum Color { Red, Green, Blue }\nenum Status { Ok = 200, NotFound = 404 }\nprint(Color.Red)\nprint(Color.Red == Color.Red)\nprint(Color.Red == Color.Green)\nprint(Status.NotFound.value)\nprint(Status.Ok.name)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "Color.Red\ntrue\nfalse\n404\nOk\n");
    }

    #[tokio::test]
    async fn match_on_literals_and_wildcard() {
        let src = "fn label(n) { return match n { 0 => \"zero\", 1 | 2 => \"small\", _ => \"many\" } }\nprint(label(0))\nprint(label(2))\nprint(label(9))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "zero\nsmall\nmany\n");
    }

    #[tokio::test]
    async fn match_on_enum_variants() {
        let src = "enum Color { Red, Green, Blue }\nfn warm(c) { return match c { Color.Red => true, _ => false } }\nprint(warm(Color.Red))\nprint(warm(Color.Blue))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "true\nfalse\n");
    }

    #[tokio::test]
    async fn match_no_arm_panics() {
        let src = "match 5 { 1 => \"a\" }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("no matching arm"));
    }

    #[tokio::test]
    async fn match_with_variable_and_expression_patterns() {
        // A bare-variable pattern must work (value-equality, not arrow-function).
        let src = "let k = 2\nprint(match 2 { k => \"hit\", _ => \"miss\" })\nprint(match 3 { k => \"hit\", _ => \"miss\" })\nlet n = 5\nprint(match 6 { n + 1 => \"plus\", _ => \"no\" })";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "hit\nmiss\nplus\n");
    }

    // ----- Phase 8a: pattern matching (Option C) -----

    async fn run_out(src: &str) -> String {
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        interp.output()
    }

    #[tokio::test]
    async fn pat_regression_literals() {
        assert_eq!(
            run_out("print(match 0 {0=>\"z\",1|2=>\"s\",_=>\"m\"})\nprint(match 2 {0=>\"z\",1|2=>\"s\",_=>\"m\"})\nprint(match 9 {0=>\"z\",1|2=>\"s\",_=>\"m\"})").await,
            "z\ns\nm\n"
        );
    }

    #[tokio::test]
    async fn pat_regression_enum_ref() {
        assert_eq!(
            run_out("enum Color { Red, Green }\nprint(match Color.Red {Color.Red=>true,_=>false})")
                .await,
            "true\n"
        );
    }

    #[tokio::test]
    async fn pat_regression_defined_var_compare() {
        assert_eq!(
            run_out("let k=2\nprint(match 2 { k => \"eq\", _ => \"no\" })\nprint(match 3 { k => \"eq\", _ => \"no\" })").await,
            "eq\nno\n"
        );
    }

    #[tokio::test]
    async fn pat_option_c_bind() {
        // x is undefined -> binds the subject.
        assert_eq!(run_out("print(match 42 { x => x })").await, "42\n");
    }

    #[tokio::test]
    async fn pat_const_compare_footgun_avoided() {
        // target is defined -> value compare, not bind.
        assert_eq!(
            run_out("const target=5\nprint(match 5 { target => \"m\", _ => \"n\" })\nprint(match 6 { target => \"m\", _ => \"n\" })").await,
            "m\nn\n"
        );
    }

    #[tokio::test]
    async fn pat_array_result_pair() {
        let src = "fn f(u, e) { return match [u, e] { [u, nil] => u, [nil, e] => \"err\" } }\nprint(f(\"alice\", nil))\nprint(f(nil, \"boom\"))";
        assert_eq!(run_out(src).await, "alice\nerr\n");
    }

    #[tokio::test]
    async fn pat_array_rest() {
        assert_eq!(
            run_out("print(match [1,2,3] { [first, ...rest] => first + len(rest) })").await,
            "3\n"
        );
    }

    #[tokio::test]
    async fn pat_object_keyval_and_shorthand() {
        assert_eq!(
            run_out("print(match {method:\"GET\",path:\"/x\"} { {method:\"GET\", path} => path })")
                .await,
            "/x\n"
        );
        assert_eq!(
            run_out("print(match {a:1,b:2} { {a,b} => a+b })").await,
            "3\n"
        );
    }

    #[tokio::test]
    async fn pat_range() {
        assert_eq!(
            run_out("print(match 5 {1..=9=>\"d\",_=>\"big\"})").await,
            "d\n"
        );
        assert_eq!(
            run_out("print(match 12 {1..=9=>\"d\",_=>\"big\"})").await,
            "big\n"
        );
    }

    #[tokio::test]
    async fn pat_guard() {
        let src = "fn g(n) { return match n {_ if n<0=>\"neg\",0=>\"zero\",_=>\"pos\"} }\nprint(g(-3))\nprint(g(0))\nprint(g(7))";
        assert_eq!(run_out(src).await, "neg\nzero\npos\n");
    }

    #[tokio::test]
    async fn pat_guard_with_binding() {
        let src = "fn g(v, e) { return match [v,e] {[x,nil] if x>10=>\"big\",[x,nil]=>\"small\",_=>\"err\"} }\nprint(g(20,nil))\nprint(g(3,nil))\nprint(g(0,\"e\"))";
        assert_eq!(run_out(src).await, "big\nsmall\nerr\n");
    }

    #[tokio::test]
    async fn pat_no_arm_still_panics() {
        let src = "match 5 { 1 => \"a\" }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("no matching arm"));
    }

    #[tokio::test]
    async fn class_construction_fields_and_methods() {
        let src = "class Animal {\n  fn init(name) { self.name = name }\n  fn speak() { return self.name + \" makes a sound\" }\n}\nlet a = Animal(\"Rex\")\nprint(a.name)\nprint(a.speak())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "Rex\nRex makes a sound\n");
    }

    #[tokio::test]
    async fn class_without_init_rejects_args() {
        let src = "class Empty {}\nEmpty(1)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("no init"));
    }

    #[tokio::test]
    async fn typed_code_runs_without_enforcement_yet() {
        let src = "let x: number = 5\nfn f(a: number): number { return a + 1 }\nprint(f(x))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "6\n");
    }

    #[tokio::test]
    async fn let_contract_passes_and_fails() {
        // passes
        let src = "let x: number = 5\nprint(x)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "5\n");

        // fails
        let bad = "let x: number = \"oops\"";
        let stmts = parse(&lex(bad).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
        assert!(err.message.contains("expected number"));
    }

    #[tokio::test]
    async fn parametric_and_union_contracts() {
        // array<number> with a bad element fails
        let bad = "let xs: array<number> = [1, \"two\", 3]";
        let stmts = parse(&lex(bad).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        assert!(interp.exec(&stmts, &env).await.is_err());

        // union passes for either member
        let ok = "let a: number | nil = nil\nlet b: number | nil = 7\nprint(b)";
        let stmts = parse(&lex(ok).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "7\n");

        // Result<number>: Ok(5) passes, Ok("x") fails
        let r = "let r: Result<number> = Ok(5)\nprint(r[0])";
        let stmts = parse(&lex(r).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "5\n");
    }

    #[tokio::test]
    async fn result_contract_accepts_both_ok_and_err() {
        // Both Ok and Err must satisfy a Result<T> contract (spec §6).
        let src = "
fn parseNum(s): Result<number> {
  if (s == \"bad\") { return Err(\"not a number\") }
  return Ok(42)
}
let good: Result<number> = parseNum(\"ok\")
let bad: Result<number> = parseNum(\"bad\")
print(good[0])
print(bad[1].message)
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "42\nnot a number\n");
    }

    #[tokio::test]
    async fn param_contract_enforced() {
        let src = "fn double(n: number): number { return n * 2 }\nprint(double(\"x\"))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
        assert!(err.message.contains("expected number"));
    }

    #[tokio::test]
    async fn return_contract_enforced() {
        // returns a string but annotated number
        let src = "fn f(): number { return \"nope\" }\nf()";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
    }

    #[tokio::test]
    async fn typed_function_happy_path() {
        let src = "fn add(a: number, b: number): number { return a + b }\nprint(add(2, 3))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "5\n");
    }

    #[tokio::test]
    async fn contract_failure_is_recoverable() {
        // a contract panic is catchable by recover (it's a Panic, M5)
        let src = "fn f(n: number) { return n }\nlet r = recover(() => f(\"bad\"))\nprint(r[0])\nprint(r[1].message)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert!(interp.output().starts_with("nil\n"));
        assert!(interp.output().contains("type contract violated"));
    }

    #[tokio::test]
    async fn optional_type_accepts_value_and_nil() {
        // nil and a number both satisfy number?; a string does not.
        assert_eq!(eval_to_value("let x: number? = nil\nx").await, Value::Nil);
        assert_eq!(
            eval_to_value("let x: number? = 7\nx").await,
            Value::Number(7.0)
        );
    }

    #[tokio::test]
    async fn optional_type_rejects_wrong_type() {
        let src =
            "let r = recover(() => { let x: number? = \"bad\"\n return nil })\nprint(r[1].message)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert!(
            interp.output().contains("type contract violated"),
            "got: {}",
            interp.output()
        );
    }

    #[tokio::test]
    async fn declared_field_type_checked_on_assignment() {
        // Assigning a wrong-typed declared field panics (recoverable).
        let src = "class C { id: number\n fn init(v) { self.id = v } }\n\
                   let r = recover(() => C(\"bad\"))\nprint(r[1].message)";
        let out = run(src).await;
        assert!(out.contains("type contract violated"), "got: {out}");
    }

    #[tokio::test]
    async fn declared_field_default_applied_at_construction() {
        let src = "class C { role: string = \"guest\"\n fn init() {} }\n\
                   let c = C()\nprint(c.role)";
        let out = run(src).await;
        assert!(out.contains("guest"), "got: {out}");
    }

    #[tokio::test]
    async fn undeclared_field_stays_dynamic() {
        // A field the class did not declare is unchecked.
        let src = "class C { fn init() { self.x = 1\n self.x = \"now a string\" } }\n\
                   let c = C()\nprint(c.x)";
        let out = run(src).await;
        assert!(out.contains("now a string"), "got: {out}");
    }

    #[tokio::test]
    async fn inherited_field_type_checked_on_assignment() {
        // A field declared on the BASE class is type-checked when assigned from
        // a subclass instance (locks in lookup_field_schema's superclass walk).
        let src = "class A { id: number\n fn init() {} }\n\
                   class B extends A { fn init() { self.id = \"bad\" } }\n\
                   let r = recover(() => B())\nprint(r[1].message)";
        let out = run(src).await;
        assert!(out.contains("type contract violated"), "got: {out}");
    }

    #[tokio::test]
    async fn declared_field_default_type_checked_at_construction() {
        // A default whose value violates the declared type is a recoverable panic.
        let src = "class C { n: number = \"oops\"\n fn init() {} }\n\
                   let r = recover(() => C())\nprint(r[1] != nil)\nprint(r[1].message)";
        let out = run(src).await;
        assert!(out.contains("true"), "expected an err pair, got: {out}");
        assert!(out.contains("type contract violated"), "got: {out}");
    }

    #[tokio::test]
    async fn ok_and_err_construct_result_pairs() {
        let src = "let r = Ok(5)\nprint(r[0])\nprint(r[1])\nlet e = Err(\"boom\")\nprint(e[0])\nprint(e[1].message)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "5\nnil\nnil\nboom\n");
    }

    #[tokio::test]
    async fn assert_passes_and_panics() {
        // passing assert returns nil
        let ok = "assert(1 < 2)\nprint(\"ok\")";
        let stmts = parse(&lex(ok).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "ok\n");

        // failing assert panics with the message
        let bad = "assert(false, \"nope\")";
        let stmts = parse(&lex(bad).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("nope"));
    }

    #[tokio::test]
    async fn question_unwraps_ok_and_propagates_err() {
        // A function that uses `?`: returns the value on Ok, propagates [nil, err] on Err.
        let src = "
fn parse(x) {
  if (x < 0) { return Err(\"negative\") }
  return Ok(x * 2)
}
fn run(x) {
  let v = parse(x)?
  return Ok(v + 1)
}
let good = run(5)
print(good[0])
print(good[1])
let bad = run(-1)
print(bad[0])
print(bad[1].message)
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // run(5): parse->Ok(10), v=10, returns Ok(11) -> [11, nil]
        // run(-1): parse->Err, ? propagates [nil, {message:"negative"}]
        assert_eq!(interp.output(), "11\nnil\nnil\nnegative\n");
    }

    #[tokio::test]
    async fn question_on_non_result_panics() {
        let src = "let x = 5\nlet y = x?";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("requires a Result pair"));
    }

    #[tokio::test]
    async fn recover_catches_a_panic() {
        // A function that panics (index out of bounds) is recovered into [nil, err].
        let src = "
fn boom() {
  let a = [1]
  return a[10]
}
let r = recover(boom)
print(r[0])
print(r[1].message)
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // r[0] is nil; r[1].message carries the panic text (index out of bounds).
        assert!(interp.output().starts_with("nil\n"));
        assert!(interp.output().contains("out of bounds"));
    }

    #[tokio::test]
    async fn recover_passes_through_success() {
        let src = "
fn good() { return 42 }
let r = recover(good)
print(r[0])
print(r[1])
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "42\nnil\n");
    }

    async fn eval_to_value(src: &str) -> Value {
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let (last, rest) = stmts.split_last().expect("at least one statement");
        interp.exec(rest, &env).await.unwrap();
        match last {
            Stmt::Expr(e) => interp.eval_expr(e, &env).await.unwrap(),
            _ => panic!("last statement must be an expression"),
        }
    }

    #[tokio::test]
    async fn unwrap_returns_value_on_ok_pair() {
        assert_eq!(eval_to_value("[42, nil]!").await, Value::Number(42.0));
        assert_eq!(eval_to_value("Ok(7)!").await, Value::Number(7.0));
    }

    #[tokio::test]
    async fn unwrap_panics_on_err_pair_preserving_message() {
        // `!` on an error pair panics; recover round-trips the original message.
        let src = "let r = recover(() => Err(\"boom\")!)\nprint(r[1].message)";
        let out = run(src).await;
        assert!(out.contains("boom"), "got: {out}");
    }

    #[tokio::test]
    async fn unwrap_on_non_pair_is_a_panic() {
        let src = "let r = recover(() => 5!)\nprint(r[1] != nil)";
        let out = run(src).await;
        assert!(out.contains("true"), "got: {out}");
    }

    #[tokio::test]
    async fn from_builds_validated_instance() {
        let src = "class U { id: number\n name: string }\n\
                   let o = { id: 1, name: \"Ada\" }\n\
                   let u = U.from(o)\nprint(u.id)\nprint(u.name)";
        let out = run(src).await;
        assert!(out.contains("1") && out.contains("Ada"), "got: {out}");
    }

    #[tokio::test]
    async fn from_rejects_wrong_type_with_field_path() {
        let src = "class U { id: number\n name: string }\n\
                   let r = recover(() => U.from({ id: \"x\", name: \"Ada\" }))\nprint(r[1].message)";
        let out = run(src).await;
        assert!(
            out.contains("u.id") && out.contains("type contract violated"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn from_optional_and_default() {
        let src = "class U { id: number\n nick: string?\n role: string = \"guest\" }\n\
                   let u = U.from({ id: 2 })\nprint(u.nick == nil)\nprint(u.role)";
        let out = run(src).await;
        assert!(out.contains("true") && out.contains("guest"), "got: {out}");
    }

    #[tokio::test]
    async fn from_recurses_into_nested_class() {
        let src = "class Addr { zip: number }\nclass U { id: number\n addr: Addr }\n\
                   let u = U.from({ id: 1, addr: { zip: 90210 } })\nprint(u.addr.zip)";
        let out = run(src).await;
        assert!(out.contains("90210"), "got: {out}");
    }

    #[tokio::test]
    async fn from_nested_path_in_error() {
        let src = "class Addr { zip: number }\nclass U { id: number\n addr: Addr }\n\
                   let r = recover(() => U.from({ id: 1, addr: { zip: \"x\" } }))\nprint(r[1].message)";
        let out = run(src).await;
        assert!(out.contains("u.addr.zip"), "got: {out}");
    }

    #[tokio::test]
    async fn from_recurses_into_array_of_class() {
        let src = "class Tag { v: number }\nclass U { tags: array<Tag> }\n\
                   let u = U.from({ tags: [{ v: 1 }, { v: 2 }] })\nprint(u.tags[1].v)";
        let out = run(src).await;
        assert!(out.contains("2"), "got: {out}");
    }

    #[tokio::test]
    async fn from_recurses_into_map_of_class() {
        // A `map<K, Class>` field whose values are raw objects validates each
        // value into the class. (Maps are a distinct value kind from objects, so
        // the raw map is built with `map.new` from [key, value] pairs.)
        let src = "import * as map from \"std/map\"\n\
                   class Tag { v: number }\nclass U { tags: map<string, Tag> }\n\
                   let raw = map.new([[\"a\", { v: 1 }], [\"b\", { v: 2 }]])\n\
                   let u = U.from({ tags: raw })\nprint(map.get(u.tags, \"b\").v)";
        let out = run(src).await;
        assert!(out.contains("2"), "got: {out}");
    }

    #[tokio::test]
    async fn from_coerces_object_into_map_of_class() {
        // A raw JSON-shaped Object validates into a `map<string, Tag>` field, with
        // each nested object validated into a Tag instance.
        let src = "import * as map from \"std/map\"\n\
                   class Tag { v: number }\nclass W { byId: map<string, Tag> }\n\
                   let w = W.from({ byId: { \"1\": { v: 10 }, \"2\": { v: 20 } } })\n\
                   print(map.get(w.byId, \"1\").v)\nprint(map.get(w.byId, \"2\").v)";
        let out = run(src).await;
        assert!(out.contains("10") && out.contains("20"), "got: {out}");
    }

    #[tokio::test]
    async fn from_object_map_nested_path_in_error() {
        // A bad nested value inside an Object-sourced map reports a path like
        // `w.byId[1].v` — only the root class name is lowercased; field names
        // and Object map keys keep their original casing.
        let src = "class Tag { v: number }\nclass W { byId: map<string, Tag> }\n\
                   let r = recover(() => W.from({ byId: { \"1\": { v: \"oops\" } } }))\nprint(r[1].message)";
        let out = run(src).await;
        assert!(
            out.contains("w.byId[1].v") && out.contains("type contract violated"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn from_on_non_object_rejected() {
        let src = "class U { id: number }\n\
                   let r = recover(() => U.from(5))\nprint(r[1].message)";
        let out = run(src).await;
        assert!(out.contains("expects an object"), "got: {out}");
    }

    #[tokio::test]
    async fn from_missing_required_field_reports_path() {
        let src = "class U { id: number\n name: string }\n\
                   let r = recover(() => U.from({ id: 1 }))\nprint(r[1].message)";
        let out = run(src).await;
        assert!(
            out.contains("u.name") && out.contains("expected string"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn from_nested_non_object_where_class_expected() {
        // `coerce_field`'s Named `_ => Ok(val)` fall-through leaves a non-object
        // value for `check_type` to reject, with the field path.
        let src = "class A { x: number }\nclass V { a: A }\n\
                   let r = recover(() => V.from({ a: 5 }))\nprint(r[1].message)";
        let out = run(src).await;
        assert!(
            out.contains("v.a") && out.contains("type contract violated"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn from_strict_rejects_extra_keys() {
        let src = "class U { id: number }\n\
                   let r = recover(() => U.from({ id: 1, extra: true }, true))\nprint(r[1].message)";
        let out = run(src).await;
        assert!(out.contains("unexpected key 'extra'"), "got: {out}");
        // Lenient (default) ignores extras:
        let src2 = "class U { id: number }\nlet u = U.from({ id: 1, extra: true })\nprint(u.id)";
        let out2 = run(src2).await;
        assert!(out2.contains("1"), "got: {out2}");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn json_parse_with_class_validates() {
        let src = "import * as json from \"std/json\"\n\
                   class U { id: number\n name: string }\n\
                   let [u, err] = json.parse(\"{\\\"id\\\":1,\\\"name\\\":\\\"Ada\\\"}\", U)\n\
                   print(err == nil)\nprint(u.id)\nprint(u.name)";
        let out = run(src).await;
        assert!(out.contains("true") && out.contains("Ada"), "got: {out}");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn json_parse_with_class_fuses_errors() {
        // shape mismatch comes back as a Tier-1 err, not a panic
        let src = "import * as json from \"std/json\"\n\
                   class U { id: number }\n\
                   let [u, err] = json.parse(\"{\\\"id\\\":\\\"x\\\"}\", U)\n\
                   print(u == nil)\nprint(err != nil)";
        let out = run(src).await;
        assert!(out.contains("true"), "got: {out}");
        // bad JSON also comes back as err (parse channel)
        let src2 = "import * as json from \"std/json\"\nclass U { id: number }\n\
                    let [u, err] = json.parse(\"{not json\", U)\nprint(err != nil)";
        let out2 = run(src2).await;
        assert!(out2.contains("true"), "got: {out2}");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn json_parse_without_class_returns_raw_object() {
        // With NO class argument, json.parse behaves exactly as before.
        let src = "import * as json from \"std/json\"\n\
                   let [v, err] = json.parse(\"{\\\"id\\\":1,\\\"name\\\":\\\"Ada\\\"}\")\n\
                   print(err == nil)\nprint(v.id)\nprint(v.name)";
        let out = run(src).await;
        assert!(
            out.contains("true") && out.contains("Ada") && out.contains('1'),
            "got: {out}"
        );
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn json_parse_with_class_strict_flag() {
        // Default (lenient): an unknown key is ignored → a validated instance.
        let lenient = "import * as json from \"std/json\"\n\
                       class U { id: number }\n\
                       let [u, err] = json.parse(\"{\\\"id\\\":1,\\\"extra\\\":2}\", U)\n\
                       print(err == nil)\nprint(u.id)";
        let out = run(lenient).await;
        assert!(
            out.contains("true") && out.contains('1'),
            "lenient got: {out}"
        );
        // strict=true (trailing 3rd arg): the unknown key is rejected → fused err.
        let strict = "import * as json from \"std/json\"\n\
                      class U { id: number }\n\
                      let [u, err] = json.parse(\"{\\\"id\\\":1,\\\"extra\\\":2}\", U, true)\n\
                      print(u == nil)\nprint(err.message)";
        let out2 = run(strict).await;
        assert!(
            out2.contains("true") && out2.contains("unexpected key 'extra'"),
            "strict got: {out2}"
        );
    }

    #[tokio::test]
    async fn evaluates_arithmetic_with_precedence() {
        match eval_to_value("1 + 2 * 3").await {
            Value::Number(n) => assert_eq!(n, 7.0),
            other => panic!("expected number, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn print_writes_to_the_output_buffer() {
        let stmts = parse(&lex("print(1 + 2 * 3)").unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "7\n");
    }

    #[tokio::test]
    async fn capture_sink_buffers_output() {
        // The default `Interp::new()` uses `OutputSink::Capture`, which buffers
        // `print` output for read-back via `output()`.
        let out = run("print(1)\nprint(2)").await;
        assert_eq!(out, "1\n2\n");
    }

    #[tokio::test]
    async fn comparison_and_equality() {
        assert_eq!(eval_to_value("1 < 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("2 == 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("1 != 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("\"a\" == \"a\"").await, Value::Bool(true));
    }

    #[tokio::test]
    async fn string_concatenation() {
        // `Str + Str` concatenates.
        assert_eq!(
            eval_to_value("\"a\" + \"b\"").await,
            Value::Str("ab".into())
        );

        // `Str + Number` must error (no coercion).
        let stmts = parse(&lex("\"a\" + 1").unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        assert!(interp.exec(&stmts, &env).await.is_err());

        // `Number + Str` must error (no coercion in the other direction).
        let stmts = parse(&lex("1 + \"a\"").unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        assert!(interp.exec(&stmts, &env).await.is_err());
    }

    #[tokio::test]
    async fn exponent_evaluates() {
        assert_eq!(eval_to_value("2 ** 10").await, Value::Number(1024.0));
    }

    #[tokio::test]
    async fn short_circuit_and_coalesce() {
        assert_eq!(eval_to_value("false && nope").await, Value::Bool(false));
        assert_eq!(eval_to_value("true || nope").await, Value::Bool(true));
        assert_eq!(eval_to_value("nil ?? 5").await, Value::Number(5.0));
        assert_eq!(eval_to_value("3 ?? nope").await, Value::Number(3.0));
        assert_eq!(eval_to_value("!0").await, Value::Bool(false));
    }

    #[tokio::test]
    async fn calling_an_undefined_name_is_an_error() {
        // `nope` is not a binding, so resolving the callee fails before the call.
        let stmts = parse(&lex("nope(1)").unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined variable"));
    }

    #[tokio::test]
    async fn call_site_errors_carry_a_span() {
        // Undefined callee name: the resolution error must carry a span.
        let stmts = parse(&lex("nope(1)").unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined variable"));
        assert!(err.span.is_some());

        // Non-callable callee value: "not callable" error must carry the callee span.
        let stmts = parse(&lex("(1)(2)").unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("not callable"));
        assert!(err.span.is_some());
    }

    #[tokio::test]
    async fn let_binding_resolves() {
        let stmts = parse(&lex("let x = 5\nprint(x + 1)").unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "6\n");
    }

    #[tokio::test]
    async fn undefined_variable_errors_with_span() {
        let stmts = parse(&lex("print(missing)").unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined variable 'missing'"));
        assert!(err.span.is_some());
    }

    #[tokio::test]
    async fn optional_semicolons_are_accepted() {
        let stmts = parse(&lex("let x = 1; print(x);").unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "1\n");
    }

    #[tokio::test]
    async fn assignment_updates_a_mutable_binding() {
        let src = "let x = 1\nx = x + 4\nprint(x)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "5\n");
    }

    #[tokio::test]
    async fn compound_assignment_runs() {
        let src = "let x = 10\nx *= 3\nprint(x)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "30\n");
    }

    #[tokio::test]
    async fn assigning_to_const_errors() {
        let src = "const x = 1\nx = 2";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("immutable"));
    }

    #[tokio::test]
    async fn if_else_chooses_branch() {
        let src = "let x = 3\nif (x < 5) { print(\"small\") } else { print(\"big\") }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "small\n");
    }

    #[tokio::test]
    async fn else_if_chain() {
        let src = "let x = 7\nif (x < 5) { print(\"a\") } else if (x < 10) { print(\"b\") } else { print(\"c\") }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "b\n");
    }

    #[tokio::test]
    async fn block_scope_does_not_leak() {
        let src = "{ let y = 1 }\nprint(y)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined variable 'y'"));
    }

    #[tokio::test]
    async fn while_loop_accumulates() {
        let src = "let i = 1\nlet sum = 0\nwhile (i <= 5) { sum += i\ni += 1 }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "15\n");
    }

    #[tokio::test]
    async fn for_range_iterates_half_open() {
        let src = "let sum = 0\nfor (i in 0..5) { sum += i }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // 0 + 1 + 2 + 3 + 4
        assert_eq!(interp.output(), "10\n");
    }

    #[tokio::test]
    async fn for_range_loop_var_is_scoped_per_iteration() {
        let src = "for (i in 0..3) { print(i) }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "0\n1\n2\n");
    }

    #[tokio::test]
    async fn break_exits_loop_early() {
        let src = "let sum = 0\nfor (i in 0..10) { if (i == 5) { break }\nsum += i }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "10\n"); // 0+1+2+3+4
    }

    #[tokio::test]
    async fn continue_skips_iteration() {
        let src = "let sum = 0\nfor (i in 0..5) { if (i == 2) { continue }\nsum += i }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "8\n"); // 0+1+3+4
    }

    #[tokio::test]
    async fn break_in_while() {
        let src = "let i = 0\nwhile (true) { if (i >= 3) { break }\ni += 1 }\nprint(i)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "3\n");
    }

    #[tokio::test]
    async fn print_is_a_resolvable_builtin_value() {
        assert_eq!(eval_to_value("print").await, Value::Builtin("print".into()));
    }

    #[tokio::test]
    async fn break_outside_loop_errors_at_top_level() {
        let err = crate::run_source("break").await.unwrap_err();
        assert!(err.message.contains("outside of a loop"));
    }

    #[tokio::test]
    async fn calls_a_user_function() {
        let src = "fn add(a, b) { return a + b }\nprint(add(2, 3))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "5\n");
    }

    #[tokio::test]
    async fn recursion_works() {
        let src = "fn fact(n) { if (n <= 1) { return 1 }\nreturn n * fact(n - 1) }\nprint(fact(5))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "120\n");
    }

    #[tokio::test]
    async fn closures_capture_their_environment() {
        // makeAdder returns a function that closes over `x`.
        let src = "fn makeAdder(x) { fn adder(y) { return x + y }\nreturn adder }\nlet add10 = makeAdder(10)\nprint(add10(5))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "15\n");
    }

    #[tokio::test]
    async fn arity_mismatch_errors() {
        let src = "fn f(a, b) { return a }\nf(1)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("expected 2 argument"));
    }

    #[tokio::test]
    async fn function_without_return_yields_nil() {
        let src = "fn noop() { let x = 1 }\nprint(noop())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "nil\n");
    }

    #[tokio::test]
    async fn arrow_expression_body() {
        let src = "let double = x => x * 2\nprint(double(21))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "42\n");
    }

    #[tokio::test]
    async fn arrow_multi_param_and_closure() {
        let src = "let base = 100\nlet f = (a, b) => a + b + base\nprint(f(1, 2))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "103\n");
    }

    #[tokio::test]
    async fn arrow_block_body_with_return() {
        let src =
            "let f = (n) => { if (n > 0) { return \"pos\" }\nreturn \"nonpos\" }\nprint(f(5))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "pos\n");
    }

    #[tokio::test]
    async fn array_literal_and_indexing() {
        let src = "let a = [10, 20, 30]\nprint(a[0])\nprint(a[2])";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "10\n30\n");
    }

    #[tokio::test]
    async fn index_assignment() {
        let src = "let a = [1, 2, 3]\na[1] = 99\nprint(a[1])\nprint(a)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "99\n[1, 99, 3]\n");
    }

    #[tokio::test]
    async fn out_of_bounds_index_errors() {
        let src = "let a = [1]\nprint(a[5])";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("out of bounds"));
    }

    #[tokio::test]
    async fn object_literal_member_and_computed_access() {
        let src = "let o = { name: \"Ada\", age: 36 }\nprint(o.name)\nprint(o[\"age\"])\nprint(o.missing)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "Ada\n36\nnil\n");
    }

    #[tokio::test]
    async fn member_and_computed_assignment() {
        let src = "let o = { a: 1 }\no.b = 2\no[\"c\"] = 3\nprint(o.a + o.b + o.c)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "6\n");
    }

    #[tokio::test]
    async fn member_of_nil_errors() {
        let src = "let x = nil\nprint(x.foo)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("cannot read property 'foo' of nil"));
    }

    #[tokio::test]
    async fn optional_chaining_short_circuits_on_nil() {
        let src = "let o = { a: nil }\nprint(o?.a)\nprint(o.a?.deep)\nprint((o.a ?? 42))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // o?.a -> nil; o.a is nil so o.a?.deep -> nil; nil ?? 42 -> 42
        assert_eq!(interp.output(), "nil\nnil\n42\n");
    }

    #[tokio::test]
    async fn optional_chaining_reads_when_present() {
        let src = "let o = { a: { b: 7 } }\nprint(o?.a?.b)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "7\n");
    }

    #[tokio::test]
    async fn optional_chaining_short_circuits_rest_of_chain() {
        // a is nil: the WHOLE chain a?.b.c yields nil (not an error on .c).
        let src = "let a = nil\nprint(a?.b.c)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "nil\n");
    }

    #[tokio::test]
    async fn optional_chaining_full_chain_with_index_and_present() {
        // present chain reads through; nil mid-chain short-circuits the rest.
        let src = "let o = { a: { b: [10, 20] } }\nprint(o?.a.b[1])\nlet z = nil\nprint(z?.a.b[1])";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "20\nnil\n");
    }

    #[tokio::test]
    async fn parentheses_break_the_optional_chain() {
        // (a?.b) evaluates to nil, then .c on nil ERRORS (chain broken by parens).
        let src = "let a = nil\nprint((a?.b).c)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("cannot read property 'c' of nil"));
    }

    #[tokio::test]
    async fn for_of_iterates_array() {
        let src = "let total = 0\nfor (x of [10, 20, 30]) { total += x }\nprint(total)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "60\n");
    }

    #[tokio::test]
    async fn for_of_iterates_string_chars() {
        let src = "let out = \"\"\nfor (c of \"abc\") { out = out + c + \".\" }\nprint(out)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "a.b.c.\n");
    }

    #[tokio::test]
    async fn for_of_non_iterable_errors() {
        let src = "for (x of 42) { print(x) }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("not iterable"));
    }

    #[tokio::test]
    async fn template_string_interpolates() {
        let src = "let name = \"Ada\"\nlet n = 3\nprint(`hi ${name}, ${n + 1} times`)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "hi Ada, 4 times\n");
    }

    #[tokio::test]
    async fn nested_template_and_plain() {
        let src = "print(`outer ${ `inner ${1 + 1}` } end`)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "outer inner 2 end\n");
    }

    #[tokio::test]
    async fn inheritance_and_super() {
        let src = "class Animal {\n  fn init(name) { self.name = name }\n  fn speak() { return self.name + \" makes a sound\" }\n}\nclass Dog extends Animal {\n  fn init(name) { super.init(name) }\n  fn speak() { return super.speak() + \" - woof\" }\n}\nlet d = Dog(\"Rex\")\nprint(d.name)\nprint(d.speak())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "Rex\nRex makes a sound - woof\n");
    }

    #[tokio::test]
    async fn inherited_method_without_override() {
        let src = "class A { fn greet() { return \"hi\" } }\nclass B extends A {}\nlet b = B()\nprint(b.greet())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "hi\n");
    }

    #[tokio::test]
    async fn undefined_superclass_errors() {
        let src = "class B extends Nope {}";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined superclass"));
    }

    #[tokio::test]
    async fn named_type_contracts() {
        let src = "class Animal { fn init() { self.ok = true } }\nclass Dog extends Animal {}\nenum Color { Red, Green }\nfn pet(a: Animal): bool { return a.ok }\nlet d: Dog = Dog()\nprint(pet(d))\nlet c: Color = Color.Red\nprint(c.name)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // Dog is-an Animal (subclass), so pet(d) passes; c: Color accepts a Color variant.
        assert_eq!(interp.output(), "true\nRed\n");
    }

    #[tokio::test]
    async fn named_contract_rejects_wrong_type() {
        let src = "class Animal { fn init() {} }\nclass Plant { fn init() {} }\nfn pet(a: Animal) { return a }\npet(Plant())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
        assert!(err.message.contains("expected Animal"));
    }

    #[tokio::test]
    async fn async_fn_and_await_surface() {
        // M16-era surface test. M17: async fn schedules eagerly, so this runs via the
        // LocalSet-aware `run` helper rather than a bare `exec`. NB: `async (n) => ...`
        // arrow is also async, so `g(9)` returns a future that `await` drives.
        let src = "async fn fetch(x) { return x * 2 }\nlet r = await fetch(21)\nprint(r)\nprint(await 5)\nlet g = async (n) => n + 1\nprint(await g(9))";
        assert_eq!(run(src).await, "42\n5\n10\n");
    }

    #[tokio::test]
    async fn imports_std_math() {
        let out = run("import * as math from \"std/math\"\nprint(math.abs(-5))\nprint(math.pow(2, 8))\nprint(math.pi > 3.14)").await;
        assert_eq!(out, "5\n256\ntrue\n");
    }

    #[tokio::test]
    async fn std_string_end_to_end() {
        let src = "import * as string from \"std/string\"\n\
                   print(string.upper(\"hi\"))\n\
                   print(string.join(string.split(\"a-b-c\", \"-\"), \"+\"))\n\
                   print(string.format(\"{}={}\", \"x\", 9))\n\
                   print(string.padStart(\"5\", 3, \"0\"))";
        assert_eq!(run(src).await, "HI\na+b+c\nx=9\n005\n");
    }

    #[tokio::test]
    async fn std_bytes_end_to_end() {
        let src = "import * as bytes from \"std/bytes\"\n\
                   let b = bytes.alloc(2)\n\
                   bytes.set(b, 0, 222)\n\
                   bytes.set(b, 1, 173)\n\
                   print(len(b))\n\
                   print(type(b))\n\
                   print(bytes.toArray(b))\n\
                   print(bytes.readUint(b, 0, 2, \"be\"))";
        assert_eq!(run(src).await, "2\nbytes\n[222, 173]\n57005\n");
    }

    #[tokio::test]
    async fn std_convert_end_to_end() {
        let src = "import * as convert from \"std/convert\"\n\
                   let [n, err] = convert.parseNumber(\"42\")\n\
                   print(n)\n\
                   print(err)\n\
                   let [bad, e2] = convert.parseNumber(\"nope\")\n\
                   print(bad)\n\
                   print(e2.message)\n\
                   print(convert.parseInt(\"ff\", 16)[0])\n\
                   print(convert.toString(123))\n\
                   print(convert.toBool(0))";
        assert_eq!(
            run(src).await,
            "42\nnil\nnil\ncannot parse 'nope' as a number\n255\n123\ntrue\n"
        );
    }

    #[tokio::test]
    async fn std_object_end_to_end() {
        let src = "import * as object from \"std/object\"\n\
                   let p = { name: \"Ada\", age: 36 }\n\
                   print(object.keys(p))\n\
                   print(object.values(p))\n\
                   print(object.has(p, \"age\"))\n\
                   print(object.delete(p, \"age\"))\n\
                   print(object.keys(p))\n\
                   print(object.merge({ x: 1 }, { x: 2, y: 3 }))";
        assert_eq!(
            run(src).await,
            "[\"name\", \"age\"]\n[\"Ada\", 36]\ntrue\ntrue\n[\"name\"]\n{x: 2, y: 3}\n"
        );
    }

    #[tokio::test]
    async fn std_array_map_filter_reduce() {
        let src = "import * as array from \"std/array\"\n\
                   let xs = [1, 2, 3, 4]\n\
                   print(array.map(xs, (x) => x * 2))\n\
                   print(array.filter(xs, (x) => x % 2 == 0))\n\
                   print(array.reduce(xs, (a, x) => a + x, 0))";
        assert_eq!(run(src).await, "[2, 4, 6, 8]\n[2, 4]\n10\n");
    }

    #[tokio::test]
    async fn std_array_map_pointfree() {
        let src = "import * as array from \"std/array\"\nimport * as math from \"std/math\"\nprint(array.map([-1, -2, 3], math.abs))";
        assert_eq!(run(src).await, "[1, 2, 3]\n");
    }

    #[tokio::test]
    async fn std_array_mutation_and_access() {
        let src = "import * as array from \"std/array\"\n\
                   let xs = [1, 2]\n\
                   print(array.push(xs, 3))\n\
                   print(xs)\n\
                   print(array.pop(xs))\n\
                   print(array.get(xs, 0))\n\
                   print(array.get(xs, 9))\n\
                   print(array.contains(xs, 2))\n\
                   print(array.slice([10,20,30,40], 1, 3))";
        assert_eq!(run(src).await, "3\n[1, 2, 3]\n3\n1\nnil\ntrue\n[20, 30]\n");
    }

    #[tokio::test]
    async fn std_array_sort_default_and_comparator() {
        let src = "import * as array from \"std/array\"\n\
                   print(array.sort([3, 1, 2]))\n\
                   print(array.sort([\"b\", \"a\", \"c\"]))\n\
                   print(array.sort([3, 1, 2], (a, b) => b - a))";
        assert_eq!(
            run(src).await,
            "[1, 2, 3]\n[\"a\", \"b\", \"c\"]\n[3, 2, 1]\n"
        );
    }

    #[tokio::test]
    async fn std_array_sort_is_stable() {
        // comparator compares only the first element of each pair; equal keys keep input order
        let src = "import * as array from \"std/array\"\n\
                   let pairs = [[1, \"a\"], [1, \"b\"], [0, \"c\"], [1, \"d\"]]\n\
                   print(array.sort(pairs, (x, y) => x[0] - y[0]))";
        assert_eq!(
            run(src).await,
            "[[0, \"c\"], [1, \"a\"], [1, \"b\"], [1, \"d\"]]\n"
        );
    }

    #[tokio::test]
    async fn named_import_from_std() {
        let out =
            run("import { sqrt, max } from \"std/math\"\nprint(sqrt(144))\nprint(max(3, 7, 2))")
                .await;
        assert_eq!(out, "12\n7\n");
    }

    #[tokio::test]
    async fn unknown_std_module_errors() {
        let err = run_err("import { x } from \"std/nope\"").await;
        assert!(err.message.contains("unknown standard library module"));
    }

    #[tokio::test]
    async fn std_module_import_is_cached() {
        let out = run("import * as m1 from \"std/math\"\nimport { abs } from \"std/math\"\nprint(m1.floor(3.7))\nprint(abs(-2))").await;
        assert_eq!(out, "3\n2\n");
    }

    #[tokio::test]
    async fn std_time_now_and_durations() {
        let out = run("import * as time from \"std/time\"\nprint(time.seconds(2))\nprint(time.now() > 1700000000000)").await;
        assert_eq!(out, "2000\ntrue\n");
    }

    #[tokio::test]
    async fn std_time_sleep_suspends() {
        // sleep a tiny amount; assert it completes and returns nil
        let out =
            run("import * as time from \"std/time\"\nawait time.sleep(5)\nprint(\"done\")").await;
        assert_eq!(out, "done\n");
    }

    #[tokio::test]
    async fn unawaited_async_call_is_cancelled_but_spawn_detaches() {
        // Structured concurrency / cancel-on-drop (M17): calling an `async fn` and
        // immediately discarding the future cancels it — the future's last handle
        // drops at the end of the expression statement, aborting the task before it
        // runs, so its side effect does NOT appear. `task.spawn(...)` is the
        // explicit opt-out: it detaches the task, which runs to completion (its
        // side effect appears, produced during the top-level drain).
        let cancelled = run("import * as time from \"std/time\"\n\
             async fn work() { await time.sleep(5) print(\"worked\") }\n\
             work()\n\
             print(\"main\")")
        .await;
        assert!(cancelled.contains("main\n"), "got: {cancelled:?}");
        assert!(
            !cancelled.contains("worked"),
            "unawaited call must be cancelled: {cancelled:?}"
        );

        let detached = run("import * as time from \"std/time\"\n\
             import * as task from \"std/task\"\n\
             async fn work() { await time.sleep(5) print(\"worked\") }\n\
             task.spawn(work())\n\
             print(\"main\")")
        .await;
        assert!(detached.contains("main\n"), "got: {detached:?}");
        assert!(
            detached.contains("worked\n"),
            "spawned task must run: {detached:?}"
        );
    }

    // ---- M17 Phase 2: futures & real async ----

    #[tokio::test]
    async fn async_call_returns_future_awaited_for_value() {
        let out = run("async fn answer() { return 42 }\n\
             let f = answer()\n\
             print(type(f))\n\
             print(await f)")
        .await;
        assert_eq!(out, "future\n42\n");
    }

    #[tokio::test]
    async fn await_on_non_future_is_identity() {
        assert_eq!(run("print(await 5)").await, "5\n");
        assert_eq!(run("print(await \"hi\")").await, "hi\n");
    }

    #[tokio::test]
    async fn nested_await_of_already_resolved_value() {
        // `await await f`: the first await yields 7 (a number), the second is identity.
        let out = run("async fn f() { return 7 }\nprint(await await f())").await;
        assert_eq!(out, "7\n");
    }

    #[tokio::test]
    async fn two_async_calls_run_concurrently() {
        // Both tasks sleep 30ms then return; started before either is awaited, so
        // total wall-time is ~max(30,30), not ~60. Assert results plus a lenient
        // upper bound on elapsed time.
        let out = run("import * as time from \"std/time\"\n\
             import * as t from \"std/task\"\n\
             async fn job(n) { await time.sleep(30) return n }\n\
             let a = job(1)\n\
             let b = job(2)\n\
             let start = time.monotonic()\n\
             print(await a)\n\
             print(await b)\n\
             let elapsed = time.monotonic() - start\n\
             print(elapsed < 200)")
        .await;
        assert_eq!(out, "1\n2\ntrue\n");
    }

    #[tokio::test]
    async fn gather_preserves_input_order() {
        let out = run("import * as time from \"std/time\"\n\
             import * as task from \"std/task\"\n\
             async fn job(ms, n) { await time.sleep(ms) return n }\n\
             let r = await task.gather([job(40, \"a\"), job(5, \"b\"), job(20, \"c\")])\n\
             print(r)")
        .await;
        // Despite different completion times, results are in INPUT order.
        assert_eq!(out, "[\"a\", \"b\", \"c\"]\n");
    }

    #[tokio::test]
    async fn gather_of_empty_array_is_empty() {
        let out = run("import * as task from \"std/task\"\nprint(await task.gather([]))").await;
        assert_eq!(out, "[]\n");
    }

    #[tokio::test]
    async fn gather_mixes_futures_and_plain_values() {
        let out = run("import * as task from \"std/task\"\n\
             async fn f() { return 1 }\n\
             print(await task.gather([f(), 2, f()]))")
        .await;
        assert_eq!(out, "[1, 2, 1]\n");
    }

    #[tokio::test]
    async fn race_returns_first_to_resolve() {
        let out = run("import * as time from \"std/time\"\n\
             import * as task from \"std/task\"\n\
             async fn job(ms, n) { await time.sleep(ms) return n }\n\
             print(await task.race([job(50, \"slow\"), job(5, \"fast\")]))")
        .await;
        assert_eq!(out, "fast\n");
    }

    #[tokio::test]
    async fn timeout_fast_future_yields_ok_pair() {
        let out = run("import * as time from \"std/time\"\n\
             import * as task from \"std/task\"\n\
             async fn quick() { await time.sleep(5) return \"v\" }\n\
             let r = await task.timeout(500, quick())\n\
             print(r[0])\n\
             print(r[1])")
        .await;
        assert_eq!(out, "v\nnil\n");
    }

    #[tokio::test]
    async fn timeout_slow_future_yields_err_pair() {
        let out = run("import * as time from \"std/time\"\n\
             import * as task from \"std/task\"\n\
             async fn slow() { await time.sleep(200) return \"v\" }\n\
             let r = await task.timeout(20, slow())\n\
             print(r[0])\n\
             print(r[1].message)")
        .await;
        assert!(out.starts_with("nil\n"), "got: {out:?}");
        assert!(out.contains("timed out"), "got: {out:?}");
    }

    #[tokio::test]
    async fn panic_propagates_across_task_boundary() {
        // A panic raised inside a spawned async-fn task re-surfaces at the await site.
        // `assert(false, msg)` is the spec's Tier-2 panic primitive.
        let err = run_err(
            "async fn boom() { assert(false, \"kaboom\") }\n\
             await boom()",
        )
        .await;
        assert!(err.message.contains("kaboom"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn question_propagation_across_await() {
        // An async fn returning a [nil, err] Result, awaited then `?`-propagated.
        let out = run("async fn fails() { return Err(\"nope\") }\n\
             fn caller() {\n\
               let v = (await fails())?\n\
               return Ok(v)\n\
             }\n\
             let r = caller()\n\
             print(r[0])\n\
             print(r[1].message)")
        .await;
        assert_eq!(out, "nil\nnope\n");
    }

    #[tokio::test]
    async fn question_propagation_across_await_ok_path() {
        let out = run("async fn ok() { return Ok(99) }\n\
             fn caller() {\n\
               let v = (await ok())?\n\
               return Ok(v)\n\
             }\n\
             let r = caller()\n\
             print(r[0])")
        .await;
        assert_eq!(out, "99\n");
    }

    #[tokio::test]
    async fn spawn_wraps_sync_function_value() {
        let out = run("import * as task from \"std/task\"\n\
             let f = task.spawn(() => 7)\n\
             print(type(f))\n\
             print(await f)")
        .await;
        assert_eq!(out, "future\n7\n");
    }

    #[tokio::test]
    async fn spawn_of_async_call_returns_its_future() {
        let out = run("import * as time from \"std/time\"\n\
             import * as task from \"std/task\"\n\
             async fn job() { await time.sleep(5) return \"done\" }\n\
             let h = task.spawn(job)\n\
             print(await h)")
        .await;
        assert_eq!(out, "done\n");
    }

    #[tokio::test]
    async fn spawn_of_existing_future_passes_through() {
        let out = run("import * as task from \"std/task\"\n\
             async fn f() { return 3 }\n\
             let fut = f()\n\
             let same = task.spawn(fut)\n\
             print(await same)")
        .await;
        assert_eq!(out, "3\n");
    }

    #[tokio::test]
    async fn class_async_method_returns_future() {
        let out = run("import * as time from \"std/time\"\n\
             class Worker {\n\
               async fn work(n) { await time.sleep(5) return n * 2 }\n\
             }\n\
             let w = Worker()\n\
             let f = w.work(21)\n\
             print(type(f))\n\
             print(await f)")
        .await;
        assert_eq!(out, "future\n42\n");
    }

    #[tokio::test]
    async fn await_inside_a_loop() {
        let out = run("import * as time from \"std/time\"\n\
             async fn job(n) { await time.sleep(2) return n }\n\
             let total = 0\n\
             for (i in [1, 2, 3]) { total = total + (await job(i)) }\n\
             print(total)")
        .await;
        // 1 + 2 + 3 = 6
        assert_eq!(out, "6\n");
    }

    #[tokio::test]
    async fn spawn_type_misuse_panics() {
        let err = run_err("import * as task from \"std/task\"\ntask.spawn(5)").await;
        assert!(
            err.message.contains("future or a 0-argument function"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn gather_type_misuse_panics() {
        let err = run_err("import * as task from \"std/task\"\ntask.gather(5)").await;
        assert!(
            err.message.contains("expects an array"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn std_time_monotonic_elapsed() {
        // monotonic measures elapsed; after a sleep it must advance
        let out = run("import * as time from \"std/time\"\n\
                       let a = time.monotonic()\n\
                       await time.sleep(10)\n\
                       let b = time.monotonic()\n\
                       print(b > a)")
        .await;
        assert_eq!(out, "true\n");
    }

    // ─── time.interval ───────────────────────────────────────────────────────

    /// interval: call tick() N times, assert N ticks completed and elapsed is
    /// at least (N-1)*interval_ms (loose lower bound to avoid flakiness).
    #[tokio::test]
    async fn std_time_interval_ticks() {
        let out = run(r#"
import * as time from "std/time"
let iv = time.interval(5)
let start = time.monotonic()
let i = 0
while (i < 4) {
    await iv.tick()
    i = i + 1
}
let elapsed = time.monotonic() - start
print(i)
print(elapsed >= 10)
"#)
        .await;
        assert_eq!(out, "4\ntrue\n");
    }

    // ─── time.debounce ───────────────────────────────────────────────────────

    /// debounce: call the wrapper 5 times in rapid succession; after waiting
    /// longer than the debounce window the underlying fn should have run exactly
    /// once (trailing-edge collapse).
    #[tokio::test]
    async fn std_time_debounce_collapses_rapid_calls() {
        let out = run(r#"
import * as time from "std/time"
import * as task from "std/task"
let counter = [0]
let fn_inc = () => { counter[0] = counter[0] + 1 }
let debounced = time.debounce(fn_inc, 20)
debounced()
debounced()
debounced()
debounced()
debounced()
await time.sleep(60)
print(counter[0])
"#)
        .await;
        assert_eq!(out, "1\n");
    }

    // ─── time.throttle ───────────────────────────────────────────────────────

    /// throttle: burst-call the wrapper many times within the window; the
    /// underlying fn should have fired exactly once (leading-edge).
    #[tokio::test]
    async fn std_time_throttle_leading_edge_once_per_window() {
        let out = run(r#"
import * as time from "std/time"
let counter = [0]
let fn_inc = () => { counter[0] = counter[0] + 1 }
let throttled = time.throttle(fn_inc, 50)
throttled()
throttled()
throttled()
throttled()
print(counter[0])
"#)
        .await;
        assert_eq!(out, "1\n");
    }

    /// interval: a sub-1ms duration (truncates to 0) and an outright 0 must be a
    /// catchable Tier-2 panic (Control::Panic), NOT a raw Rust panic crashing the
    /// process inside tokio ("period must be non-zero").
    #[tokio::test]
    async fn std_time_interval_sub_millisecond_is_tier2_panic() {
        for src in [
            "import * as time from \"std/time\"\ntime.interval(0.5)",
            "import * as time from \"std/time\"\ntime.interval(0)",
        ] {
            let result = crate::run_source(src).await;
            assert!(
                result.is_err(),
                "interval with a zero-rounding ms must be a Tier-2 panic, got: {result:?}"
            );
        }
    }

    /// debounce: a later call within the window cancels the earlier pending
    /// fire, so only the LAST call fires once. This exercises the per-call
    /// `.abort()` of the previous AbortHandle (the live cancellation path that
    /// trailing-edge collapse depends on). NOTE: GC-on-drop of an unreachable
    /// wrapper is NOT script-observable — resources live in the table until the
    /// interp tears down; `DebounceState::Drop`'s `.abort()` is unit-tested
    /// directly in `time_timers.rs` (`drop_aborts_pending_task`).
    #[tokio::test]
    async fn std_time_debounce_later_call_cancels_earlier() {
        let out = run(r#"
import * as time from "std/time"
let last = ["none"]
let count = [0]
let debounced = time.debounce((tag) => { last[0] = tag; count[0] = count[0] + 1 }, 25)
debounced("a")
await time.sleep(10)   // within the 25ms window → "a" still pending
debounced("b")         // cancels "a", reschedules for "b"
await time.sleep(60)   // let "b" fire
print(count[0])        // 1 — "a" was cancelled, only "b" fired
print(last[0])         // "b"
"#)
        .await;
        assert_eq!(out, "1\nb\n");
    }

    /// debounce with an ASYNC callback: the deferred task must DRIVE the inner
    /// future to completion (calling an `async fn` returns a Future without
    /// running the body). Async callbacks are the primary debounce use case
    /// (debounced save), so the body must actually run.
    #[tokio::test]
    async fn std_time_debounce_drives_async_callback() {
        let out = run(r#"
import * as time from "std/time"
let c = [0]
let d = time.debounce(async () => { c[0] = c[0] + 1 }, 15)
d()
d()
d()
await time.sleep(50)
print(c[0])   // 1 — the async body ran exactly once (trailing edge)
"#)
        .await;
        assert_eq!(out, "1\n");
    }

    /// throttle with an ASYNC callback: the leading-edge call must drive the
    /// inner future to completion so the async body actually runs.
    #[tokio::test]
    async fn std_time_throttle_drives_async_callback() {
        let out = run(r#"
import * as time from "std/time"
let t = [0]
let th = time.throttle(async () => { t[0] = t[0] + 1 }, 50)
th()
th()
await time.sleep(5)
print(t[0])   // 1 — the async body ran once on the leading edge
"#)
        .await;
        assert_eq!(out, "1\n");
    }

    #[cfg(feature = "datetime")]
    #[tokio::test]
    async fn std_date_end_to_end() {
        let src = "import * as date from \"std/date\"\n\
                   let [d, err] = date.parse(\"2021-06-15T12:30:00Z\")\n\
                   print(d.year)\n\
                   print(d.month)\n\
                   print(date.format(d, \"%Y/%m/%d\"))\n\
                   let later = date.addDays(d, 10)\n\
                   print(later.day)\n\
                   print(date.diffMs(later, d))";
        assert_eq!(run(src).await, "2021\n6\n2021/06/15\n25\n864000000\n");
    }

    #[cfg(feature = "intl")]
    #[tokio::test]
    async fn std_intl_end_to_end() {
        let src = "import * as intl from \"std/intl\"\n\
                   print(intl.formatNumber(1234567, \"en-US\"))\n\
                   print(intl.formatNumber(1234567, \"de-DE\"))\n\
                   print(intl.caseUpper(\"istanbul\", \"tr\"))\n\
                   print(intl.caseUpper(\"istanbul\", \"en\"))\n\
                   print(intl.compare(\"apple\", \"banana\", \"en\"))";
        assert_eq!(
            run(src).await,
            "1,234,567\n1.234.567\nİSTANBUL\nISTANBUL\n-1\n"
        );
    }

    // ─── exit() builtin unit tests ────────────────────────────────────────────

    /// Helper: exec a program and return the raw `Result<Flow, Control>`.
    async fn exec_raw(src: &str) -> Result<Flow, Control> {
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        let r = local
            .run_until(async { interp.exec(&stmts, &env).await })
            .await;
        local.await;
        r
    }

    #[tokio::test]
    async fn exit_3_produces_control_exit_3() {
        match exec_raw("exit(3)").await {
            Err(Control::Exit(3)) => {}
            other => panic!("expected Control::Exit(3), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_0_produces_control_exit_0() {
        match exec_raw("exit(0)").await {
            Err(Control::Exit(0)) => {}
            other => panic!("expected Control::Exit(0), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_no_arg_produces_control_exit_0() {
        match exec_raw("exit()").await {
            Err(Control::Exit(0)) => {}
            other => panic!("expected Control::Exit(0) for exit(), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recover_does_not_catch_exit() {
        // `recover(() => { exit(5) })` must NOT catch the exit — it passes through.
        match exec_raw("recover(() => { exit(5) })").await {
            Err(Control::Exit(5)) => {}
            other => panic!("expected Control::Exit(5) to pass through recover, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_out_of_range_is_tier2_panic() {
        match exec_raw("exit(300)").await {
            Err(Control::Panic(e)) => {
                assert!(
                    e.message.contains("0..=255"),
                    "expected 0..=255 in panic message, got: {}",
                    e.message
                );
            }
            other => panic!("expected Control::Panic for exit(300), got {other:?}"),
        }
    }

    // ---- decimal operator overloading tests ----

    /// THE HEADLINE: 0.1 + 0.2 == 0.3 is exact with decimals (unlike f64).
    #[tokio::test]
    async fn decimal_headline_exactness() {
        let out = run(r#"
import * as decimal from "std/decimal"
let result = decimal.from("0.1") + decimal.from("0.2") == decimal.from("0.3")
print(result)
"#)
        .await;
        assert_eq!(out.trim(), "true");
    }

    #[tokio::test]
    async fn decimal_arithmetic_basic() {
        let out = run(r#"
import * as decimal from "std/decimal"
let a = decimal.from("1.50")
let b = decimal.from("2.50")
print(a + b)
print(b - a)
print(a * b)
"#)
        .await;
        assert_eq!(out.trim(), "4.00\n1.00\n3.7500");
    }

    #[tokio::test]
    async fn decimal_times_number() {
        // decimal.from(2) * 3  == decimal 6
        let out = run(r#"
import * as decimal from "std/decimal"
let d = decimal.from(2) * 3
print(d)
"#)
        .await;
        assert_eq!(out.trim(), "6");
    }

    #[tokio::test]
    async fn decimal_comparisons() {
        let out = run(r#"
import * as decimal from "std/decimal"
let a = decimal.from("1.0")
let b = decimal.from("2.0")
print(a < b)
print(a > b)
print(a <= decimal.from("1.0"))
print(a >= decimal.from("1.0"))
"#)
        .await;
        assert_eq!(out.trim(), "true\nfalse\ntrue\ntrue");
    }

    #[tokio::test]
    async fn decimal_division_by_zero_panics() {
        let err = run_err(
            r#"
import * as decimal from "std/decimal"
let _ = decimal.from(1) / decimal.from(0)
"#,
        )
        .await;
        assert!(
            err.message.contains("zero"),
            "expected 'zero' in: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn decimal_cross_type_eq_number() {
        // decimal.from("1.5") == 1.5  (cross-type Number eq) → true
        let out = run(r#"
import * as decimal from "std/decimal"
let d = decimal.from("1.5")
print(d == 1.5)
print(1.5 == d)
print(d != 1.5)
"#)
        .await;
        assert_eq!(out.trim(), "true\ntrue\nfalse");
    }

    #[tokio::test]
    async fn decimal_unary_minus() {
        // -decimal.from("2") == decimal -2
        let out = run(r#"
import * as decimal from "std/decimal"
let d = -decimal.from("2")
print(d)
print(d == decimal.from("-2"))
"#)
        .await;
        assert_eq!(out.trim(), "-2\ntrue");
    }

    #[tokio::test]
    async fn decimal_modulo() {
        let out = run(r#"
import * as decimal from "std/decimal"
let a = decimal.from("10")
let b = decimal.from("3")
print(a % b)
"#)
        .await;
        assert_eq!(out.trim(), "1");
    }

    #[tokio::test]
    async fn decimal_modulo_by_zero_panics() {
        let err = run_err(
            r#"
import * as decimal from "std/decimal"
let _ = decimal.from(1) % decimal.from(0)
"#,
        )
        .await;
        assert!(
            err.message.contains("zero"),
            "expected 'zero' in: {}",
            err.message
        );
    }

    /// Regression: normal number arithmetic must be unaffected.
    #[tokio::test]
    async fn regression_number_arithmetic_unaffected() {
        let out = run("print(2 + 3 == 5)").await;
        assert_eq!(out.trim(), "true");
    }

    /// Regression: string concatenation must be unaffected.
    #[tokio::test]
    async fn regression_string_concat_unaffected() {
        let out = run(r#"print("a" + "b" == "ab")"#).await;
        assert_eq!(out.trim(), "true");
    }

    #[tokio::test]
    async fn decimal_is_truthy_regardless_of_zero() {
        // spec §4: only nil and false are falsy — Decimal(0) is truthy.
        // Use `if (z)` since AScript requires parens around the condition.
        let out = run(r#"
import * as decimal from "std/decimal"
let z = decimal.from("0")
if (z) { print("truthy") } else { print("falsy") }
"#)
        .await;
        assert_eq!(out.trim(), "truthy");
    }

    #[tokio::test]
    async fn decimal_type_name() {
        // `type` is the AScript builtin for runtime type name (not `typeOf`).
        let out = run(r#"
import * as decimal from "std/decimal"
let d = decimal.from("1.5")
print(type(d))
"#)
        .await;
        assert_eq!(out.trim(), "decimal");
    }

    // ── 6d: json.parse(text, schema) ─────────────────────────────────────────

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn json_parse_with_schema_ok() {
        // json.parse(validJson, schema) → [value, nil]
        let src = r#"
import * as json from "std/json"
import * as schema from "std/schema"
let s = schema.object({name: schema.string(), age: schema.number()})
let [v, err] = json.parse("{\"name\":\"Ada\",\"age\":30}", s)
print(err == nil)
print(v.name)
print(v.age)
"#;
        let out = run(src).await;
        assert!(
            out.contains("true") && out.contains("Ada") && out.contains("30"),
            "got: {out}"
        );
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn json_parse_with_schema_bad_shape() {
        // json.parse(validJson but wrong shape, schema) → [nil, {path, message}]
        let src = r#"
import * as json from "std/json"
import * as schema from "std/schema"
let s = schema.object({id: schema.number()})
let [v, err] = json.parse("{\"id\":\"not-a-number\"}", s)
print(v == nil)
print(err != nil)
"#;
        let out = run(src).await;
        assert!(out.contains("true"), "got: {out}");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn json_parse_with_schema_malformed_json() {
        // json.parse(malformedJson, schema) → [nil, err]  (parse failure fused)
        let src = r#"
import * as json from "std/json"
import * as schema from "std/schema"
let s = schema.object({id: schema.number()})
let [v, err] = json.parse("{not json", s)
print(v == nil)
print(err != nil)
"#;
        let out = run(src).await;
        assert!(out.contains("true"), "got: {out}");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn json_parse_schema_regression_1arg() {
        // REGRESSION: json.parse(text) with no second arg still works.
        let src = r#"
import * as json from "std/json"
let [v, err] = json.parse("{\"x\":1}")
print(err == nil)
print(v.x)
"#;
        let out = run(src).await;
        assert!(out.contains("true") && out.contains('1'), "got: {out}");
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn json_parse_schema_regression_class() {
        // REGRESSION: json.parse(text, Class) still validates into the class.
        let src = r#"
import * as json from "std/json"
class P { id: number }
let [v, err] = json.parse("{\"id\":42}", P)
print(err == nil)
print(v.id)
"#;
        let out = run(src).await;
        assert!(out.contains("true") && out.contains("42"), "got: {out}");
    }

    // ── 6d: schema.fromClass ─────────────────────────────────────────────────

    #[tokio::test]
    async fn schema_from_class_ok() {
        // schema.fromClass(SomeClass) → derived schema that validates objects
        // matching the class's declared fields.
        let src = "import * as schema from \"std/schema\"\n\
                   class User {\n  id: number\n  name: string\n}\n\
                   let s = schema.fromClass(User)\n\
                   let [v, err] = schema.parse(s, {id: 1, name: \"Alice\"})\n\
                   print(err == nil)\n\
                   print(v.id)\n\
                   print(v.name)";
        let out = run(src).await;
        assert!(
            out.contains("true") && out.contains('1') && out.contains("Alice"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn schema_from_class_mismatch() {
        // A wrong-typed field via fromClass schema → [nil, errObj] (Tier-1).
        let src = "import * as schema from \"std/schema\"\n\
                   class User {\n  id: number\n  name: string\n}\n\
                   let s = schema.fromClass(User)\n\
                   let [v, err] = schema.parse(s, {id: \"oops\", name: \"Alice\"})\n\
                   print(v == nil)\n\
                   print(err != nil)";
        let out = run(src).await;
        assert!(out.contains("true"), "got: {out}");
    }

    #[tokio::test]
    async fn schema_from_class_nested_recurses() {
        // A nested class field (addr: Address) recurses into a nested object
        // schema — so a deep field is fully validated, not silently accepted.
        let prelude = "import * as schema from \"std/schema\"\n\
                       class Address {\n  city: string\n  zip: number\n}\n\
                       class User {\n  name: string\n  addr: Address\n}\n\
                       let s = schema.fromClass(User)\n";

        // 1. a fully-matching nested object → ok.
        let ok_src = format!(
            "{}let [v, err] = schema.parse(s, {{name: \"a\", addr: {{city: \"x\", zip: 1}}}})\n\
             print(err == nil)\nprint(v.addr.city)\nprint(v.addr.zip)",
            prelude
        );
        let out = run(&ok_src).await;
        assert!(
            out.contains("true") && out.contains('x') && out.contains('1'),
            "ok case got: {out}"
        );

        // 2. a wrong-typed DEEP field (addr.zip is a string) → Tier-1 err whose
        //    path points into the nested field and message mentions number.
        let bad_src = format!(
            "{}let [v, err] = schema.parse(s, {{name: \"a\", addr: {{city: \"x\", zip: \"bad\"}}}})\n\
             print(v == nil)\nprint(err.path)\nprint(err.message)",
            prelude
        );
        let out2 = run(&bad_src).await;
        assert!(out2.contains("true"), "deep mismatch not rejected: {out2}");
        assert!(
            out2.contains("addr.zip"),
            "err.path should point into nested field, got: {out2}"
        );
        assert!(
            out2.contains("number"),
            "err.message should mention number, got: {out2}"
        );

        // 3. a NON-OBJECT nested value (addr: 42) → rejected (must be an object),
        //    NOT silently accepted as `any`.
        let nonobj_src = format!(
            "{}let [v, err] = schema.parse(s, {{name: \"a\", addr: 42}})\n\
             print(v == nil)\nprint(err != nil)\nprint(err.path)",
            prelude
        );
        let out3 = run(&nonobj_src).await;
        assert!(
            out3.contains("true"),
            "non-object nested value should be rejected, got: {out3}"
        );
        assert!(
            out3.contains("addr"),
            "err.path should mention addr, got: {out3}"
        );
    }

    #[tokio::test]
    async fn schema_from_class_includes_inherited_fields() {
        // fromClass walks the superclass chain (merged_field_schema): a base-class
        // field is included in the derived schema and validated.
        let src = "import * as schema from \"std/schema\"\n\
                   class Animal {\n  legs: number\n}\n\
                   class Dog extends Animal {\n  name: string\n}\n\
                   let s = schema.fromClass(Dog)\n\
                   let [v, err] = schema.parse(s, {legs: 4, name: \"Rex\"})\n\
                   print(err == nil)\nprint(v.legs)\nprint(v.name)\n\
                   let [v2, err2] = schema.parse(s, {legs: \"four\", name: \"Rex\"})\n\
                   print(v2 == nil)\nprint(err2.path)";
        let out = run(src).await;
        assert!(
            out.contains("true") && out.contains('4') && out.contains("Rex"),
            "inherited-field ok case got: {out}"
        );
        // The inherited base field is also type-checked.
        assert!(
            out.contains("legs"),
            "inherited field mismatch should report path 'legs', got: {out}"
        );
    }

    // ── fluent schema method chaining (call-site hook) ────────────────────────

    /// A full fluent chain of refiners ending in `.parse(...)` — equivalent to
    /// the nested free-function form, and ok / minLength-fail / pattern-fail
    /// each routed through the same `call_schema` ops.
    #[cfg(feature = "data")] // pattern enforcement needs `data` (regex)
    #[tokio::test]
    async fn schema_fluent_chain_parse() {
        let src = r#"
import * as schema from "std/schema"
let s = schema.string().minLength(3).maxLength(12).pattern("^[a-z0-9_]+$")
let [v, err] = s.parse("ada_lovelace")
print(v)
print(err == nil)
let [v2, err2] = s.parse("ab")
print(v2 == nil)
print(err2.message)
let [v3, err3] = s.parse("Ada!")
print(v3 == nil)
print(err3.message)
"#;
        let out = run(src).await;
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "ada_lovelace", "ok value: {out}");
        assert_eq!(lines[1], "true", "ok err nil: {out}");
        assert_eq!(lines[2], "true", "minLength fail value nil: {out}");
        assert!(
            lines[3].contains("minLength"),
            "minLength fail message: {out}"
        );
        assert_eq!(lines[4], "true", "pattern fail value nil: {out}");
        assert!(lines[5].contains("pattern"), "pattern fail message: {out}");
    }

    /// `s.parse(v)` must equal `schema.parse(s, v)` (method == free function).
    #[tokio::test]
    async fn schema_fluent_parse_equals_free_fn() {
        let src = r#"
import * as schema from "std/schema"
let s = schema.number().min(1).max(10)
let [a, ae] = s.parse(5)
let [b, be] = schema.parse(s, 5)
print(a == b)
print(ae == nil)
print(be == nil)
let [c, ce] = s.parse(99)
let [d, de] = schema.parse(s, 99)
print(c == d)
print(ce.message == de.message)
"#;
        assert_eq!(run(src).await, "true\ntrue\ntrue\ntrue\ntrue\n");
    }

    /// A fluent-built schema can be used with the free-function `parse`.
    #[tokio::test]
    async fn schema_fluent_built_used_with_free_parse() {
        let src = r#"
import * as schema from "std/schema"
let [v, err] = schema.parse(schema.string().minLength(3), "ab")
print(v == nil)
print(err != nil)
"#;
        assert_eq!(run(src).await, "true\ntrue\n");
    }

    /// Re-refine / collision: `minLength(3).minLength(5)` — the second call
    /// routes to call_schema even though the field is already set, overwriting
    /// the constraint.
    #[tokio::test]
    async fn schema_fluent_re_refine_overwrites() {
        let src = r#"
import * as schema from "std/schema"
let s = schema.string().minLength(3).minLength(5)
let [v, err] = s.parse("abcd")
print(v == nil)
print(err != nil)
let [v2, err2] = s.parse("abcde")
print(v2)
print(err2 == nil)
"#;
        assert_eq!(run(src).await, "true\ntrue\nabcde\ntrue\n");
    }

    /// Bare member access still reads the STORED constraint field (not a method).
    #[tokio::test]
    async fn schema_fluent_bare_access_reads_field() {
        let src = r#"
import * as schema from "std/schema"
let s = schema.string().minLength(3)
print(s.minLength)
print(s.__kind)
"#;
        assert_eq!(run(src).await, "3\nstring\n");
    }

    /// `optional()` as a method wraps the receiver and accepts nil.
    #[tokio::test]
    async fn schema_fluent_optional_method() {
        let src = r#"
import * as schema from "std/schema"
let s = schema.number().optional()
let [v, err] = s.parse(nil)
print(v == nil)
print(err == nil)
let [v2, err2] = s.parse(42)
print(v2)
print(err2 == nil)
let [v3, err3] = s.parse("x")
print(v3 == nil)
print(err3 != nil)
"#;
        assert_eq!(run(src).await, "true\ntrue\n42\ntrue\ntrue\ntrue\n");
    }

    /// Object schema built via constructor, then `.parse(...)` as a method.
    #[tokio::test]
    async fn schema_fluent_object_parse_method() {
        let src = r#"
import * as schema from "std/schema"
let s = schema.object({a: schema.number(), b: schema.string()})
let [v, err] = s.parse({a: 1, b: "x"})
print(err == nil)
print(v.a)
print(v.b)
let [v2, err2] = s.parse({a: "no", b: "x"})
print(v2 == nil)
print(err2.path)
"#;
        assert_eq!(run(src).await, "true\n1\nx\ntrue\na\n");
    }

    // ── regression: the call-site hook must not change non-schema calls ───────

    /// Module call (`math.abs(x)`) — a Member callee on a module-namespace
    /// object whose fields are builtins. Must NOT be intercepted as a schema.
    #[tokio::test]
    async fn regression_module_call_after_hook() {
        let src = r#"
import * as math from "std/math"
print(math.abs(-5))
print(math.max(1, 2, 3))
"#;
        assert_eq!(run(src).await, "5\n3\n");
    }

    /// Instance method call still dispatches the bound method.
    #[tokio::test]
    async fn regression_instance_method_after_hook() {
        let src = r#"
class Counter {
  n: number
  fn init() { self.n = 0 }
  fn inc() { self.n = self.n + 1; return self.n }
}
let c = Counter()
print(c.inc())
print(c.inc())
"#;
        assert_eq!(run(src).await, "1\n2\n");
    }

    /// Plain object field-fn call `o.f()` still works (object is not a schema).
    #[tokio::test]
    async fn regression_object_field_fn_after_hook() {
        let src = r#"
let o = {f: () => 1, g: (x) => x + 1}
print(o.f())
print(o.g(41))
"#;
        assert_eq!(run(src).await, "1\n42\n");
    }

    /// An object that merely HAS a `__kind` field but it is NOT a known schema
    /// kind must fall through to the normal field-fn path, not be hijacked.
    #[tokio::test]
    async fn regression_object_with_bogus_kind_field() {
        let src = r#"
let o = {__kind: "widget", parse: (x) => x + 1}
print(o.parse(41))
"#;
        assert_eq!(run(src).await, "42\n");
    }

    /// schema constructor call `schema.string()` (a module-fn call) still works.
    #[tokio::test]
    async fn regression_schema_constructor_call() {
        let src = r#"
import * as schema from "std/schema"
let s = schema.string()
print(s.__kind)
"#;
        assert_eq!(run(src).await, "string\n");
    }

    /// Fallback evaluation ORDER: on a non-schema Member callee, `read_member`
    /// runs BEFORE the args. A member-read error (nil receiver) must preempt arg
    /// evaluation, so a side-effecting arg is NEVER evaluated, and the surfaced
    /// error is the member-read error — NOT an arg error.
    #[tokio::test]
    async fn regression_fallback_member_before_args() {
        let src = r#"
let calls = [0]
fn sideEffect() { calls[0] = calls[0] + 1; return 1 }
let n = nil
let [v, err] = recover(() => n.foo(sideEffect()))
print(v == nil)
print(err.message)
print(calls[0])   // 0 — the arg side effect must NOT have run
"#;
        let out = run(src).await;
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "true", "recover value nil: {out}");
        assert!(
            lines[1].contains("cannot read property 'foo' of nil"),
            "expected member-read error, got: {out}"
        );
        assert_eq!(
            lines[2], "0",
            "arg side effect must not run when member-read errors: {out}"
        );
    }

    /// Optional member call `o?.m()` still falls through the existing path.
    #[tokio::test]
    async fn regression_opt_member_call() {
        let src = r#"
let o = {m: () => 7}
print(o?.m())
let n = nil
print(n?.m())
"#;
        assert_eq!(run(src).await, "7\nnil\n");
    }
}
