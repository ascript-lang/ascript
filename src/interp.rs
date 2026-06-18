//! Async tree-walking evaluator. `eval_expr`/`exec` are async to establish
//! the event-loop seam from spec §7, even though the skeleton never suspends.

use crate::ast::{BinOp, Expr, ExprKind, Stmt, UnOp};
use crate::env::{AssignError, Environment};
use crate::error::AsError;
use crate::span::Span;
use crate::value::{OwnedKind, Value, ValueKind};
// RT §2.2: the legacy lexer/parser are the tree-walker front-end — gated OUT of the
// runtime-only build (the tree-walker is never an entry point in a stub). The `Interp`
// KERNEL stays; its source-loading methods refuse under `cfg(ascript_rt)`.
#[cfg(not(ascript_rt))]
use crate::{lexer, parser};
use async_recursion::async_recursion;
use std::cell::{Cell, RefCell};

// RT §2.2 — ELIDE paranoid-mode proof verification leans on the checker's `ElisionSet`
// (a front-end product). Non-rt uses the real type; the runtime-only build (no checker)
// uses a trivially-empty stand-in so the `Interp` kernel still compiles. Paranoid mode
// is set only from the gated source entry points, so on a stub the set is always empty
// and `maybe_paranoid_escalate` short-circuits — byte-identical behavior (never armed).
#[cfg(not(ascript_rt))]
type ParanoidElisionSet = crate::check::infer::elide::ElisionSet;
#[cfg(ascript_rt)]
#[derive(Default)]
pub(crate) struct ParanoidElisionSet;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The callable parts shared by plain functions and methods: parameter list,
/// optional return contract, and the body to execute.
struct BodySpec<'a> {
    params: &'a [crate::ast::Param],
    ret: &'a Option<crate::ast::Type>,
    body: &'a [Stmt],
    /// ELIDE §6.3 paranoid mode: the char-offset span of the fn's NAME token,
    /// for `fn_rets` paranoid lookup at the return-type check site. `None` for
    /// anonymous arrows / fn-expressions / methods (not in `fn_rets`).
    name_span: Option<crate::span::Span>,
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

/// DEFER §3.1: what kind of deferred call to run at body exit.
pub(crate) enum DeferKind {
    /// A plain function call: `defer f(args)` or `defer (() => …)(args)`.
    Call { callee: Value },
    /// A method call on an already-evaluated receiver: `defer o.m(args)`.
    /// Re-enters the member-call evaluator at drain time so schema/shared/workflow
    /// call-position hooks fire (spec §3.1 — pre-binding via `read_member` would
    /// silently skip them).
    Method { recv: Value, name: Rc<str> },
}

/// DEFER §3.1: one registered deferred call (args captured at statement time).
pub(crate) struct DeferEntry {
    pub kind: DeferKind,
    pub args: Vec<Value>,
    /// Whether this is `defer await f()` (§3.4).
    pub awaited: bool,
    pub span: Span,
}

/// DEFER §3.6: fold a panic raised by a deferred call into the in-flight frame
/// outcome. Rules (both engines share this helper — divergence is structurally
/// impossible):
/// - `Ok(_)` or `Propagate` → replaced by the new panic (r1/r2).
/// - `Panic(orig)` → the orig message is preserved, the new panic appended as a
///   suppressed note — exact format is LOCKED (r3).
/// - `Exit` → unreachable (the drain never runs under `Control::Exit`).
pub(crate) fn merge_defer_panic(pending: &mut Result<Value, Control>, new: AsError) {
    match pending {
        Ok(_) | Err(Control::Propagate(_)) => *pending = Err(Control::Panic(new)),
        Err(Control::Panic(orig)) => {
            orig.message = format!(
                "{} (suppressed panic in deferred call: {})",
                orig.message, new.message
            );
        }
        Err(Control::Exit(_)) => unreachable!("defer drain must not run under Exit"),
    }
}

/// RESIL §5: the current task's ambient locals (deadline, trace id). An immutable,
/// `Rc`-shared record: setting a value builds a NEW `Rc` (copy-on-write), so a child
/// task captured at spawn time is forever isolated from the parent's later scopes.
///
/// The fields are read by the `resilience`/`net`/`postgres`/`redis`/`sql` features
/// (deadline + trace readers); under a bare `--no-default-features` build none of
/// those are present, so allow dead_code there (mirrors the bare `#[allow(dead_code)]`
/// precedent elsewhere in this file for core-but-feature-consumed items).
#[allow(dead_code)]
pub(crate) struct TaskLocals {
    /// Absolute monotonic deadline (ms, the `clock_monotonic_ms` domain), if any.
    pub deadline_at_ms: Option<f64>,
    /// Ambient trace/request id, if any.
    pub trace_id: Option<Rc<str>>,
}

tokio::task_local! {
    /// RESIL §5.1: the CURRENT task's ambient locals (deadline budget, trace id). A
    /// tokio task-local (NOT a shared cell) so concurrent `spawn_local` tasks each
    /// have their OWN locals — a deadline set in one task can never leak into an
    /// unrelated concurrent task. It survives `.await` within a task and is isolated
    /// across tasks.
    ///
    /// **CORE, NOT feature-gated** (engine infrastructure, like [`SpanStatus`]): the
    /// seam compiles under `--no-default-features`. The cell holds an
    /// `Option<Rc<TaskLocals>>` (COPY-ON-WRITE — each `deadline`/`withTrace` builds a
    /// fresh `Rc`), seeded at every entry point (`ambient_root_scope`) and re-seeded
    /// at every user-code async-fn/method/static spawn site (capturing the spawning
    /// task's current `Rc`), so a child task spawned WHILE a deadline is set inherits
    /// it — but is isolated from the parent's LATER scope mutations.
    ///
    /// **Zero-cost when unset:** consulting it is `try_with(|c| c.get())` → `None`
    /// fast (a TLS lookup + `Cell` read; the `Rc` is cloned only on `Some`). No probe
    /// runs work on the `None` path.
    ///
    /// **NOT wrapped** at these spawn sites (per spec §5.1): `task_mod.rs`
    /// race-resolver tasks (only await existing futures, no user code); worker
    /// isolates (locals do NOT cross the airlock — a worker body starts with EMPTY
    /// locals, honest + documented); generators (`coro.rs` bodies are lazily polled
    /// INSIDE the resuming caller's task → `gen.next()` sees the resumer's current
    /// locals ambiently — resume-time semantics, correct for deadlines); http_server
    /// connection tasks (each request starts fresh by design); internal bridges
    /// (actor reply, postgres driver, etc. — no user code).
    pub(crate) static TASK_LOCALS: std::cell::Cell<Option<Rc<TaskLocals>>>;
}

/// RESIL §5.1: capture the current task's ambient locals (for propagation into a
/// spawned task). `None` if unset (e.g. no deadline) — one refcount bump on `Some`,
/// zero cost on `None`. Consulted at every user-code async spawn site.
pub(crate) fn task_locals_capture() -> Option<Rc<TaskLocals>> {
    TASK_LOCALS.try_with(|c| {
        // Clone the `Rc` out, then put it back (the `Cell` requires take-or-replace).
        let cur = c.take();
        let out = cur.clone();
        c.set(cur);
        out
    })
    .ok()
    .flatten()
}

/// RESIL §5.1: the current task's ambient locals, if any (a clone of the `Rc`).
/// Alias of [`task_locals_capture`] — named for the 4.3 reader call sites.
/// Only the deadline/trace readers (behind `resilience`/`net`/`postgres`/`redis`/
/// `sql`) call it; dead under a bare `--no-default-features` build.
#[allow(dead_code)]
pub(crate) fn task_locals_current() -> Option<Rc<TaskLocals>> {
    task_locals_capture()
}

/// RESIL §5.1: run `fut` within a fresh [`TASK_LOCALS`] scope seeded with `parent`
/// (the spawning task's current locals, captured at spawn time). Used at the
/// user-code async-fn/method/static spawn sites so the captured locals flow into the
/// spawned body.
pub(crate) async fn task_locals_scope<F, T>(parent: Option<Rc<TaskLocals>>, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    TASK_LOCALS.scope(std::cell::Cell::new(parent), fut).await
}

#[cfg(feature = "telemetry")]
tokio::task_local! {
    /// SP12: the CURRENT telemetry span context for THIS async task. A tokio
    /// task-local (NOT a shared cell) so concurrent `spawn_local` tasks each have
    /// their OWN current span — a span started in one task can never leak as the
    /// parent of an unrelated concurrent task (spec §9.3, the subtle correctness
    /// point). It survives `.await` within a task and is isolated across tasks.
    ///
    /// Seeded at each entry point and re-seeded at every async-fn/method spawn
    /// site (capturing the spawning task's current), so the captured lineage flows
    /// into the spawned body. `telemetry.span` mutates this cell around its
    /// callback (save → set → await → restore); `startSpan`/`telemetry_open_span`
    /// read it to choose a parent.
    pub(crate) static TELEMETRY_CURRENT: std::cell::Cell<Option<crate::stdlib::telemetry::model::SpanCtx>>;
}

/// SP12: run `fut` within a fresh [`TELEMETRY_CURRENT`] scope seeded with
/// `parent` (the spawning task's current span, captured at spawn time). Used at
/// the async-fn/method spawn sites and the entry points so every task has the
/// task-local in scope.
#[cfg(feature = "telemetry")]
pub(crate) async fn telemetry_scope<F, T>(
    parent: Option<crate::stdlib::telemetry::model::SpanCtx>,
    fut: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    TELEMETRY_CURRENT
        .scope(std::cell::Cell::new(parent), fut)
        .await
}

/// Root ambient-scope for the entry points (`run_file*`, `run_source*`, repl,
/// `run_tests`). Establishes BOTH the root [`TASK_LOCALS`] scope (ALWAYS — core, so
/// `try_with` never errs on the main task) AND, when the `telemetry` feature is on,
/// the root [`TELEMETRY_CURRENT`] scope (no parent). With telemetry OFF the telemetry
/// half is the identity, so non-telemetry builds pay only the (cheap) TASK_LOCALS
/// scope. RESIL §5.1 renamed this from `telemetry_root_scope`.
pub async fn ambient_root_scope<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    // RESIL §5.1: always establish the root TASK_LOCALS scope (core), so every entry
    // point has the cell in scope — `task_locals_capture()`'s `try_with` never errs.
    // The telemetry root scope nests inside it when that feature is on; with it off the
    // inner future is `fut` itself (no redundant async wrapper).
    #[cfg(feature = "telemetry")]
    let inner = telemetry_scope(None, fut);
    #[cfg(not(feature = "telemetry"))]
    let inner = fut;
    task_locals_scope(None, inner).await
}

/// Capture the current task's telemetry span context (for propagation into a
/// spawned task). `None` if telemetry is off or no span is current.
#[cfg(feature = "telemetry")]
pub(crate) fn telemetry_capture_current() -> Option<crate::stdlib::telemetry::model::SpanCtx> {
    TELEMETRY_CURRENT
        .try_with(|c| c.get())
        .ok()
        .flatten()
}

/// A span outcome, used by the SP12 `std/telemetry` soft hook (the contract
/// `std/ai` calls). Defined in this CORE module — **not** behind the `telemetry`
/// feature — so `std/ai` can name it whether or not the `telemetry` feature is
/// compiled in (the hook methods on [`Interp`] keep always-present signatures and
/// bridge to the telemetry pipeline only when the feature is on). Maps 1:1 onto
/// OTLP status codes (`Unset`/`Ok`/`Error`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpanStatus {
    #[default]
    Unset,
    Ok,
    Error,
}

/// A fresh global environment with the built-in functions installed.
/// The bare (unqualified) builtin names installed in every program's global env.
/// Shared with the checker (`undefined-variable`) so they cannot drift.
pub const BUILTIN_NAMES: &[&str] = &[
    "print", "Ok", "Err", "assert", "recover", "test", "len", "type", "range", "exit", "int",
    "float", "run_in_worker",
];

pub fn global_env() -> Environment {
    let env = Environment::global();
    for &name in BUILTIN_NAMES {
        env.define(name, Value::builtin(name), false)
            .expect("global env starts empty");
    }
    env
}

/// Build a `[value, err]` Result pair.
// pub(crate): used by std/* modules (std/convert) later in M10.
pub(crate) fn make_pair(value: Value, err: Value) -> Value {
    Value::array(vec![value, err])
}

/// Build an error object `{ message: <msg> }`.
// pub(crate): used by std/* modules (std/convert) later in M10.
pub(crate) fn make_error(msg: Value) -> Value {
    let mut map = indexmap::IndexMap::new();
    map.insert("message".to_string(), msg);
    Value::object(map)
}

/// RESIL §5.4: the canonical `[nil, {message, code:"deadline-exceeded"}]` Tier-1
/// err pair returned when a deadline budget is exhausted. ONE construction site so
/// the shape is byte-identical across the deadline race ([`crate::stdlib::resilience`]),
/// the limiter/bulkhead budget-aware parks, and the deadline-aware I/O consult sites
/// (`std/http` / `std/postgres` / `std/redis` / `std/sqlite`). Field order is
/// `message` then `code` (matching `make_error` + an explicit `code`). This lives in
/// core (not behind the `resilience` feature) so the I/O consult sites can build it
/// even when `resilience` is compiled out but the I/O module is enabled.
#[cfg_attr(
    not(any(
        feature = "resilience",
        feature = "net",
        feature = "postgres",
        feature = "redis",
        feature = "sql"
    )),
    allow(dead_code)
)]
pub(crate) fn deadline_exceeded_pair() -> Value {
    let mut err = indexmap::IndexMap::new();
    err.insert("message".to_string(), Value::str("deadline exceeded"));
    err.insert("code".to_string(), Value::str("deadline-exceeded"));
    make_pair(Value::nil(), Value::object(err))
}

/// RESIL §3.1.3: test whether `v` is a Result pair `[_, err]` with `err != nil`.
///
/// Returns `Some(err)` if `v` is a 2-element array whose second element is
/// non-nil; `None` otherwise (plain value, ok-pair `[v, nil]`, or not an array).
/// Recognizes a `[value, err]` ERROR pair: a 2-element array whose second element is
/// non-nil, returning that error. This is the SAME error-pair *shape* the `?` operator
/// (`ExprKind::Try`) recognizes (2-element array, non-nil `[1]`), but it is a deliberately
/// LENIENT predicate distinct from `Try`'s inline logic: `Try` ERRORS on a non-pair (the `?`
/// operator requires a Result pair), whereas this returns `None` on a non-pair or a `[v, nil]`
/// — because for outcome classification (the circuit breaker, §3.1.3) a non-pair or success
/// pair is a SUCCESS, not a failure. The two stay behaviorally consistent on the error-pair
/// case by the four-mode differential; they are not literally the same function (Try's
/// non-pair-is-error rule must not leak into outcome classification).
pub(crate) fn result_pair_err(v: &Value) -> Option<Value> {
    match v.kind() {
        crate::value::ValueKind::Array(a) => {
            let b = a.borrow();
            if b.len() == 2 && b[1] != Value::nil() {
                Some(b[1].clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[derive(Clone)]
pub struct ModuleEntry {
    pub env: Environment,
    pub exports: Rc<RefCell<HashSet<String>>>,
}

/// The non-`Clone` OS resource behind a `Value::native` handle. Real variants are
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
    // SP5 §6 std/postgres: a tokio-postgres async connection. Holds the `Client`
    // plus the `AbortHandle` of the spawned driver task (the `Connection` future
    // that drives the protocol). Dropping/closing the resource aborts the driver
    // task — deterministic teardown, matching the cancel-on-drop discipline. The
    // Client's query/execute take `&self`, but we still take_resource it out across
    // the await to avoid holding a `resources` borrow.
    #[cfg(feature = "postgres")]
    PostgresConnection {
        client: tokio_postgres::Client,
        conn_task: tokio::task::AbortHandle,
    },
    // SP5 §6 std/redis: a multiplexed async connection. Its command methods take
    // `&mut self`; taken out across the await per the borrow discipline. Boxed to
    // keep the enum compact (MultiplexedConnection is sizeable).
    #[cfg(feature = "redis")]
    RedisConnection(Box<redis::aio::MultiplexedConnection>),
    // SP5 §7 std/lru: a bounded LRU cache (core, not feature-gated). Boxed to keep
    // the enum compact (the IndexMap can grow).
    Lru(Box<crate::stdlib::lru::LruState>),
    // SP5 §7 std/events: an event-emitter's per-event listener lists (core).
    Events(Box<crate::stdlib::events::EventsState>),
    // SP12 std/telemetry: an in-progress span (ids, name, start ns, attrs, events,
    // status). On `end()` it is finalized into a `SpanRecord` and buffered for
    // export, then the resource is removed. Boxed to keep the enum compact.
    #[cfg(feature = "telemetry")]
    TelemetrySpan(Box<crate::stdlib::telemetry::model::OpenSpan>),
    // SP11 std/ai: a live streaming chat (`ai.stream`). Holds the genai
    // `ChatStream` (a `Stream` of events) plus the running terminal aggregate
    // (`stream.result()`). `next()`/`textOnly()` poll one event per call via the
    // take-out-across-await pattern; `result()` returns the aggregate. Boxed to keep
    // the `ResourceState` enum compact. Feature `ai`.
    #[cfg(feature = "ai")]
    AiStream(Box<crate::stdlib::ai::AiStreamState>),
    /// Workers Spec B §Task 5: a `worker class` ACTOR proxy. Holds the outbound
    /// `Send` mailbox sender + the dedicated `IsolateHandle` (whose `Drop` tears the
    /// isolate down) + the declared class name. The actor instance + any native
    /// resources it opens live IN the isolate, never here. Boxed for
    /// `large_enum_variant`.
    ///
    /// GC INVARIANT: this is a native handle holding `Send` channels + a thread
    /// handle — NOT script `Value`s. The GC must NEVER trace into it. `Value::native`
    /// already traces as a no-op (`gc.rs`'s `_ => {}` arm), so the invariant holds.
    WorkerActor(Box<crate::worker::actor::WorkerActorHandle>),
    /// FFI campaign §3.4: an open shared library (`ffi.open` → `dlopen`). The
    /// `libloading::Library` `dlclose`s on `Drop` (deterministic reclaim, matching the
    /// native-resource discipline). Wrapped in an `Rc` so a `ForeignSymbol` can keep
    /// the owning `Library` alive past the lib handle's own drop (a borrowed
    /// `Symbol<'lib>` cannot be `'static`; the raw-address-plus-kept-alive-`Library`
    /// pairing gives both `'static` storage and lifetime correctness — §3.4).
    ///
    /// GC INVARIANT: a `Library` is an opaque OS handle; the GC must NEVER trace into
    /// it. `Value::native` traces as a no-op (`gc.rs`'s `_ => {}`), so the invariant
    /// holds for `ForeignLib`/`ForeignSymbol`/`ForeignPtr` automatically.
    #[cfg(feature = "ffi")]
    ForeignLib(std::rc::Rc<libloading::Library>),
    /// FFI campaign §3.4: a resolved symbol + its bound signature. Stores the function
    /// address as a raw pointer (in `FfiSymbol`) and KEEPS THE OWNING `Library` ALIVE
    /// via an `Rc` clone, so the address stays valid for every `sym.call`. Boxed to
    /// keep the enum compact. GC-UNTRACED (raw pointer + opaque handle).
    #[cfg(feature = "ffi")]
    ForeignSymbol(Box<crate::stdlib::ffi::FfiSymbol>),
    /// FFI campaign §3.4: an opaque C pointer (a `malloc` result, a C "constructor"
    /// handle) carried as a raw `usize` address. NOT auto-freed (the C library owns the
    /// lifetime). GC-UNTRACED.
    #[cfg(feature = "ffi")]
    ForeignPtr(usize),
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

/// A single resolved third-party package: its loadable root directory and the
/// absolute entry module to bind for a bare `import "<name>"` (SP6 §6).
///
/// This is a DEPENDENCY-FREE plain-`std` type living in the interpreter core so a
/// bare-specifier import resolves identically on both engines. The CLI's `pkg`
/// feature (manifest/lock/MVS/fetch) builds the [`PackageMap`] and installs it via
/// [`Interp::set_package_resolver`]; the core never grows a network/git/toml
/// dependency. Under `--no-default-features` (no `pkg`) the map is simply always
/// empty, so a bare specifier yields the clean "unknown package" error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPkg {
    /// The package's root directory (a content-addressed `store/<hash>/` dir for a
    /// git/url dep, or the local dir for a path dep). Package-internal `./`
    /// imports resolve within this root via the existing file-module loader.
    pub root: PathBuf,
    /// The absolute entry module bound for a bare `import "<name>"` (no subpath).
    pub entry: PathBuf,
}

/// The CLI-injected resolved dependency set: package key → resolved package. The
/// key is the first path segment (`http` for `import "http/router"`) or the
/// `@scope/name` pair for a scoped package.
pub type PackageMap = HashMap<String, ResolvedPkg>;

/// The three-way classification of an `import` specifier, shared BYTE-IDENTICALLY
/// by both engines (the tree-walker `Stmt::Import` arm and the VM `Op::Import`
/// exec) via [`Interp::classify_specifier`] (SP6 §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecifierKind {
    /// `std/*` — a built-in stdlib module (unchanged; never touches the FS).
    Std,
    /// A relative/absolute file module (`./m`, `../m`, `/abs/m`) — unchanged; the
    /// resolved path is joined against the importer's dir by the existing loader.
    Relative(PathBuf),
    /// A bare package specifier whose first segment resolved in the package map.
    /// `target` is the absolute file the existing loader should load: the
    /// package's `entry` (no subpath) or `root.join(subpath)` with a default
    /// `.as` extension.
    Package { key: String, target: PathBuf },
    /// A bare package specifier whose first segment is NOT in the resolved set →
    /// a Tier-2 "unknown package" error (identical message on both engines).
    UnknownPackage(String),
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
    /// Live OS resources backing `Value::native` handles, keyed by handle id.
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
    /// `Value::closure` callback (see [`Interp::call_value`]'s `Closure` arm).
    /// `None` (an empty `Weak`) on a pure tree-walker run where no VM exists; a
    /// `Value::closure` can only be produced by the VM, so a VM is always
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
    /// Current LOGICAL CALL recursion depth (SP3 §B). Incremented EXACTLY ONCE per
    /// logical call — on the tree-walker in `run_body` (the single call funnel), on
    /// the VM at each `CallFrame` push / native re-entry — and decremented on return
    /// / unwind. Shared by BOTH engines (the VM holds an `Rc<Interp>`), so a program
    /// of logical call-depth D trips [`MAX_CALL_DEPTH`] at the SAME D on both →
    /// byte-identical at/over the limit. It does NOT count expression nesting (that
    /// is the separate [`Interp::expr_depth`] dimension) — counting expr nesting here
    /// would double-count each call on the tree-walker (the call sub-expression's
    /// `eval_expr` frames are live alongside the `run_body` frame) and make it trip
    /// at ~MAX/2 while the VM trips at MAX. A `Cell` (never a `RefCell`) so it is
    /// never held across an `.await` (`await_holding_refcell_ref` stays satisfied).
    call_depth: Cell<u32>,
    /// Current EXPRESSION-NESTING depth (SP3 §B / O1) — a SEPARATE dimension from
    /// [`Interp::call_depth`]. A pathologically nested expression (`((((…))))`, a
    /// huge binary chain) overflows the recursive evaluator with NO calls involved;
    /// the tree-walker bounds it here in `eval_expr` and the VM bounds the mirror in
    /// `compile_expr`, both at [`EXPR_NEST_LIMIT`]. Keeping it off `call_depth`
    /// prevents expression nesting from contaminating the per-call count. A `Cell`,
    /// never held across an `.await`.
    expr_depth: Cell<u32>,
    /// std/log minimum level (records below it are dropped). Default `Info`.
    #[cfg(feature = "log")]
    log_level: std::cell::Cell<LogLevel>,
    /// std/log output format. Default `Human`.
    #[cfg(feature = "log")]
    log_format: std::cell::Cell<LogFormat>,
    /// std/log capture buffer (used under `OutputSink::Capture`, i.e. tests).
    #[cfg(feature = "log")]
    log_capture: RefCell<String>,
    /// SP12 std/telemetry pipeline. `None` = uninitialized = every telemetry call
    /// is a no-op (the "safe to leave in production" promise). Set by
    /// `telemetry.init`, cleared by `telemetry.shutdown`. State lives behind this
    /// `RefCell` so the `&self` `call_telemetry` path can mutate buffered signals
    /// through short-lived borrows (never held across an `.await`).
    #[cfg(feature = "telemetry")]
    telemetry: RefCell<Option<crate::stdlib::telemetry::model::TelemetryState>>,
    /// SP12 std/telemetry capture sink: the outbound HTTP requests the exporters
    /// WOULD have sent, recorded under `OutputSink::Capture` (tests) instead of
    /// opening a socket. Tests read it via [`Interp::telemetry_capture`].
    #[cfg(feature = "telemetry")]
    telemetry_capture: RefCell<Vec<crate::stdlib::telemetry::model::CapturedRequest>>,
    /// RESIL §3.6/§6.1: per-isolate singleflight table and metrics registry.
    /// `None` fields are absent in a non-resilience build; `Default` gives zero
    /// cost (empty IndexMap + empty registry). Tasks 3.2/3.3/5.x consume this;
    /// Task 3.1 (this field) is the single Interp touch so later tasks are
    /// pure-stdlib changes. Never held across `.await`; accessed through
    /// short-lived borrows only.
    // Read starting in Task 3.2 (singleflight uses `.flights`) and Phase 5 (`.registry`);
    // pre-declared here as the single Interp touch (spec §3.6/§6.1).
    #[cfg(feature = "resilience")]
    #[allow(dead_code)]
    pub(crate) resilience: RefCell<crate::stdlib::resilience::ResilState>,
    /// The script's own CLI arguments (`ascript run file.as <args...>`).
    /// Excludes the binary name and the script path — only the trailing args.
    /// Empty unless set by [`Interp::set_cli_args`] (i.e. the REPL and test
    /// runner always see `[]`, which is correct).
    cli_args: RefCell<Vec<Rc<str>>>,
    /// SP11 std/ai state: the lazily-built genai `Client` (one per `Interp`, with
    /// our pooled reqwest client injected) and an optional fixture-replay seam used
    /// by tests to serve recorded JSON/SSE bodies with no socket/secret. `None`
    /// until the first `ai.*` call materializes it. State lives behind this
    /// `RefCell` so the `&self` `call_ai` path can take the client OUT across each
    /// `.await` (take-out-across-await), never holding a borrow over a genai await.
    #[cfg(feature = "ai")]
    ai: RefCell<crate::stdlib::ai::AiClient>,
    /// SP9 §3 determinism context. `None` (default) = INERT: the clock/RNG/sleep
    /// seams take their existing real paths and behavior is byte-identical to
    /// pre-SP9. `Some(..)` = deterministic mode (entered by `workflow.run`/`resume`):
    /// the seams route through the [`crate::det::DeterminismContext`]'s virtual
    /// clock, seeded RNG, and recorded event stream. State lives behind this
    /// `RefCell` so the `&self` seam accessors can read/advance it through
    /// short-lived borrows, NEVER held across an `.await` (the accessors take the
    /// value out / drop the borrow before returning, like the `resources`
    /// discipline).
    determinism: RefCell<Option<crate::det::DeterminismContext>>,
    /// SP6 §6: the CLI-injected resolved third-party package set. `None` until
    /// [`Interp::set_package_resolver`] installs it (the REPL / tests / a project
    /// with no deps leave it `None` → every bare specifier is "unknown package").
    /// A DEPENDENCY-FREE plain map (no network/git/toml types), so the core
    /// compiles under `--no-default-features` with the map simply always absent.
    /// Read through a short-lived borrow that is dropped BEFORE the loader
    /// `.await` (the resolved target is cloned out first — never hold this borrow
    /// across an await; `await_holding_refcell_ref` stays satisfied).
    package_resolver: RefCell<Option<PackageMap>>,
    /// Workers Spec A: the entry program's full source text, retained so a
    /// `worker fn` dispatch can (re)compile it to a top-level [`crate::vm::chunk::Chunk`]
    /// and build the shippable code slice (entry fn + transitive top-level deps).
    /// Shared by BOTH engines — the tree-walker has no compiled chunk of its own, so
    /// this is how it produces an identical `.aso` slice that the isolate's VM runs.
    /// `None` until [`Interp::set_worker_source`] is called by the run entry point; a
    /// `worker fn` call with no source set raises a clear recoverable panic.
    /// IFACE §5.3: the per-isolate `instanceof Interface` verdict cache. Memoizes the
    /// structural `conforms` result keyed by `(Rc::as_ptr(class) as usize,
    /// Rc::as_ptr(iface) as usize)`. Stores `usize` keys + `(generation, bool)` ONLY — no
    /// `Value`, no `Rc`, no `Cc` — so it holds nothing alive and the GC never traces it; a
    /// pure memo (same answer hot or cold), active in BOTH specialized and generic VM modes.
    ///
    /// **Pointer-reuse soundness (the generation guard).** Class/interface descriptors are
    /// NOT load-time-immortal: a `class`/`interface` declared inside a fn body or loop mints
    /// a fresh `Rc` per execution that can drop to refcount 0 when its scope dies, after
    /// which the allocator may hand the SAME address to a later, DIFFERENT descriptor. A raw
    /// pointer key would then alias a stale verdict. So each entry records the
    /// `iface_cache_gen` it was computed under, and `iface_cache_gen` is bumped on every
    /// runtime class/interface DEFINITION (tree-walker `exec` + VM `Op::Class`/
    /// `Op::DefineInterface`). A reused address necessarily carries an older generation than
    /// the define that created the new descriptor, so its entry is treated as a miss and
    /// recomputed. In the common case (all descriptors defined at load, before any
    /// `instanceof`) the generation stabilizes early and every verdict stays cached.
    iface_verdict_cache: RefCell<HashMap<(usize, usize), (u64, bool)>>,
    /// Monotonic generation bumped on every runtime class/interface definition; guards
    /// `iface_verdict_cache` against pointer reuse (see its doc). Pure-memo invalidation, so
    /// it never changes observable behavior — only whether a verdict is recomputed.
    iface_cache_gen: std::cell::Cell<u64>,
    /// FFI §4.3: the per-isolate capability set (default = ALL granted, so the
    /// central dispatch gate is a no-op short-circuit and every existing program
    /// is byte-identical). `RefCell` only because the irreversible `caps.drop`
    /// mutates it; the dispatch gate reads a `Copy` [`crate::stdlib::caps::CapBits`]
    /// **snapshot**, NEVER holding this borrow across an `.await`
    /// (`await_holding_refcell_ref` stays satisfied).
    caps: RefCell<crate::stdlib::caps::CapSet>,
    /// FFI §4.5a: whether `caps.drop`/`caps.dropAll` are permitted on THIS isolate.
    /// `true` on the top-level program isolate and a dedicated `run_in_worker`
    /// isolate (where a drop is durable, single-tenant). A **pooled** `worker fn`
    /// REUSES one `Interp` across requests, so a drop there would leak forward into
    /// unrelated work — the pooled-request install clears this flag (Task 8) and
    /// `caps.drop` is then refused with a loud recoverable panic (§4.5a). A `Cell`
    /// (never held across `.await`).
    caps_drop_allowed: Cell<bool>,
    worker_source: RefCell<Option<Rc<str>>>,
    /// Workers Spec A (.aso path): the raw `.aso` bytes of the entry program, retained
    /// so a `worker fn` dispatch can re-parse them into a top-level
    /// [`crate::vm::chunk::Chunk`] and build its shippable code slice directly (via
    /// `build_code_slice`) without recompiling from source. Set by `run_aso_file` (which
    /// has no source); `worker_source` takes priority when BOTH are set (a run-from-source
    /// always uses the source path). `None` in every run mode that sets `worker_source`.
    worker_aso_bytes: RefCell<Option<Rc<[u8]>>>,
    /// SELF-CONTAINED-BUNDLES Task 1.6: the encoded [`crate::vm::archive::ModuleArchive`]
    /// bytes of a BUNDLED multi-module program, retained so every worker isolate can decode
    /// and install the archive on its own fresh `Vm` BEFORE re-running the program's
    /// top-level imports. A worker isolate re-runs those imports; without the archive a
    /// re-run `Op::Import` of an imported module finds no archive and no source tree. Plain
    /// `Send` bytes (an `Rc<[u8]>` here; `.to_vec()`d at each dispatch site to cross the
    /// airlock). `None` for an ordinary unbundled program (no archive) gives zero behavior
    /// change. Mirrors [`worker_aso_bytes`].
    worker_archive_bytes: RefCell<Option<Rc<[u8]>>>,
    /// DX D2 Task 8: snapshot "update mode" (the `--update-snapshots` re-baseline).
    /// When `true`, `assert.snapshot` OVERWRITES the stored snapshot with the freshly
    /// serialized value and PASSES (a `jest -u`-style bulk re-baseline without editing
    /// source), and orphan snapshot files are DELETED after the run. Default `false`
    /// (a normal run never writes a changed snapshot and never deletes). A `Cell`,
    /// never held across an `.await`.
    snapshot_update: Cell<bool>,
    /// DX D2 Task 8: the set of snapshot FILES (canonical-ish `.snap` paths) that an
    /// `assert.snapshot` call actually TOUCHED this run. After a full run, a `.snap`
    /// file in a touched `__snapshots__/` dir that is NOT in this set is an ORPHAN
    /// (its assertion was removed) — reported, and removed under `--update-snapshots`.
    /// A `BTreeSet` for deterministic (sorted) orphan reporting. Never held across an
    /// `.await`. Only read on a `sys`+`data` build (the `assert.snapshot` handler and
    /// the orphan scan are feature-gated); the field is unconditional so the runner can
    /// always call `set_snapshot_update`.
    #[allow(dead_code)]
    snapshots_touched: RefCell<std::collections::BTreeSet<PathBuf>>,
    /// ELIDE §4.3 Task 3.2: when `true`, the tree-walker module loader runs
    /// `elision_proofs` on each module's source and calls `mark_program` before
    /// execution. Defaults to `false` — zero behavior change in normal runs.
    /// Task 4.1 gates the default-on decision (measured §5.1 budget); for THIS
    /// task the flag is exposed via `set_elide_mode` so tests can turn it on
    /// without changing any existing run path. A `Cell` (never held across `.await`).
    pub(crate) elide_mode: Cell<bool>,
    /// ELIDE §6.3 paranoid mode: when `Some(set)`, any contract failure at a site
    /// whose span appears in `set` escalates to an "ELIDE proof violated" panic
    /// (indicating a checker soundness bug). The set is built by merging the
    /// per-module [`ElisionSet`]s collected during the proof phase. Consulted ONLY
    /// on the contract-failure paths — ZERO hot-path cost (the lookup happens only
    /// when a check is ALREADY failing, i.e. about to panic anyway). A `RefCell`
    /// (never held across `.await` since all lookup sites are sync). `None` when
    /// paranoid mode is off (the default) — the check short-circuits immediately.
    pub(crate) paranoid_set: RefCell<Option<ParanoidElisionSet>>,
}

/// Above this many in-flight async tasks, an async-fn call cooperatively yields
/// after spawning so the executor can reap finished/cancelled tasks. Keeps a
/// no-await loop of un-awaited async calls bounded instead of growing to N.
const INFLIGHT_YIELD_CAP: u64 = 256;

/// Split a bare import specifier into `(package_key, subpath)` (SP6 §6). The key
/// is the first path segment, or the `@scope/name` pair for a scoped package; the
/// subpath is everything after it (empty if none). E.g.
/// `"http"` → `("http", "")`, `"http/router"` → `("http", "router")`,
/// `"@acme/schema"` → `("@acme/schema", "")`,
/// `"@acme/schema/sub"` → `("@acme/schema", "sub")`.
pub(crate) fn split_package_key(source: &str) -> (String, String) {
    if let Some(rest) = source.strip_prefix('@') {
        // Scoped: the key is the first TWO segments (`@scope/name`).
        let mut parts = rest.splitn(3, '/');
        let scope = parts.next().unwrap_or("");
        let name = parts.next().unwrap_or("");
        let subpath = parts.next().unwrap_or("");
        let key = format!("@{scope}/{name}");
        (key, subpath.to_string())
    } else {
        match source.split_once('/') {
            Some((first, rest)) => (first.to_string(), rest.to_string()),
            None => (source.to_string(), String::new()),
        }
    }
}

/// The real wall clock in ms since the Unix epoch (the value `time.now`/`date.now`
/// return when NOT in deterministic mode). Shared so the determinism seam and the
/// stdlib seams agree on the format. Saturating to 0 on a pre-epoch clock.
pub(crate) fn real_now_ms() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// The maximum LOGICAL call/eval recursion depth (SP3 §B). Exceeding it raises a
/// clean, catchable Tier-2 panic `maximum recursion depth exceeded` BEFORE the
/// native stack overflows (no SIGABRT). It is a single source of truth shared by
/// both engines (the VM reaches it via its `Rc<Interp>`), so they trip at the SAME
/// logical depth → byte-identical at/over the limit.
///
/// The number is pinned EMPIRICALLY (SP3 §B6): the tree-walker (the largest
/// per-frame budget of either engine — its `#[async_recursion]` futures are huge)
/// overflows the stock 8 MB main-thread stack at ~99 logical frames in a DEBUG
/// build (~82 KB/frame) and ~810 in release (~10 KB/frame). To let 3000 frames sit
/// comfortably UNDER native capacity the entry points run the program on a worker
/// thread with an enlarged [`WORKER_STACK_SIZE`] stack: 3000 × ~82 KB ≈ 246 MB in
/// the debug worst case, so a 512 MB worker stack gives > 2× headroom. Truly
/// unbounded recursion stays the SP9 architectural non-goal (needs an
/// explicit-stack VM); SP3 turns the crash into a deterministic, catchable error.
///
/// **SP9 §1 coordination (robust recursion):** SP9 inserts `stacker::maybe_grow`
/// guards at the native re-entry funnels (VM `call_value`/method dispatch, generator
/// `resume_vm`, both parsers, the resolver, `compile_expr`, and the tree-walker
/// `eval_expr`/`run_body` — see `src/vm/stack.rs`). Those guards grow the native
/// stack on demand so the narrow native-re-entry paths (deep `map`/reduce callbacks,
/// nested generator composition, deeply nested expressions) REACH this logical cap
/// cleanly instead of SIGABRTing the native stack first — even off the enlarged
/// worker thread. This cap stays the product default and the safety backstop (it
/// also bounds heap-`frames` growth, which `stacker` does not); SP9 chose the
/// "always-on stacker, cap stays the ceiling" option (spec §1.6), so the value is
/// unchanged and both engines still trip at the SAME `MAX_CALL_DEPTH`.
pub const MAX_CALL_DEPTH: u32 = 3000;

/// The maximum EXPRESSION-NESTING depth (SP3 §B / O1) — a SEPARATE limit from
/// [`MAX_CALL_DEPTH`], on the [`Interp::expr_depth`] counter. Bounds a
/// pathologically nested expression (`((((…))))`, a giant binary chain) on the
/// tree-walker (`eval_expr`) and its mirror on the VM (`compile_expr`) so neither
/// SIGABRTs; over it → the SAME `maximum recursion depth exceeded` Tier-2 panic, so
/// both engines error byte-identically (stdout + exit). Kept equal to
/// `MAX_CALL_DEPTH`: a single uniform "logical recursion" ceiling, just split across
/// two non-interfering counters so expression nesting never inflates the per-call
/// count. The expression evaluator's per-nesting native frame is far smaller than a
/// `run_body` call frame, so this comfortably fits the [`WORKER_STACK_SIZE`] stack.
pub const EXPR_NEST_LIMIT: u32 = MAX_CALL_DEPTH;

/// The worker-thread stack size the entry points install (SP3 §B6) so
/// [`MAX_CALL_DEPTH`] logical frames fit under native capacity with > 2× margin
/// even for the tree-walker's large debug-build frames. Sized off the empirical
/// ~82 KB-per-LOGICAL-CALL debug measurement (3000 × 82 KB ≈ 246 MB → 512 MB ≈
/// 2.08×). A thread stack is virtual address space — only touched pages are
/// committed — so a normal shallow program pays nothing.
pub const WORKER_STACK_SIZE: usize = 512 << 20;

/// RAII guard bounding a recursion counter (SP3 §B). [`DepthGuard::enter`]
/// increments the given counter and returns `Err(Control::Panic)` if the new value
/// exceeds `limit`; [`Drop`] decrements, so the count unwinds correctly through a
/// `?`-early-return OR a panic. Used for BOTH the per-call counter
/// (`call_depth`/`MAX_CALL_DEPTH`, incremented ONCE per logical call) and the
/// separate expression-nesting counter (`expr_depth`/`EXPR_NEST_LIMIT`). The panic
/// message is the same `maximum recursion depth exceeded` for both, so the two
/// dimensions are observationally identical at the boundary.
pub(crate) struct DepthGuard<'a> {
    depth: &'a Cell<u32>,
}

impl<'a> DepthGuard<'a> {
    /// Enter one recursion level on `depth`, capped at `limit`, anchored at `span`.
    /// On overflow the counter is NOT incremented (so `Drop` does not under-count)
    /// and a clean Tier-2 panic is returned.
    pub(crate) fn enter(depth: &'a Cell<u32>, limit: u32, span: Span) -> Result<Self, Control> {
        let next = depth.get() + 1;
        if next > limit {
            return Err(Control::Panic(AsError::at(
                "maximum recursion depth exceeded",
                span,
            )));
        }
        depth.set(next);
        Ok(DepthGuard { depth })
    }
}

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        // `saturating_sub` (not `- 1`) so this can never underflow-panic in a
        // destructor. A GENERATOR body parks at `yield` with its `run_body`/
        // `eval_expr` guards still live on the SUSPENDED future's stack; the
        // generator driver snapshot-restores the counters around each poll
        // (`coro.rs`), so the main-stack accounting stays exact, but when the
        // suspended future is finally DROPPED (`gen.close()` / abandonment) those
        // held guards decrement against the restored (possibly already-zero)
        // counter — `saturating_sub` makes that a no-op instead of a panic.
        self.depth.set(self.depth.get().saturating_sub(1));
    }
}

/// SP3 §B: a depth guard for the VM's RE-ENTRANT `Vm::run` boundaries
/// (`invoke_compiled_method` / `call_value`). Unlike [`DepthGuard`] (which
/// decrements by exactly one), this SNAPSHOTS the counter on entry (+1 for the
/// re-entry's own logical frame, with the limit check) and on `Drop` RESTORES the
/// counter to the snapshot — absorbing ANY imbalance left by the nested run,
/// including frames abandoned when a `Control::Panic` unwinds `Vm::run` (so a
/// `recover`-caught recursion panic leaves the depth exactly where it was before
/// the recovered call, matching the tree-walker's RAII unwind). The VM's per-frame
/// increment/decrement (`enter_frame_depth`/`leave_frame_depth`) balances on the
/// NORMAL path; this restore is the safety net for the panic-unwind path.
pub(crate) struct DepthRestore<'a> {
    depth: &'a Cell<u32>,
    saved: u32,
}

impl<'a> DepthRestore<'a> {
    pub(crate) fn enter(depth: &'a Cell<u32>, span: Span) -> Result<Self, Control> {
        let saved = depth.get();
        let next = saved + 1;
        if next > MAX_CALL_DEPTH {
            return Err(Control::Panic(AsError::at(
                "maximum recursion depth exceeded",
                span,
            )));
        }
        depth.set(next);
        Ok(DepthRestore { depth, saved })
    }
}

impl Drop for DepthRestore<'_> {
    fn drop(&mut self) {
        self.depth.set(self.saved);
    }
}

/// SP3 §B / O1: RAII helper that RESETS the expression-nesting counter to 0 for the
/// duration of a function-body execution and restores it on `Drop`. A call boundary
/// starts a FRESH expression-nesting context — exactly like the VM, whose
/// `compile_expr` depth resets per compiled function body. Without this reset the
/// live `expr_depth` would accumulate across recursive `run_body` frames (the
/// caller's `1 + f(n-1)` `eval_expr` frames stay on the native stack while the
/// callee runs), so deep recursion would trip `EXPR_NEST_LIMIT` at ~half the call
/// depth — making the tree-walker diverge from the VM (which has no runtime
/// expr-nesting counter). With the reset, recursion is bounded SOLELY by
/// `call_depth` on both engines, and expression nesting is bounded per body.
pub(crate) struct ExprDepthReset<'a> {
    depth: &'a Cell<u32>,
    saved: u32,
}

impl<'a> ExprDepthReset<'a> {
    fn enter(depth: &'a Cell<u32>) -> Self {
        let saved = depth.get();
        depth.set(0);
        ExprDepthReset { depth, saved }
    }
}

impl Drop for ExprDepthReset<'_> {
    fn drop(&mut self) {
        self.depth.set(self.saved);
    }
}

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
    /// DX D2 Task 10: tests SKIPPED by `--filter` (name didn't match). Not counted as
    /// passed or failed — reported separately as "N filtered".
    pub filtered: usize,
    /// `(test name, failure message)` for each failed test.
    pub failures: Vec<(String, String)>,
}

impl TestSummary {
    /// DX D2 — print the one-line tally (`ok. P passed; F failed[; K filtered]`). The
    /// `filtered` clause is OMITTED when zero so a run with no `--filter` is byte-identical
    /// to the historical output. Shared by the one-shot dispatch and the `--watch` re-runs.
    pub fn print_tally(&self) {
        if self.filtered > 0 {
            println!(
                "ok. {} passed; {} failed; {} filtered",
                self.passed, self.failed, self.filtered
            );
        } else {
            println!("ok. {} passed; {} failed", self.passed, self.failed);
        }
    }
}

impl TestSummary {
    /// DX D2 Task 5 — the airlock shape. Encode this summary as a `Value::object_cell`
    /// of EXISTING sendable kinds so a worker isolate can ship it back across the
    /// structured-clone airlock (`serialize::encode`) and the parent decode it.
    /// Shape: `{passed: number, failed: number, failures: array<{name, message}>}`.
    /// No new sendable `Value` kind — an ordinary `Object` of numbers/strings/arrays.
    pub fn to_value(&self) -> Value {
        let failures: Vec<Value> = self
            .failures
            .iter()
            .map(|(name, message)| {
                let mut f = indexmap::IndexMap::new();
                f.insert("name".to_string(), Value::str(name.as_str()));
                f.insert("message".to_string(), Value::str(message.as_str()));
                Value::object(f)
            })
            .collect();
        let mut map = indexmap::IndexMap::new();
        map.insert("passed".to_string(), Value::int(self.passed as i64));
        map.insert("failed".to_string(), Value::int(self.failed as i64));
        // DX D2 Task 10: carry the filtered count across the airlock so a parallel isolate's
        // `--filter` skips aggregate correctly.
        map.insert("filtered".to_string(), Value::int(self.filtered as i64));
        map.insert(
            "failures".to_string(),
            Value::array(failures),
        );
        Value::object(map)
    }

    /// DX D2 Task 5 — reconstruct a `TestSummary` from the `Value::object_cell` produced
    /// by [`TestSummary::to_value`] after it crosses the airlock (lossless round-trip).
    /// Returns `None` on a malformed shape (a defensive guard: a corrupt isolate
    /// result is a clean error at the call site, never a panic).
    pub fn from_value(v: &Value) -> Option<TestSummary> {
        let obj = v.as_object()?;
        let passed = match obj.get("passed")?.kind() {
            ValueKind::Int(n) if n >= 0 => n as usize,
            _ => return None,
        };
        let failed = match obj.get("failed")?.kind() {
            ValueKind::Int(n) if n >= 0 => n as usize,
            _ => return None,
        };
        // DX D2 Task 10: `filtered` is tolerated-absent (default 0) for forward/back
        // compatibility of the airlock shape.
        let filtered = match obj.get("filtered").map(|v| v.into_kind()) {
            Some(OwnedKind::Int(n)) if n >= 0 => n as usize,
            Some(_) => return None,
            None => 0,
        };
        let arr = match obj.get("failures")?.kind() {
            ValueKind::Array(a) => a.borrow().clone(),
            _ => return None,
        };
        let mut failures = Vec::with_capacity(arr.len());
        for entry in arr.iter() {
            let fobj = entry.as_object()?;
            let name = match fobj.get("name")?.kind() {
                ValueKind::Str(s) => s.to_string(),
                _ => return None,
            };
            let message = match fobj.get("message")?.kind() {
                ValueKind::Str(s) => s.to_string(),
                _ => return None,
            };
            failures.push((name, message));
        }
        Some(TestSummary {
            passed,
            failed,
            filtered,
            failures,
        })
    }
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
            call_depth: Cell::new(0),
            expr_depth: Cell::new(0),
            #[cfg(feature = "log")]
            log_level: Cell::new(log_level_from_env_str(
                std::env::var("ASCRIPT_LOG").ok().as_deref(),
            )),
            #[cfg(feature = "log")]
            log_format: Cell::new(LogFormat::Human),
            #[cfg(feature = "log")]
            log_capture: RefCell::new(String::new()),
            #[cfg(feature = "telemetry")]
            telemetry: RefCell::new(None),
            #[cfg(feature = "telemetry")]
            telemetry_capture: RefCell::new(Vec::new()),
            #[cfg(feature = "resilience")]
            resilience: RefCell::new(crate::stdlib::resilience::ResilState::default()),
            #[cfg(feature = "ai")]
            ai: RefCell::new(crate::stdlib::ai::AiClient::default()),
            cli_args: RefCell::new(Vec::new()),
            determinism: RefCell::new(None),
            package_resolver: RefCell::new(None),
            iface_verdict_cache: RefCell::new(HashMap::new()),
            iface_cache_gen: std::cell::Cell::new(0),
            // FFI §4.3: default = ALL granted → byte-identical for every existing
            // program (the gate short-circuits until something is dropped).
            caps: RefCell::new(crate::stdlib::caps::CapSet::all_granted()),
            // Top-level / dedicated isolate → drops are durable. A pooled worker
            // clears this (Task 8) so a leaking drop is refused.
            caps_drop_allowed: Cell::new(true),
            worker_source: RefCell::new(None),
            worker_aso_bytes: RefCell::new(None),
            worker_archive_bytes: RefCell::new(None),
            // DX D2 Task 8: snapshot re-baseline is OFF by default — a normal run
            // never overwrites a changed snapshot and never deletes an orphan.
            snapshot_update: Cell::new(false),
            snapshots_touched: RefCell::new(std::collections::BTreeSet::new()),
            // ELIDE §4.3 Task 3.2: marking pass is OFF by default so every existing
            // run is byte-identical (Task 4.1 decides the default-on path).
            elide_mode: Cell::new(false),
            // ELIDE §6.3 paranoid mode: OFF by default. Enabled by
            // `set_paranoid_set` / `ASCRIPT_ELIDE_PARANOID=1`. When off, all
            // contract-failure paths are entirely unaffected (zero cost).
            paranoid_set: RefCell::new(None),
        }
    }

    /// FFI §4.5: install the initial capability set (CLI/manifest-composed, Task 5,
    /// or a dedicated worker's reduced set, Task 8). Called ONCE after construction
    /// and BEFORE running any program code. Replaces the default all-granted set.
    pub fn set_caps(&self, caps: crate::stdlib::caps::CapSet) {
        *self.caps.borrow_mut() = caps;
    }

    /// A clone of the current capability set (for tests / worker spawn plumbing).
    pub fn caps(&self) -> crate::stdlib::caps::CapSet {
        self.caps.borrow().clone()
    }

    /// A `Copy` snapshot of the grant bitset — what the dispatch gate reads. The
    /// borrow is released immediately (the returned value is `Copy`), so it is
    /// never held across an `.await`.
    pub(crate) fn caps_bits(&self) -> crate::stdlib::caps::CapBits {
        self.caps.borrow().bits_snapshot()
    }

    /// FFI §4.5a: set whether `caps.drop` is permitted on this isolate. Cleared by
    /// a pooled `worker fn` request install (Task 8); left `true` on the top-level
    /// program and a dedicated isolate.
    // Used by the pooled-worker request install (Task 8); the field already gates
    // `caps.drop` (Task 4). Allowed-dead until Task 8 wires the pooled install.
    #[allow(dead_code)]
    pub fn set_caps_drop_allowed(&self, allowed: bool) {
        self.caps_drop_allowed.set(allowed);
    }

    /// Whether `caps.drop`/`caps.dropAll` may mutate this isolate's `caps` (§4.5a).
    /// Consumed by `std/caps` `drop`/`dropAll` routing.
    pub(crate) fn caps_drop_allowed(&self) -> bool {
        self.caps_drop_allowed.get()
    }

    /// ELIDE §4.3 Task 3.2: enable/disable the AST marking pass on the tree-walker
    /// module loader. When `true`, each module's source is run through
    /// `elision_proofs` and `mark_program` before execution. Defaults to `false`
    /// so every existing run is byte-identical (Task 4.1 decides the default-on
    /// path). Exposed here so integration tests can turn it on without changing
    /// any run path.
    pub fn set_elide_mode(&self, on: bool) {
        self.elide_mode.set(on);
    }

    /// ELIDE §6.3: install a merged [`ElisionSet`] for paranoid-mode proof
    /// verification. Called once before execution when `ASCRIPT_ELIDE_PARANOID=1`
    /// is set. The set is built from all per-module proof results; any contract
    /// failure at a proven site is escalated to the soundness-bug message instead
    /// of the normal panic.
    #[cfg(not(ascript_rt))]
    pub fn set_paranoid_set(&self, set: crate::check::infer::elide::ElisionSet) {
        *self.paranoid_set.borrow_mut() = Some(set);
    }

    /// ELIDE §6.3 test-only seam: inject a FAKE call-site span into the paranoid
    /// set (simulating a checker soundness bug — a span the predicate would NOT
    /// have proven). Used by `tests/elide.rs` to verify the escalation path fires.
    /// Only reachable from `#[cfg(any(test, feature = "fuzzgen", fuzzing))]` callers.
    #[cfg(all(not(ascript_rt), any(test, feature = "fuzzgen", fuzzing)))]
    pub fn inject_paranoid_call_span(&self, start: u32, end: u32) {
        let mut guard = self.paranoid_set.borrow_mut();
        let set = guard.get_or_insert_with(crate::check::infer::elide::ElisionSet::default);
        set.calls.insert((start, end));
    }

    /// ELIDE §6.3: check whether a contract failure at `span` should be escalated
    /// to a paranoid proof-violation panic. Returns `Some(Control::Panic(…))` when
    /// paranoid mode is active AND the span is in the proven set (checker soundness
    /// bug); returns `None` otherwise (caller falls through to the normal panic).
    ///
    /// Consulted ONLY when a contract check is ALREADY failing — zero hot-path cost.
    pub(crate) fn maybe_paranoid_escalate(
        &self,
        ty: &crate::ast::Type,
        value: &Value,
        span: crate::span::Span,
    ) -> Option<Control> {
        // RT §2.2: paranoid mode is never armed on a stub (the source entry points that
        // set the proof set are gated out) → the set is always empty. Short-circuit so
        // the body's `crate::check`-typed field accessors aren't needed at compile time.
        #[cfg(ascript_rt)]
        {
            let _ = (ty, value, span);
            None
        }
        #[cfg(not(ascript_rt))]
        {
        let guard = self.paranoid_set.borrow();
        let set = guard.as_ref()?;
        let key = (span.start as u32, span.end as u32);
        if set.calls.contains(&key)
            || set.lets.contains(&key)
            || set.fn_rets.contains(&key)
        {
            Some(
                AsError::at(
                    format!(
                        "ELIDE proof violated (checker soundness bug): \
                         proven site at {}:{} failed — expected {}, got {} ({})",
                        span.start,
                        span.end,
                        ty,
                        type_name(value),
                        value,
                    ),
                    span,
                )
                .into(),
            )
        } else {
            None
        }
        }
    }

    /// DX D2 Task 8: enable/disable snapshot "update mode" (the `--update-snapshots`
    /// re-baseline). Set by the test runner BEFORE loading test files; the
    /// `assert.snapshot` handler reads it via [`Interp::snapshot_update_mode`].
    pub fn set_snapshot_update(&self, update: bool) {
        self.snapshot_update.set(update);
    }

    /// DX D2 Task 8: whether snapshot re-baseline is active (`--update-snapshots`).
    /// Read only by the `sys`+`data`-gated `assert.snapshot` handler.
    #[allow(dead_code)]
    pub(crate) fn snapshot_update_mode(&self) -> bool {
        self.snapshot_update.get()
    }

    /// DX D2 Task 8: record that an `assert.snapshot` call touched `path` this run
    /// (so a `.snap` file no assertion touched can be detected as an orphan). A short
    /// borrow; never held across an `.await`. Used only on a `sys`+`data` build.
    #[allow(dead_code)]
    pub(crate) fn record_snapshot_touched(&self, path: PathBuf) {
        self.snapshots_touched.borrow_mut().insert(path);
    }

    /// DX D2 Task 8: a sorted snapshot of the `.snap` files touched this run. Used by
    /// the post-run orphan scan. Deterministic order (the set is a `BTreeSet`). Used
    /// only on a `sys`+`data` build (the serial-path orphan reporter).
    #[allow(dead_code)]
    pub(crate) fn snapshots_touched(&self) -> Vec<PathBuf> {
        self.snapshots_touched.borrow().iter().cloned().collect()
    }

    /// Irreversibly **deny** `cap` on this isolate (`caps.drop`). Subtractive only —
    /// there is no inverse. The caller (`call_caps`) has already verified
    /// [`caps_drop_allowed`](Interp::caps_drop_allowed).
    pub(crate) fn caps_deny(&self, cap: crate::stdlib::caps::Cap) {
        self.caps.borrow_mut().deny(cap);
    }

    /// Irreversibly deny ALL five dangerous capabilities (`caps.dropAll`).
    pub(crate) fn caps_deny_all(&self) {
        self.caps.borrow_mut().deny_all_dangerous();
    }

    /// FFI §4.3: the ONE capability gate. Given a required [`Cap`] (from
    /// [`crate::stdlib::required_cap`]) raise the recoverable Tier-2 denial panic
    /// `capability '<name>' denied` if it is not granted. Reads a `Copy` bitset
    /// **snapshot** (never holds the `caps` borrow), so it is await-safe.
    ///
    /// **Gate-12:** the caller short-circuits when ALL caps are granted; this
    /// helper is only reached when *something* has been dropped, so the single
    /// bitset test here is off the hot path.
    pub(crate) fn require_cap(
        &self,
        cap: crate::stdlib::caps::Cap,
        _module: &str,
        _func: &str,
        _args: &[Value],
        span: Span,
    ) -> Result<(), Control> {
        use crate::stdlib::caps::CapDecision;
        // Short, await-free borrow to read the dispatch decision (consults the
        // optional fs/net carve-out scope). The borrow is dropped before return.
        let decision = self.caps.borrow().dispatch_decision(cap);
        match decision {
            // Granted-outright, OR a granular carve-out is configured (the
            // connect/bind / fs-path STAGE 2 enforces the allow-list). Defer passes
            // the dispatch gate here; the resolved host/path isn't known yet.
            CapDecision::Allow | CapDecision::Defer => Ok(()),
            // Recoverable Tier-2 panic (catchable by `recover`): a host sandboxing a
            // plugin observes the denial rather than crashing. Names the capability.
            CapDecision::Deny => Err(Control::Panic(AsError::at(
                format!("capability '{}' denied", cap.name()),
                span,
            ))),
        }
    }

    /// FFI §4.4 STAGE 2 (net): enforce a `net` carve-out against a resolved `host`
    /// at connect/bind time. **Gate-12 fast path:** when no `net` carve-out is
    /// configured (`net_scope` is `None` — the default and the all-deny/all-grant
    /// cases) this returns `Ok(())` immediately with **no host comparison** — the
    /// dispatch-site bitset test was already conclusive. The allow-list is consulted
    /// ONLY when a carve-out exists.
    /// FFI §4.4 (BLOCKER 1): is a `net` carve-out configured on this isolate? Used by
    /// the HTTP path to decide whether redirects must be disabled (a redirect could
    /// escape the host allow-list, which we only validate for the initial host).
    /// Gate-12: a single `Option::is_some` on the `Copy`-cheap borrow — no comparison.
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub(crate) fn net_carveout_active(&self) -> bool {
        self.caps.borrow().net_scope.is_some()
    }

    #[cfg_attr(not(feature = "net"), allow(dead_code))] // only the net entries call it
    pub(crate) fn check_net_host(&self, host: &str, span: Span) -> Result<(), Control> {
        // Borrow is await-free and dropped before return.
        let allowed = {
            let caps = self.caps.borrow();
            match &caps.net_scope {
                None => return Ok(()), // Gate-12: no carve-out → no host comparison.
                Some(scope) => scope.allows_host(host),
            }
        };
        if allowed {
            Ok(())
        } else {
            Err(Control::Panic(AsError::at(
                format!("capability 'net' denied for host '{host}'"),
                span,
            )))
        }
    }

    /// FFI §4.4 STAGE 2 (fs): enforce an `fs` carve-out against a resolved `path`
    /// (a write iff `is_write`) at the path-resolving entry. **Gate-12 fast path:**
    /// when no `fs` carve-out is configured (`fs_scope` is `None`) this returns
    /// `Ok(())` immediately with **no path canonicalization**. Canonicalization is
    /// paid for ONLY by programs that configure a carve-out.
    pub(crate) fn check_fs_path(
        &self,
        path: &std::path::Path,
        is_write: bool,
        span: Span,
    ) -> Result<(), Control> {
        let allowed = {
            let caps = self.caps.borrow();
            match &caps.fs_scope {
                None => return Ok(()), // Gate-12: no carve-out → no canonicalization.
                Some(scope) => scope.allows_path(path, is_write),
            }
        };
        if allowed {
            Ok(())
        } else {
            Err(Control::Panic(AsError::at(
                format!("capability 'fs' denied for path '{}'", path.display()),
                span,
            )))
        }
    }

    /// Workers Spec A: record the entry program's full source so a `worker fn`
    /// dispatch can recompile it into the shippable code slice. Idempotent; the run
    /// entry points call it once before execution.
    pub fn set_worker_source(&self, src: &str) {
        *self.worker_source.borrow_mut() = Some(Rc::from(src));
    }

    /// The entry program's source, if recorded (Workers Spec A). Cloned out so the
    /// borrow never spans the compile/await below.
    pub(crate) fn worker_source(&self) -> Option<Rc<str>> {
        self.worker_source.borrow().clone()
    }

    /// Workers Spec A (.aso path): record the raw `.aso` bytes of the entry program so
    /// `dispatch_worker_closure` can re-parse them into a top-level chunk and build a
    /// code slice directly (no source recompile needed). Called by `run_aso_file` before
    /// execution; `worker_source` takes priority and this is `None` whenever source is set.
    pub fn set_worker_aso_bytes(&self, bytes: Rc<[u8]>) {
        *self.worker_aso_bytes.borrow_mut() = Some(bytes);
    }

    /// The raw `.aso` bytes of the entry program (`.aso` run path), if recorded.
    /// Cloned out so the borrow never spans the re-parse/dispatch below.
    pub(crate) fn worker_aso_bytes(&self) -> Option<Rc<[u8]>> {
        self.worker_aso_bytes.borrow().clone()
    }

    /// SELF-CONTAINED-BUNDLES Task 1.6: record the encoded `ModuleArchive` bytes of a bundled
    /// multi-module program so each worker isolate can decode + install the archive on its own
    /// `Vm` before re-running the program's top-level imports. Called by the archive run entry
    /// points (`run_verified_archive` / `run_archive`). Mirrors [`set_worker_aso_bytes`].
    pub fn set_worker_archive_bytes(&self, bytes: Rc<[u8]>) {
        *self.worker_archive_bytes.borrow_mut() = Some(bytes);
    }

    /// The encoded `ModuleArchive` bytes of a bundled program, if recorded. Cloned out (an
    /// `Rc` bump) so the borrow never spans the `.to_vec()`/dispatch at a worker site;
    /// `None` for an ordinary unbundled program. Mirrors [`worker_aso_bytes`].
    pub(crate) fn worker_archive_bytes(&self) -> Option<Rc<[u8]>> {
        self.worker_archive_bytes.borrow().clone()
    }

    /// Store the script's trailing CLI arguments so `env.args()` can return them.
    /// Called by `run_file` after construction, before execution.
    pub fn set_cli_args(&self, args: &[String]) {
        *self.cli_args.borrow_mut() = args.iter().map(|s| Rc::from(s.as_str())).collect();
    }

    /// Return the stored CLI args as a `Value::array_cell` of strings.
    /// Called from `env.args` (sys-gated) and `cli.parse` (always available).
    pub(crate) fn get_cli_args(&self) -> Value {
        let args: Vec<Value> = self
            .cli_args
            .borrow()
            .iter()
            .map(|s| Value::str(s.clone()))
            .collect();
        Value::array(args)
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
    /// higher-order stdlib function reaching a `Value::closure` in
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

    /// The shared logical-recursion-depth cell (SP3 §B). Both engines acquire a
    /// [`DepthGuard`] over this ONE cell so they trip [`MAX_CALL_DEPTH`] at the same
    /// logical depth. Used by the VM (which holds an `Rc<Interp>`) and the compiler.
    pub(crate) fn call_depth_cell(&self) -> &Cell<u32> {
        &self.call_depth
    }

    // ===================================================================== //
    // SP9 §3 — determinism seams. The accessors below read/advance the        //
    // `determinism` context through SHORT borrows that are always dropped     //
    // before returning (never held across an `.await`).                       //
    // ===================================================================== //

    /// Enter deterministic Record mode with `seed`, the virtual clock started at a
    /// FIXED, seed-derived epoch (NOT the real wall clock) so two same-seed runs are
    /// byte-identical on the clock too (the determinism oracle, spec §3.5). Installs a
    /// fresh [`crate::det::DeterminismContext`]; used by the `--deterministic` test
    /// seam. Returns the previous context (if any) so a caller can restore it.
    pub(crate) fn enter_deterministic(
        &self,
        seed: u64,
    ) -> Option<crate::det::DeterminismContext> {
        let start_ms = crate::det::deterministic_start_ms(seed);
        self.determinism
            .borrow_mut()
            .replace(crate::det::DeterminismContext::record(seed, start_ms))
    }

    /// Install an explicit determinism context (Record or Replay), returning the
    /// previous one. Used by `workflow.resume` to prime a Replay context with the
    /// recorded event stream.
    #[cfg(feature = "workflow")]
    pub(crate) fn install_determinism(
        &self,
        ctx: crate::det::DeterminismContext,
    ) -> Option<crate::det::DeterminismContext> {
        self.determinism.borrow_mut().replace(ctx)
    }

    /// Remove and return the current determinism context (end of a workflow), so the
    /// caller can read the recorded `events` to persist + restore the previous one.
    #[cfg(feature = "workflow")]
    pub(crate) fn take_determinism(&self) -> Option<crate::det::DeterminismContext> {
        self.determinism.borrow_mut().take()
    }

    /// Restore a previously-saved determinism context (or clear it when `None`).
    #[cfg(feature = "workflow")]
    pub(crate) fn restore_determinism(&self, prev: Option<crate::det::DeterminismContext>) {
        *self.determinism.borrow_mut() = prev;
    }

    /// True iff deterministic mode is active. A cheap `is_some` check on the seam
    /// fast paths (the default `None` path is byte-identical to pre-SP9).
    pub(crate) fn is_deterministic(&self) -> bool {
        self.determinism.borrow().is_some()
    }

    /// The wall clock in ms-epoch: the virtual/recorded clock when deterministic,
    /// else the real wall clock. The seam for `time.now` / `date.now`.
    pub(crate) fn clock_now_ms(&self) -> f64 {
        let mut guard = self.determinism.borrow_mut();
        match guard.as_mut() {
            Some(ctx) => ctx.clock_now_ms(),
            None => real_now_ms(),
        }
    }

    /// The monotonic clock in ms: the virtual/recorded clock when deterministic, else
    /// the real monotonic clock (caller passes the real value for the `None` path so
    /// this module needs no `Instant` baseline). The seam for `time.monotonic`.
    pub(crate) fn clock_monotonic_ms(&self, real_value: f64) -> f64 {
        let mut guard = self.determinism.borrow_mut();
        match guard.as_mut() {
            Some(ctx) => ctx.clock_monotonic_ms(),
            None => real_value,
        }
    }

    /// RESIL §5.2: the remaining deadline budget in ms (≥ 0), or `None` when no
    /// deadline is set on the current task's [`TASK_LOCALS`]. `now` is the det-routed
    /// monotonic clock so it agrees with how the deadline was computed. Zero-cost when
    /// unset (a `try_with` → `None` short-circuits before any clock read).
    /// Consulted by the §5.4 deadline-aware sites (`resilience`/`net`/`postgres`/
    /// `redis`/`sql`); dead under a bare `--no-default-features` build.
    #[allow(dead_code)]
    pub(crate) fn deadline_remaining_ms(&self) -> Option<f64> {
        let at = task_locals_current().and_then(|l| l.deadline_at_ms)?;
        let now = self.clock_monotonic_ms(crate::stdlib::time::real_monotonic_ms());
        Some((at - now).max(0.0))
    }

    /// The next seeded `[0,1)` random value when deterministic, or `None` when not
    /// (so the caller falls back to today's thread-local PRNG — byte-identical).
    pub(crate) fn next_seeded_f64(&self) -> Option<f64> {
        let mut guard = self.determinism.borrow_mut();
        guard.as_mut().map(|ctx| ctx.next_random_f64())
    }

    /// Fill `buf` with deterministic bytes when in deterministic mode — the random
    /// source for `uuid.v4`, the `uuid.v7` random tail, `crypto.randomBytes`, and the
    /// `crypto.hashPassword` / `crypto.bcryptHash` salts. Returns `true` if it filled;
    /// `false` means not deterministic and the caller uses its real RNG (byte-identical
    /// default).
    /// Gated on the features whose modules call it so it is not dead under
    /// `--no-default-features` (where `uuid`/`crypto` are compiled out).
    #[cfg(any(feature = "data", feature = "crypto"))]
    pub(crate) fn fill_seeded_bytes(&self, buf: &mut [u8]) -> bool {
        let mut guard = self.determinism.borrow_mut();
        match guard.as_mut() {
            Some(ctx) => {
                // Task 0.19c: event-source the byte draw (record the drawn bytes /
                // replay them verbatim + detect a desync), symmetric with
                // `next_seeded_f64` → `next_random_f64`. The NON-deterministic path
                // (`None`) is unchanged: returns false so the caller uses the real CSPRNG.
                ctx.next_seeded_bytes(buf);
                true
            }
            None => false,
        }
    }

    /// Run `f` with a mutable borrow of the determinism context, if active. Used by
    /// `std/workflow` to append/consume activity + timer events. `None` when not in
    /// deterministic mode. The borrow is local to `f` and never spans an `.await`
    /// (callers pass a synchronous closure).
    pub(crate) fn with_determinism_mut<R>(
        &self,
        f: impl FnOnce(&mut crate::det::DeterminismContext) -> R,
    ) -> Option<R> {
        let mut guard = self.determinism.borrow_mut();
        guard.as_mut().map(f)
    }

    /// FFI Task 10 (§7): the current determinism mode, or `None` when INERT (the
    /// default — the FFI path is then byte-identical to Unit B). A `Copy` snapshot read
    /// through a short borrow that is dropped before return (await-safe). `ffi.rs`'s
    /// `sym.call` consults this to decide Record / Replay / pass-through.
    #[cfg(feature = "ffi")] // only the FFI seam consults these
    pub(crate) fn determinism_mode(&self) -> Option<crate::det::Mode> {
        self.determinism.borrow().as_ref().map(|ctx| ctx.mode)
    }

    /// FFI Task 10 (§7A): in Record mode, append an `FfiCall` event (the marshalled
    /// return + post-call `Bytes` out-param snapshots). A no-op when not deterministic.
    #[cfg(feature = "ffi")]
    pub(crate) fn record_ffi_call(
        &self,
        ret: crate::det::FfiRet,
        out_params: Vec<(usize, Vec<u8>)>,
    ) {
        if let Some(ctx) = self.determinism.borrow_mut().as_mut() {
            ctx.record_ffi_call(ret, out_params);
        }
    }

    /// FFI Task 10 (§7A): in Replay mode, return the recorded `FfiCall` outcome (the
    /// marshalled return plus the out-param byte snapshots) WITHOUT re-invoking C, or
    /// `None` to fall through to a real call (stream exhausted / kind mismatch). The
    /// borrow is short and await-free.
    #[cfg(feature = "ffi")]
    #[allow(clippy::type_complexity)]
    pub(crate) fn replay_ffi_call(
        &self,
    ) -> Option<(crate::det::FfiRet, Vec<(usize, Vec<u8>)>)> {
        self.determinism
            .borrow_mut()
            .as_mut()
            .and_then(|ctx| ctx.replay_ffi_call())
    }

    /// Acquire a SNAPSHOT-RESTORE depth guard for a VM re-entrant `Vm::run`
    /// boundary (SP3 §B). On drop it restores the counter to its pre-entry value,
    /// absorbing frames abandoned by a panic unwind so `recover` resumes at the
    /// correct depth (see [`DepthRestore`]).
    pub(crate) fn enter_call_depth_scoped(&self, span: Span) -> Result<DepthRestore<'_>, Control> {
        DepthRestore::enter(&self.call_depth, span)
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
                let s = match args.first().map(|v| v.kind()) {
                    Some(ValueKind::Str(s)) => s.to_string(),
                    _ => {
                        return Err(AsError::at("log.setLevel expects a level string", span).into())
                    }
                };
                match level_of(&s) {
                    Some(l) => {
                        self.set_log_level(l);
                        Ok(Value::nil())
                    }
                    None => Err(AsError::at(format!("unknown log level {:?}", s), span).into()),
                }
            }
            "setFormat" => {
                let s = match args.first().map(|v| v.kind()) {
                    Some(ValueKind::Str(s)) => s.to_string(),
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
                        Ok(Value::nil())
                    }
                    "json" => {
                        self.set_log_format(LogFormat::Json);
                        Ok(Value::nil())
                    }
                    o => Err(AsError::at(format!("unknown log format {:?}", o), span).into()),
                }
            }
            "debug" | "info" | "warn" | "error" => {
                let lvl = level_of(func).unwrap();
                if lvl < self.log_level.get() {
                    return Ok(Value::nil());
                }
                let mut parts: Vec<String> = Vec::new();
                let mut fields = serde_json::Map::new();
                let mut iter = args.iter();
                // A thunk is only honored as the FIRST arg. It is invoked lazily
                // (after the level filter above) so a filtered call is free.
                if matches!(
                    args.first().map(|v| v.kind()),
                    // A VM-compiled thunk is a `Value::closure` — equally a deferred
                    // message callable (`call_value` dispatches it via the V4-T5
                    // bridge). Inert for the tree-walker (never makes a Closure).
                    Some(ValueKind::Function(_)) | Some(ValueKind::Closure(_)) | Some(ValueKind::Builtin(_))
                ) {
                    let r = self.call_value(args[0].clone(), vec![], span).await?;
                    // An `async fn` thunk returns a `Value::future`; drive it to
                    // completion using the same mechanism as `await` (M17).
                    let r = if let ValueKind::Future(f) = r.kind() {
                        f.get().await?
                    } else {
                        r
                    };
                    parts.push(r.to_string());
                    iter.next(); // consume index 0
                }
                for a in iter {
                    if let ValueKind::Object(o) = a.kind() {
                        for (k, val) in o.entries() {
                            fields.insert(
                                k.to_string(),
                                crate::stdlib::json::to_json_lossy(&val, &mut Vec::new()),
                            );
                        }
                    } else {
                        parts.push(a.to_string());
                    }
                }
                let msg = parts.join(" ");
                // RESIL §5.5: attach the ambient trace id (if a `withTrace` scope is
                // active) as a RESERVED `traceId` field. Zero-cost `None` path: when no
                // trace is set the read is a TLS lookup + `Cell` read and NO field is
                // added (so logs outside a trace scope stay byte-identical).
                let trace_id: Option<std::rc::Rc<str>> =
                    crate::interp::task_locals_current().and_then(|l| l.trace_id.clone());
                let line = match self.log_format.get() {
                    LogFormat::Json => {
                        let mut rec = serde_json::Map::new();
                        // User fields FIRST, then reserved keys, so a user field
                        // named `level`/`msg`/`traceId` can never clobber the
                        // authoritative ones.
                        for (k, v) in fields {
                            rec.insert(k, v);
                        }
                        rec.insert("level".into(), serde_json::Value::String(func.into()));
                        rec.insert("msg".into(), serde_json::Value::String(msg));
                        if let Some(tid) = &trace_id {
                            rec.insert(
                                "traceId".into(),
                                serde_json::Value::String(tid.to_string()),
                            );
                        }
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
                        if let Some(tid) = &trace_id {
                            s.push_str(&format!(" traceId={}", tid));
                        }
                        s
                    }
                };
                self.emit_log(&line);
                Ok(Value::nil())
            }
            other => Err(AsError::at(format!("std/log has no function '{}'", other), span).into()),
        }
    }

    /// Emit a `debug`-level structured log line SYNCHRONOUSLY (RESIL §6.1
    /// breadcrumb path): respects the level filter + format, then routes to
    /// [`Self::emit_log`]. Unlike [`Self::call_log`] this takes a static message +
    /// pre-resolved string fields (no thunk, no `Value` args) so it can be called
    /// from a non-async context. Used for resilience transition breadcrumbs (its
    /// only caller — so gated on both features).
    #[cfg(all(feature = "log", feature = "resilience"))]
    pub(crate) fn log_debug_breadcrumb(&self, msg: &str, fields: &[(&str, &str)]) {
        if LogLevel::Debug < self.log_level.get() {
            return;
        }
        let trace_id: Option<std::rc::Rc<str>> =
            crate::interp::task_locals_current().and_then(|l| l.trace_id.clone());
        let line = match self.log_format.get() {
            LogFormat::Json => {
                let mut rec = serde_json::Map::new();
                for (k, v) in fields {
                    rec.insert((*k).to_string(), serde_json::Value::String((*v).to_string()));
                }
                rec.insert("level".into(), serde_json::Value::String("debug".into()));
                rec.insert("msg".into(), serde_json::Value::String(msg.to_string()));
                if let Some(tid) = &trace_id {
                    rec.insert("traceId".into(), serde_json::Value::String(tid.to_string()));
                }
                serde_json::Value::Object(rec).to_string()
            }
            LogFormat::Human => {
                let mut s = if msg.is_empty() {
                    "[DEBUG]".to_string()
                } else {
                    format!("[DEBUG] {}", msg)
                };
                for (k, v) in fields {
                    s.push_str(&format!(" {}={}", k, v));
                }
                if let Some(tid) = &trace_id {
                    s.push_str(&format!(" traceId={}", tid));
                }
                s
            }
        };
        self.emit_log(&line);
    }

    // ---- SP12 std/telemetry: state access + the SP11-facing soft hook ----
    //
    // The hook methods (`telemetry_active`/`telemetry_span_start`/…) have
    // ALWAYS-PRESENT signatures; only their bodies are `#[cfg(feature =
    // "telemetry")]`-bridged. With the feature OFF they are inert (`false` /
    // `None` / no-op), so `std/ai` (SP11) calls them with NO `cfg` of its own and
    // NO telemetry import — `std/ai` builds with telemetry absent and
    // `std/telemetry` builds with ai absent. Tracing is OPT-IN: active only once
    // `telemetry.init` has run (`telemetry_active()`).

    /// True iff telemetry is initialized AND has at least one configured exporter
    /// (i.e. emitting is meaningful). The SP11 GenAI-span hook checks this so it
    /// emits nothing when telemetry is uninitialized.
    pub fn telemetry_active(&self) -> bool {
        #[cfg(feature = "telemetry")]
        {
            self.telemetry.borrow().is_some()
        }
        #[cfg(not(feature = "telemetry"))]
        {
            false
        }
    }

    /// Snapshot of the captured exporter HTTP requests (test hook). Empty under
    /// `Live`. Only present with the `telemetry` feature (the returned type lives
    /// behind the feature); SP12 capture-mode tests are likewise feature-gated.
    #[cfg(feature = "telemetry")]
    pub fn telemetry_capture(&self) -> Vec<crate::stdlib::telemetry::model::CapturedRequest> {
        self.telemetry_capture.borrow().clone()
    }

    /// Flattened snapshots of the currently-buffered (not-yet-flushed) spans
    /// (test hook). Lets the F1 tracing tests assert span semantics — name,
    /// parenting, status, attributes, events — without the F2 OTLP wire shaping.
    #[cfg(feature = "telemetry")]
    pub fn telemetry_spans_debug(&self) -> Vec<crate::stdlib::telemetry::model::SpanSnapshot> {
        self.telemetry
            .borrow()
            .as_ref()
            .map(|s| s.spans.iter().map(|sp| sp.snapshot()).collect())
            .unwrap_or_default()
    }

    /// Start a span through the telemetry pipeline (used by `std/ai`'s GenAI-span
    /// emission). Returns an opaque span id (the span resource id), or `None` when
    /// telemetry is absent/off so callers never branch on a feature. The span
    /// parents to the current scoped span if one is active, else is a trace root.
    pub fn telemetry_span_start(
        &self,
        name: &str,
        attrs: Vec<(String, Value)>,
    ) -> Option<u64> {
        #[cfg(feature = "telemetry")]
        {
            if !self.telemetry_active() {
                return None;
            }
            Some(self.telemetry_open_span(name, attrs))
        }
        #[cfg(not(feature = "telemetry"))]
        {
            let _ = (name, attrs);
            None
        }
    }

    /// Set an attribute on an open span (no-op if the id is unknown / feature off).
    pub fn telemetry_span_set(&self, id: u64, key: &str, val: Value) {
        #[cfg(feature = "telemetry")]
        {
            self.telemetry_span_set_attr(id, key, val);
        }
        #[cfg(not(feature = "telemetry"))]
        {
            let _ = (id, key, val);
        }
    }

    /// Add a timestamped event to an open span (no-op if unknown / feature off).
    pub fn telemetry_span_event(&self, id: u64, name: &str, attrs: Vec<(String, Value)>) {
        #[cfg(feature = "telemetry")]
        {
            self.telemetry_span_add_event(id, name, attrs);
        }
        #[cfg(not(feature = "telemetry"))]
        {
            let _ = (id, name, attrs);
        }
    }

    /// End an open span with a status, buffering it for export (no-op if unknown /
    /// feature off).
    pub fn telemetry_span_end(&self, id: u64, status: SpanStatus) {
        #[cfg(feature = "telemetry")]
        {
            self.telemetry_finish_span(id, status);
        }
        #[cfg(not(feature = "telemetry"))]
        {
            let _ = (id, status);
        }
    }

    /// The SP12 exporter send seam. In CAPTURE mode (tests/REPL/embedders) it
    /// records the request into the capture sink (read via `telemetry_capture()`)
    /// and performs NO network I/O — so unit tests assert the exact OTLP/Sentry/
    /// PostHog wire payloads with no socket and no secret. In LIVE mode it POSTs
    /// via the pooled reqwest client shared with `std/net/http`. A live failure is
    /// returned as `Err(message)` (the caller logs once + drops it — a telemetry
    /// failure never aborts the user's program, spec §5).
    ///
    /// No `RefCell`/`resources` borrow is held across the `.await` (the request is
    /// an owned value).
    #[cfg(feature = "telemetry")]
    pub(crate) async fn telemetry_send(
        &self,
        req: crate::stdlib::telemetry::model::CapturedRequest,
    ) -> Result<(), String> {
        if matches!(self.output, OutputSink::Capture(_)) {
            self.telemetry_capture.borrow_mut().push(req.clone());
            // Test seam: a test may force the capture send to "fail" to exercise
            // the error model (a flush failure is logged once + dropped, never
            // panics). Off by default; set per-thread via
            // `crate::telemetry_test_force_send_error`.
            if crate::stdlib::telemetry::test_force_send_error() {
                return Err(format!(
                    "telemetry {} export to {} failed: forced (test)",
                    req.exporter, req.url
                ));
            }
            return Ok(());
        }
        // Live: POST via the shared pooled client. Build the request fully before
        // awaiting; hold no borrow across the send.
        let client = crate::stdlib::net_http::shared_client();
        let mut rb = client
            .post(&req.url)
            .header("content-type", "application/json")
            .body(req.body);
        for (k, v) in &req.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        match rb.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    Ok(())
                } else {
                    Err(format!(
                        "telemetry {} export to {} failed: HTTP {}",
                        req.exporter, req.url, status
                    ))
                }
            }
            Err(e) => Err(format!(
                "telemetry {} export to {} failed: {}",
                req.exporter, req.url, e
            )),
        }
    }

    /// Set THIS task's current telemetry span context (returns the previous value
    /// so the caller can restore it — `telemetry.span` does save → set → await →
    /// restore around its callback). No-op (returns None) if the task-local is not
    /// in scope (telemetry off, or code running outside a telemetry scope).
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_set_current(
        &self,
        ctx: Option<crate::stdlib::telemetry::model::SpanCtx>,
    ) -> Option<crate::stdlib::telemetry::model::SpanCtx> {
        TELEMETRY_CURRENT
            .try_with(|c| c.replace(ctx))
            .ok()
            .flatten()
    }

    /// The current task's telemetry span context, if any (task-local; isolated
    /// across concurrent `spawn_local` tasks).
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_current(&self) -> Option<crate::stdlib::telemetry::model::SpanCtx> {
        crate::interp::telemetry_capture_current()
    }

    /// Open a new span: mint ids (root → new trace; else parent to the current
    /// scoped span), register it as a `TelemetrySpan` resource, and return its id.
    /// Caller must have confirmed telemetry is active.
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_open_span(&self, name: &str, attrs: Vec<(String, Value)>) -> u64 {
        use crate::stdlib::telemetry::model::{
            new_span_id, new_trace_id, now_unix_nanos, OpenSpan, SpanStatusRecord,
        };
        let (trace_id, parent_id) = match self.telemetry_current() {
            Some(ctx) => (ctx.trace_id, Some(ctx.span_id)),
            None => (new_trace_id(), None),
        };
        // RESIL §5.5: attach the ambient trace/request id (from a `withTrace` scope) as
        // a `trace_id` span attribute when one is set. Zero-cost `None` path (a TLS +
        // `Cell` read; no attr pushed when no trace is active).
        let mut attrs = attrs;
        if let Some(tid) = crate::interp::task_locals_current().and_then(|l| l.trace_id.clone()) {
            attrs.push(("trace_id".to_string(), Value::str(&*tid)));
        }
        let open = OpenSpan {
            trace_id,
            span_id: new_span_id(),
            parent_id,
            name: name.to_string(),
            start_unix_nano: now_unix_nanos(),
            attributes: attrs,
            events: Vec::new(),
            status: SpanStatusRecord::default(),
        };
        let handle = self.register_resource(
            crate::value::NativeKind::TelemetrySpan,
            indexmap::IndexMap::new(),
            ResourceState::TelemetrySpan(Box::new(open)),
        );
        match handle.kind() {
            ValueKind::Native(n) => n.id,
            _ => unreachable!("register_resource yields a Native handle"),
        }
    }

    /// Set an attribute on an open span (last-write-wins by key). No-op if the id
    /// is not a live span (already ended).
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_span_set_attr(&self, id: u64, key: &str, val: Value) {
        self.with_resource_mut(id, |r| {
            if let Some(ResourceState::TelemetrySpan(s)) = r {
                if let Some(slot) = s.attributes.iter_mut().find(|(k, _)| k == key) {
                    slot.1 = val;
                } else {
                    s.attributes.push((key.to_string(), val));
                }
            }
        });
    }

    /// Add a timestamped event to an open span. No-op if the span has ended.
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_span_add_event(&self, id: u64, name: &str, attrs: Vec<(String, Value)>) {
        use crate::stdlib::telemetry::model::{now_unix_nanos, SpanEvent};
        self.with_resource_mut(id, |r| {
            if let Some(ResourceState::TelemetrySpan(s)) = r {
                s.events.push(SpanEvent {
                    name: name.to_string(),
                    time_unix_nano: now_unix_nanos(),
                    attributes: attrs,
                });
            }
        });
    }

    /// Set an open span's status (and optional message). No-op if ended.
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_span_set_status(
        &self,
        id: u64,
        code: crate::stdlib::telemetry::model::SpanStatusCode,
        message: Option<String>,
    ) {
        self.with_resource_mut(id, |r| {
            if let Some(ResourceState::TelemetrySpan(s)) = r {
                s.status.code = code;
                if message.is_some() {
                    s.status.message = message;
                }
            }
        });
    }

    /// End an open span: freeze it into a `SpanRecord` and buffer it for export.
    /// No-op if the span has already ended (id absent) — calling a method after
    /// `end()` is documented as a no-op, not a panic.
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_finish_span(&self, id: u64, status: SpanStatus) {
        use crate::stdlib::telemetry::model::{now_unix_nanos, SpanStatusCode};
        // Take the open span out of the table (removes it; a second end is a no-op).
        let open = match self.take_resource(id) {
            Some(ResourceState::TelemetrySpan(s)) => *s,
            // Not a span (or already ended): put any other state back and bail.
            Some(other) => {
                self.return_resource(id, other);
                return;
            }
            None => return,
        };
        let mut record = open.finish(now_unix_nanos());
        // The hook's explicit status wins over a status set via setStatus only
        // when it is not Unset (so `span.setStatus("error")` then `end()` keeps
        // error; the scoped helper passes Ok/Error explicitly).
        match status {
            SpanStatus::Ok => record.status.code = SpanStatusCode::Ok,
            SpanStatus::Error => record.status.code = SpanStatusCode::Error,
            SpanStatus::Unset => {}
        }
        if let Some(st) = self.telemetry.borrow_mut().as_mut() {
            st.spans.push(record);
        }
    }

    /// Take the configured telemetry pipeline out of the cell (for an async flush
    /// across an `.await` without holding the `RefCell` borrow). Pair with
    /// [`Interp::telemetry_return_state`].
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_take_state(
        &self,
    ) -> Option<crate::stdlib::telemetry::model::TelemetryState> {
        self.telemetry.borrow_mut().take()
    }

    /// Put the telemetry pipeline back after an async flush. If a re-`init` ran
    /// during the flush (installing a new pipeline) the new one wins and the old
    /// is dropped.
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_return_state(
        &self,
        state: crate::stdlib::telemetry::model::TelemetryState,
    ) {
        let mut slot = self.telemetry.borrow_mut();
        if slot.is_none() {
            *slot = Some(state);
        }
    }

    /// Install a freshly-built telemetry pipeline (replacing any existing one,
    /// which the caller has already flushed). Set by `telemetry.init`.
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_install(
        &self,
        state: crate::stdlib::telemetry::model::TelemetryState,
    ) {
        *self.telemetry.borrow_mut() = Some(state);
    }

    /// Register (idempotently by name) a metric instrument and return its
    /// resource id (here just a monotonic id from the resource counter). Telemetry
    /// is known active by the caller.
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_register_instrument(
        &self,
        name: &str,
        kind: crate::stdlib::telemetry::model::MetricKind,
        unit: Option<String>,
        description: Option<String>,
    ) -> u64 {
        let mut slot = self.telemetry.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return u64::MAX;
        };
        // Idempotent: an existing instrument with the same name returns its id.
        if let Some(id) = state
            .instruments
            .iter()
            .find(|(_, inst)| inst.name == name)
            .map(|(id, _)| *id)
        {
            return id;
        }
        let id = self.next_id();
        state.instruments.insert(
            id,
            crate::stdlib::telemetry::model::MetricInstrument {
                name: name.to_string(),
                kind,
                unit,
                description,
                points: indexmap::IndexMap::new(),
                start_unix_nano: crate::stdlib::telemetry::model::now_unix_nanos(),
            },
        );
        id
    }

    /// Apply a metric sample (`add`/`record`/`set`) to the instrument's point for
    /// the given attribute set (cumulative temporality). No-op if unknown.
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_record_metric(
        &self,
        id: u64,
        method: &str,
        amount: f64,
        attrs: Vec<(String, Value)>,
    ) {
        use crate::stdlib::telemetry::model::{attr_key, MetricKind, MetricPoint};
        let key = attr_key(&attrs);
        let mut slot = self.telemetry.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return;
        };
        let Some(inst) = state.instruments.get_mut(&id) else {
            return;
        };
        let entry = inst
            .points
            .entry(key)
            .or_insert_with(|| (attrs, MetricPoint::default()));
        let point = &mut entry.1;
        let _ = method; // the kind determines the aggregation, not the method name
        match inst.kind {
            MetricKind::Counter => {
                point.value += amount;
                point.count += 1;
            }
            MetricKind::Gauge => {
                point.value = amount;
                point.count = 1;
            }
            MetricKind::Histogram => {
                if point.count == 0 {
                    point.min = amount;
                    point.max = amount;
                } else {
                    point.min = point.min.min(amount);
                    point.max = point.max.max(amount);
                }
                point.value += amount;
                point.count += 1;
            }
        }
    }

    /// Enqueue an analytics event for the next flush. No-op if uninitialized OR if
    /// there is no destination for events (no PostHog exporter AND mirroring to
    /// OTLP off) — per spec §4.2 a `capture` with nowhere to go is a no-op.
    #[cfg(feature = "telemetry")]
    pub(crate) fn telemetry_enqueue_event(
        &self,
        ev: crate::stdlib::telemetry::model::AnalyticsEvent,
    ) {
        if let Some(state) = self.telemetry.borrow_mut().as_mut() {
            if state.exporters.posthog.is_some() || state.mirror_events_to_otlp {
                state.events.push(ev);
            }
        }
    }

    /// Flush the telemetry pipeline at process exit (spec §2: an automatic flush
    /// on the existing shutdown path). A no-op if telemetry was never initialized
    /// or the feature is off. A flush failure is logged once to stderr and dropped
    /// — telemetry must never affect the program's exit. Buffered signals are
    /// cleared either way.
    pub async fn telemetry_flush_on_exit(&self) {
        #[cfg(feature = "telemetry")]
        {
            if !self.telemetry_active() {
                return;
            }
            if let Some(mut state) = self.telemetry_take_state() {
                let outcome =
                    crate::stdlib::telemetry::flush_state_public(self, &mut state).await;
                state.spans.clear();
                state.events.clear();
                self.telemetry_return_state(state);
                if let Err(msg) = outcome {
                    // Live builds warn once; capture builds (tests) stay quiet.
                    if matches!(self.output, OutputSink::Live) {
                        use std::io::Write;
                        let mut e = std::io::stderr().lock();
                        let _ = writeln!(e, "telemetry: flush-on-exit failed: {}", msg);
                    }
                }
            }
        }
    }

    /// `std/telemetry` dispatch (mirrors `call_log`). Delegates to the telemetry
    /// module, which owns the wire shaping; the `Interp` owns the state cells,
    /// resource handles, and the send seam.
    #[cfg(feature = "telemetry")]
    pub(crate) async fn call_telemetry(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        crate::stdlib::telemetry::dispatch(self, func, args, span).await
    }

    /// `std/ai` dispatch (SP11). Delegates to the ai module, which owns the genai
    /// request/response mapping; the `Interp` owns the genai `Client` lifetime
    /// (`self.ai`) + resource handles. Borrow discipline: the genai client is taken
    /// OUT of `self.ai` across each `.await` (take-out-across-await) so no `RefCell`
    /// borrow is ever held over a genai future.
    #[cfg(feature = "ai")]
    pub(crate) async fn call_ai(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        crate::stdlib::ai::dispatch(self, func, args, span).await
    }

    /// Mutable borrow of the per-`Interp` AI client state. SP11 take-out-across-await
    /// helper: callers clone out the genai `Client` (cheap `Arc` inside) before any
    /// `.await`, never holding this borrow across one.
    #[cfg(feature = "ai")]
    pub(crate) fn ai_state(&self) -> std::cell::RefMut<'_, crate::stdlib::ai::AiClient> {
        self.ai.borrow_mut()
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

    /// Register an OS `state` behind a fresh `Value::native` handle of `kind`,
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
        Value::native(std::rc::Rc::new(crate::value::NativeObject {
            id,
            kind,
            fields,
        }))
    }

    /// Mint a `Value::native` handle carrying only plain readable `fields` and NO
    /// backing OS resource (no `resources` table entry). Used by SP11 std/ai's
    /// provider/model/tool handles, which are pure config data — there is nothing to
    /// reclaim on drop, so they need no `ResourceState`. The id is still unique.
    #[cfg(feature = "ai")]
    pub(crate) fn make_native_data(
        &self,
        kind: crate::value::NativeKind,
        fields: indexmap::IndexMap<String, Value>,
    ) -> Value {
        let id = self.next_id();
        Value::native(std::rc::Rc::new(crate::value::NativeObject { id, kind, fields }))
    }

    /// Drive a value to completion if it is a `Value::future` (an `async fn`
    /// return), else return it unchanged. SP11's tool loop uses this to await an
    /// `async fn` tool `execute` (mirrors the `await` operator's semantics:
    /// `await` on a non-future is identity). A panic in the spawned task
    /// re-surfaces here.
    #[cfg(feature = "ai")]
    pub(crate) async fn await_if_future(&self, v: Value) -> Result<Value, Control> {
        if let ValueKind::Future(f) = v.kind() {
            f.get().await
        } else {
            Ok(v)
        }
    }

    /// Project a `shape:` argument (a `Value::class` or a `std/schema` tagged
    /// Object) into a JSON Schema (`serde_json::Value`) for SP11 structured output.
    /// A class's nested `Type::Named` fields resolve through the class's `def_env`
    /// (the same environment `validate_into` uses), so nested-class / `array<Class>` /
    /// `map<K,Class>` fields project to their full nested schema.
    #[cfg(feature = "ai")]
    pub(crate) fn project_shape_json(&self, shape: &Value) -> serde_json::Value {
        if let ValueKind::Class(c) = shape.kind() {
            crate::stdlib::ai::json_schema::class_to_json_schema_env(c, &c.def_env)
        } else {
            crate::stdlib::ai::json_schema::schema_value_to_json_schema(shape)
        }
    }

    /// Remove and return the resource for `id` (used by `close`/`kill`/EOF, and to
    /// own a resource across an `.await` without holding the table borrow — pair
    /// with `return_resource`). Used unconditionally by std/time timers, plus the
    /// feature-gated modules (sqlite/process/net/...).
    pub(crate) fn take_resource(&self, id: u64) -> Option<ResourceState> {
        self.resources.borrow_mut().remove(&id)
    }

    // -----------------------------------------------------------------------
    // Workers Spec B §Task 5: actor host (`ClassName.spawn`, handle dispatch).
    // Shared by BOTH engines (the tree-walker `eval_chain` hook and the VM
    // `Op::Call*` hook call these, so actor behavior is byte-identical).
    // -----------------------------------------------------------------------

    /// `WorkerClass.spawn(args)` → spawn a dedicated actor isolate, ship the class
    /// code slice + init args, register a `ResourceState::WorkerActor`, and return a
    /// `future<Value::native(WorkerActor)>` (spawning is async — the future resolves
    /// once the isolate has constructed the instance via `init`).
    ///
    /// `!Send` integrity: the isolate builds its OWN `Interp`/`Vm`; only `Send` bytes
    /// (the encoded args + class slice) and `Send` channels cross. The proxy stays on
    /// this thread.
    pub(crate) async fn spawn_actor(
        &self,
        class: &Rc<crate::value::Class>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        // Sendability gate on the init args (field-path panic on failure).
        for a in &args {
            crate::worker::serialize::check_sendable(a)
                .map_err(|e| Control::Panic(AsError::at(e.message(), span)))?;
        }
        // Build the class code slice (superclass chain + methods + defaults) from the
        // program source — the SINGLE path shared by both engines — or, when running a
        // compiled `.aso` (no source), from the stored `.aso` bytes (Plan A Task 15
        // mechanism extended to actor spawn).
        let slice = crate::worker::build_class_slice_for_interp(self, &class.name)?;
        // Encode the init args as one array (preserving cross-arg sharing).
        let args_array = Value::array(args);
        let (encoded, encoded_shared) = crate::worker::serialize::encode(&args_array)
            .map_err(|e| Control::Panic(AsError::at(e.message(), span)))?;

        // Spawn the dedicated actor isolate + its mailbox. Task 1.6: ship the bundled
        // program's archive (if any) so the isolate installs it before the class slice loads
        // — an actor method that calls into an imported module resolves it from memory.
        let archive_bytes = self.worker_archive_bytes().map(|b| b.to_vec());
        let (tx, isolate) =
            crate::worker::actor::spawn_actor_isolate(archive_bytes).map_err(|e| {
                Control::Panic(AsError::at(
                    format!("could not spawn actor isolate: {e}"),
                    span,
                ))
            })?;

        // Send the Init message; await the ack on a future.
        let (init_reply_tx, init_reply_rx) =
            tokio::sync::oneshot::channel::<crate::worker::actor::ActorReply>();
        let init_msg = crate::worker::actor::ActorMsg::Init {
            class_name: class.name.clone(),
            slice_bytes: slice.entry_aso.to_vec(),
            args: encoded,
            shared: encoded_shared,
            reply: init_reply_tx,
        };
        if tx.send(init_msg).is_err() {
            return Err(Control::Panic(AsError::at(
                "actor isolate terminated before initialization".to_string(),
                span,
            )));
        }

        // Register the handle as a native resource (its Drop tears the isolate down).
        let mut fields = indexmap::IndexMap::new();
        fields.insert("name".to_string(), Value::str(class.name.clone()));
        let handle = crate::worker::actor::WorkerActorHandle::new(
            tx,
            isolate,
            Rc::from(class.name.as_str()),
        );
        let native = self.register_resource(
            crate::value::NativeKind::WorkerActor,
            fields,
            ResourceState::WorkerActor(Box::new(handle)),
        );

        // The future resolves to the native handle once Init acks. We must keep the
        // native handle alive across the await (it owns the isolate) — so move a clone
        // into the bridge task and resolve with it.
        let fut = crate::task::SharedFuture::new();
        let cell = fut.cell();
        let native_for_task = native.clone();
        let bridge = tokio::task::spawn_local(async move {
            let result = match init_reply_rx.await {
                Ok(crate::worker::actor::ActorReply::Ok(..)) => Ok(native_for_task),
                Ok(crate::worker::actor::ActorReply::Panic(msg)) => {
                    Err(Control::Panic(AsError::at(msg, span)))
                }
                Err(_) => Err(Control::Panic(AsError::at(
                    "actor isolate terminated during initialization".to_string(),
                    span,
                ))),
            };
            cell.resolve(result);
        });
        fut.set_abort(bridge.abort_handle());
        Ok(Value::future(fut))
    }

    /// An async method call on an actor handle (`await handle.method(args)`), OR the
    /// synchronous `handle.close()` teardown. For a method: `check_sendable` the args,
    /// send an `ActorMsg::Call`, and return a `future<T>` awaiting the oneshot reply +
    /// decode. TAKE-OUT-ACROSS-AWAIT: the `Send` sender is cloned OUT of the resources
    /// table BEFORE the future awaits; no `resources` borrow is held across `.await`.
    pub(crate) async fn actor_handle_call(
        &self,
        native: &Rc<crate::value::NativeObject>,
        method: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        // `close()` is a host-side teardown: take the resource out (dropping the
        // handle → dropping the IsolateHandle → channel close + thread join).
        if method == "close" {
            self.take_resource(native.id);
            return Ok(Value::nil());
        }

        // Sendability gate on the args (field-path panic).
        for a in &args {
            crate::worker::serialize::check_sendable(a)
                .map_err(|e| Control::Panic(AsError::at(e.message(), span)))?;
        }
        let args_array = Value::array(args);
        let (encoded, encoded_shared) = crate::worker::serialize::encode(&args_array)
            .map_err(|e| Control::Panic(AsError::at(e.message(), span)))?;

        // SP9 determinism (Spec B Task 12) — REPLAY: if a determinism context is active
        // in Replay mode AND it has a recorded `ActorCall` at the cursor, return the
        // recorded reply WITHOUT crossing the isolate boundary (the actor's side effect
        // already ran exactly once, at Record). The `None`/default and Record paths fall
        // through to the real boundary crossing below — byte-identical when inert.
        // The borrow is a SHORT sync borrow inside `with_determinism_mut`, never held
        // across an `.await`.
        let replayed: Option<crate::det::BoundaryOutcome> = self.with_determinism_mut(|ctx| {
            if ctx.mode == crate::det::Mode::Replay {
                ctx.replay_actor_call(method)
            } else {
                None
            }
        }).flatten();
        if let Some(outcome) = replayed {
            return Ok(self.resolve_boundary_outcome(outcome, span));
        }

        // TAKE-OUT-ACROSS-AWAIT: clone the Send sender out under a SHORT borrow, then
        // drop the borrow before building/awaiting the future.
        let tx = {
            let table = self.resources.borrow();
            match table.get(&native.id) {
                Some(ResourceState::WorkerActor(h)) => h.tx.clone(),
                _ => {
                    // The actor was closed (resource removed) → recoverable panic.
                    return Err(Control::Panic(AsError::at(
                        "actor is closed".to_string(),
                        span,
                    )));
                }
            }
        };

        let (reply_tx, reply_rx) =
            tokio::sync::oneshot::channel::<crate::worker::actor::ActorReply>();
        let call_msg = crate::worker::actor::ActorMsg::Call {
            method: method.to_string(),
            args: encoded,
            shared: encoded_shared,
            reply: reply_tx,
        };
        if tx.send(call_msg).is_err() {
            return Err(Control::Panic(AsError::at(
                "actor is closed".to_string(),
                span,
            )));
        }

        // Bridge: await the reply, decode against THIS interp, resolve the future.
        let interp_rc = self.rc();
        let method_owned = method.to_string();
        let fut = crate::task::SharedFuture::new();
        let cell = fut.cell();
        let bridge = tokio::task::spawn_local(async move {
            let reply = reply_rx.await;
            // SP9 determinism (Task 12) — RECORD: if a Record-mode context is active,
            // event-source the boundary OUTCOME (the encoded reply bytes / panic) so a
            // later Replay reproduces it without re-crossing the isolate. The borrow is
            // a SHORT sync borrow AFTER the `.await`, never held across it.
            if let Ok(ref r) = reply {
                let outcome = match r {
                    // SRV: the determinism log is byte-only; an actor returning a
                    // `Value::shared` records its bytes (the `Arc` side-vector is not
                    // event-sourced — a frozen graph is not replayable bytes, an
                    // accepted determinism limitation for shared results).
                    crate::worker::actor::ActorReply::Ok(bytes, _shared) => {
                        crate::det::BoundaryOutcome::Bytes(bytes.clone())
                    }
                    crate::worker::actor::ActorReply::Panic(msg) => {
                        crate::det::BoundaryOutcome::Panic(msg.clone())
                    }
                };
                interp_rc.with_determinism_mut(|ctx| {
                    if ctx.mode == crate::det::Mode::Record {
                        ctx.record_actor_call(&method_owned, &outcome);
                    }
                });
            }
            let result = match reply {
                Ok(crate::worker::actor::ActorReply::Ok(bytes, shared)) => {
                    crate::worker::serialize::decode_with_shared(&bytes, &shared, &interp_rc)
                        .map_err(|e| Control::Panic(e.into()))
                }
                Ok(crate::worker::actor::ActorReply::Panic(msg)) => {
                    Err(Control::Panic(AsError::at(msg, span)))
                }
                // The reply sender dropped without replying → the actor was closed.
                Err(_) => Err(Control::Panic(AsError::at(
                    "actor is closed".to_string(),
                    span,
                ))),
            };
            cell.resolve(result);
        });
        fut.set_abort(bridge.abort_handle());
        Ok(Value::future(fut))
    }

    /// SP9 determinism (Task 12): wrap a REPLAYED actor-call boundary outcome into an
    /// already-resolved `Value::future`, matching the shape `actor_handle_call` returns
    /// for the real path. The recorded bytes are decoded on THIS consumer thread (no
    /// isolate crossing). A recorded panic replays as the same recoverable Tier-2 panic.
    fn resolve_boundary_outcome(
        &self,
        outcome: crate::det::BoundaryOutcome,
        span: Span,
    ) -> Value {
        let result = match outcome {
            crate::det::BoundaryOutcome::Bytes(bytes) => {
                crate::worker::serialize::decode(&bytes, &self.rc())
                    .map_err(|e| Control::Panic(e.into()))
            }
            crate::det::BoundaryOutcome::Panic(msg) => {
                Err(Control::Panic(AsError::at(msg, span)))
            }
            // An actor call never yields "done"; treat defensively as nil.
            crate::det::BoundaryOutcome::Done => Ok(Value::nil()),
        };
        let fut = crate::task::SharedFuture::new();
        fut.cell().resolve(result);
        Value::future(fut)
    }

    /// Workers Spec B §Task 6: build a streaming-generator handle for a `worker fn*`.
    /// Spawns a DEDICATED isolate running the generator body, ships the code slice +
    /// call args, and returns a `Value::generator` backed by a cross-thread
    /// [`crate::coro::GenImpl::Worker`] demand-driven driver. `for await` / `.next(v)` /
    /// `.close()` then work transparently — each consumer step is one demand credit
    /// across the boundary (strict pull = backpressure).
    ///
    /// Shared by BOTH engines (the tree-walker `call_function` and the VM `Op::Call`
    /// hooks call this), so streaming behavior is byte-identical.
    ///
    /// SPAWN IS SYNCHRONOUS here (unlike `spawn_actor`'s `future<handle>`): a generator
    /// call returns a `Value::generator` immediately, matching local generators — the
    /// isolate is spawned + the producer built (its `Init` ack awaited) before returning,
    /// so a build failure surfaces eagerly at the call, exactly like a local generator's
    /// eager arg binding.
    ///
    /// `!Send` integrity: the isolate builds its OWN `Interp`/`Vm` and runs its OWN
    /// generator; only `Send` bytes (slice + encoded args) cross. The driver stays here.
    pub(crate) async fn spawn_worker_stream(
        &self,
        entry_name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        // Sendability gate on the call args (field-path panic on failure).
        for a in &args {
            crate::worker::serialize::check_sendable(a)
                .map_err(|e| Control::Panic(AsError::at(e.message(), span)))?;
        }
        // Build the `worker fn*` code slice (entry + transitive top-level deps) from the
        // program source — the SINGLE path shared by both engines — or, when running a
        // compiled `.aso` (no source), from the stored `.aso` bytes (Plan A Task 15
        // mechanism extended to the worker-generator stream path).
        let slice = crate::worker::build_stream_slice_for_interp(self, entry_name)?;
        // Encode the call args as one array (preserving cross-arg sharing).
        let args_array = Value::array(args);
        let (encoded, encoded_shared) = crate::worker::serialize::encode(&args_array)
            .map_err(|e| Control::Panic(AsError::at(e.message(), span)))?;

        // Spawn the dedicated isolate + build the producer (awaits the Init ack). Task 1.6:
        // ship the bundled program's archive (if any) so the isolate installs it before the
        // producer slice loads — a `worker fn*` that calls an imported fn resolves it from
        // memory. Read the `Rc` out before the `.await` (no borrow held across it).
        let archive_bytes = self.worker_archive_bytes().map(|b| b.to_vec());
        let driver = crate::worker::stream::StreamDriver::spawn(
            entry_name.to_string(),
            slice.entry_aso.to_vec(),
            encoded,
            encoded_shared,
            archive_bytes,
        )
        .await
        .map_err(|msg| Control::Panic(AsError::at(msg, span)))?;

        let handle = crate::coro::GeneratorHandle::new_worker(
            Box::new(driver),
            std::rc::Rc::downgrade(&self.rc()),
        );
        Ok(Value::generator(Rc::new(handle)))
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
        self.run_registered_tests_filtered(None).await
    }

    /// DX D2 Task 10 — run the registered tests, optionally PRUNING by a `--filter` on the
    /// test NAME (substring or `/regex/`). A test whose name doesn't match the filter is
    /// SKIPPED — counted in `filtered`, never run, never pass/fail. The filter is applied
    /// identically inside every isolate, so the result is independent of `--parallel` (§7).
    pub async fn run_registered_tests_filtered(
        &self,
        filter: Option<&crate::test_filter::TestFilter>,
    ) -> Result<TestSummary, Control> {
        let mut summary = TestSummary::default();
        // Clone out the registrations first so the table borrow is not held across
        // each `call_value` await.
        let tests = self.tests.borrow().clone();
        for (name, func) in tests {
            // Name-gate BEFORE running the body — a skipped test executes nothing.
            if let Some(f) = filter {
                if !f.matches(&name) {
                    summary.filtered += 1;
                    continue;
                }
            }
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
        // RT §2.3(g): the tree-walker's source module loader needs the lexer/parser/
        // checker — gated OUT of the runtime-only build (the tree-walker is never an
        // entry point in a stub; this method stays only so the `Interp` kernel compiles
        // and the linker dead-strips it). Refuse loudly before any setup work. Non-rt
        // below is byte-identical.
        #[cfg(ascript_rt)]
        {
            let _ = &src;
            return Err(Control::Panic(AsError::new(format!(
                "cannot compile module '{}': this runtime has no compiler — the module is not embedded in the bundle (rebuild with the ascript toolchain)",
                canon.display()
            ))));
        }
        #[cfg(not(ascript_rt))]
        {
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

        // Workers Spec A: record the ENTRY module's source the first time we load a
        // module so a `worker fn` dispatch can recompile it into a code slice. Only
        // the first (entry) module sets it — imported modules don't overwrite it, so
        // the slice builder sees the program the worker fn was declared in.
        if self.worker_source().is_none() {
            self.set_worker_source(&src);
        }

        let tokens =
            lexer::lex(&src).map_err(|e| Control::Panic(e.with_source(src_info.clone())))?;
        let mut program =
            parser::parse(&tokens).map_err(|e| Control::Panic(e.with_source(src_info.clone())))?;
        // ELIDE §4.3 Task 3.2: when the marking pass is enabled, run elision proofs
        // on this module's source and mark the AST in place. The flag defaults to
        // `false`; Task 4.1 decides the default-on path. The mark runs ONCE per
        // module load (the cache above ensures we never re-enter this path).
        if self.elide_mode.get() {
            let set = crate::check::infer::elision_proofs(&src);
            if !set.is_empty() {
                crate::elide_mark::mark_program(&mut program, &set);
            }
        }
        // DEFER §2.3: run the module body via `exec_program` so a top-level `defer`
        // (in the entry module OR any imported module) installs + drains its defer
        // scope. The module body runs to completion during import and its defers run
        // BEFORE the importer reads the exports. `exec_program` returns
        // `Result<Flow, Control>` with the SAME completion semantics `exec` had here:
        // `Ok(Flow::Normal)`/`Ok(Flow::Return(_))` = normal module completion (ignored),
        // a top-level `?` is folded to `Ok(Flow::Normal)`, and Panic/Exit stay `Err`
        // (propagated below). Break/Continue at module top level become a `Panic` (the
        // same "'break'/'continue' outside of a loop" the entry path produces).
        let result = self.exec_program(&program, &env).await;

        {
            *self.module_dir.borrow_mut() = prev_dir;
            *self.current_exports.borrow_mut() = prev_exports;

            if let Err(Control::Panic(e)) = result {
                return Err(Control::Panic(e.with_source(src_info)));
            }
            result?; // propagate any other control flow from the module body (Exit)
            Ok(entry)
        }
        }
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

    /// Set the directory a relative import resolves against (the importer's dir).
    /// Used by BNDL's `compile_archive` to drive [`Self::classify_specifier`] per
    /// module while walking the import graph — no code runs; this only positions the
    /// resolver's base. (The normal load path sets `module_dir` internally per module.)
    pub(crate) fn set_module_dir(&self, dir: PathBuf) {
        *self.module_dir.borrow_mut() = dir;
    }

    /// Install the CLI-resolved third-party package set (SP6 §6). Called once,
    /// before running, by `ascript run`/`test`. A subsequent bare specifier
    /// (`import "http"`) resolves through this map on BOTH engines. Replaces any
    /// previously-installed map (the REPL re-installs per session).
    pub fn set_package_resolver(&self, map: PackageMap) {
        *self.package_resolver.borrow_mut() = Some(map);
    }

    /// Classify an `import` specifier three ways, SHARED byte-identically by both
    /// engines (SP6 §6). The split, in order:
    ///
    /// 1. `std/…`              → [`SpecifierKind::Std`] (unchanged).
    /// 2. `./`, `../`, absolute → [`SpecifierKind::Relative`] (unchanged; the
    ///    path is the importer-relative file the existing loader will join).
    /// 3. otherwise → a BARE PACKAGE specifier: split off the first segment (or
    ///    `@scope/name` for a scoped package) as the key; look it up in the
    ///    resolved map. Hit with no subpath → the package `entry`; hit with a
    ///    subpath → `root.join(subpath)` (default `.as`); miss →
    ///    [`SpecifierKind::UnknownPackage`].
    ///
    /// This holds the `package_resolver` borrow ONLY for the synchronous lookup
    /// and clones the resolved [`ResolvedPkg`] out — the returned `target` is owned
    /// so the caller never holds the borrow across the loader `.await`.
    pub(crate) fn classify_specifier(&self, source: &str) -> SpecifierKind {
        if source.starts_with("std/") {
            return SpecifierKind::Std;
        }
        if source.starts_with("./")
            || source.starts_with("../")
            || Path::new(source).is_absolute()
        {
            return SpecifierKind::Relative(self.resolve_import(source));
        }

        // Bare package specifier. Compute the package key + the remaining subpath.
        let (key, subpath) = split_package_key(source);

        let resolved = self
            .package_resolver
            .borrow()
            .as_ref()
            .and_then(|m| m.get(&key).cloned());
        match resolved {
            None => SpecifierKind::UnknownPackage(key),
            Some(pkg) => {
                let target = if subpath.is_empty() {
                    pkg.entry
                } else {
                    let mut p = pkg.root.join(&subpath);
                    if p.extension().is_none() {
                        p.set_extension("as");
                    }
                    p
                };
                SpecifierKind::Package { key, target }
            }
        }
    }

    /// Resolve a `std/*` module to its `ModuleEntry` for the bytecode VM. This is
    /// the SAME `load_std_module` the tree-walker's `Stmt::Import` arm uses, so the
    /// two engines bind byte-identical export values and error identically on an
    /// unknown / feature-disabled module. The VM's `Op::Import` exec calls this;
    /// non-`std/` (file-module) imports are a compile-time deferral (V12-T4) and
    /// never reach here.
    pub(crate) fn import_std(&self, source: &str) -> Result<ModuleEntry, Control> {
        self.load_std_module(source)
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
                            // IFACE: env-aware so a `let x: Reader = …` interface
                            // annotation resolves the name to its descriptor.
                            if !self.check_type_env(&v, ty, env)? {
                                // §6.3 paranoid: escalate if this let-site was proven.
                                if let Some(e) = self.maybe_paranoid_escalate(ty, &v, value.span) {
                                    return Err(e);
                                }
                                return Err(contract_panic(ty, &v, value.span));
                            }
                        }
                        v
                    }
                    // `let x` / `let x: T` with no initializer binds nil. The type
                    // annotation is not enforced here: there is no value to check,
                    // and the language does not contract-check later assignments.
                    None => Value::nil(),
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
                let items = if let ValueKind::Array(a) = v.kind() {
                    a.borrow().clone()
                } else {
                    return Err(AsError::at(
                        format!(
                            "cannot destructure a non-array value of type {}",
                            type_name(&v)
                        ),
                        value.span,
                    )
                    .into());
                };
                for (i, name) in names.iter().enumerate() {
                    let elem = items.get(i).cloned().unwrap_or(Value::nil());
                    env.define(name, elem, *mutable).map_err(AsError::new)?;
                }
                if let Some((rest_name, _)) = rest {
                    let tail: Vec<Value> = items.iter().skip(names.len()).cloned().collect();
                    let arr = Value::array(tail);
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
                if !matches!(v.kind(), ValueKind::Object(_) | ValueKind::Instance(_)) {
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
                    match v.kind() {
                        ValueKind::Object(o) => o.get(key).unwrap_or(Value::nil()),
                        ValueKind::Instance(i) => {
                            i.borrow().get(key).unwrap_or(Value::nil())
                        }
                        _ => Value::nil(),
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
                    match v.kind() {
                        ValueKind::Object(o) => {
                            for (k, val) in o.entries() {
                                if !bound.contains(k.as_ref()) {
                                    remaining.insert(k.to_string(), val);
                                }
                            }
                        }
                        ValueKind::Instance(i) => {
                            for (k, val) in i.borrow().entries() {
                                if !bound.contains(k.as_ref()) {
                                    remaining.insert(k.to_string(), val);
                                }
                            }
                        }
                        _ => {}
                    }
                    let obj = Value::object(remaining);
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
                inclusive,
                step,
                body,
            } => {
                // RANGES FEATURE, Phase 3. `for (i in a..b)` is exclusive and
                // `for (i in a..=b)` is inclusive; an optional `step k` (sign
                // honored as direction) is resolved + validated by `resolve_step`,
                // the SHARED source of truth with the VM and value materialization.
                // When `step` is omitted the direction is inferred from the bounds:
                // a bare descending range counts DOWN (`for (i in 10..7)` → 10 9 8).
                let start_v = self.eval_expr(start, env).await?;
                let end_v = self.eval_expr(end, env).await?;
                // NUM §4: int bounds → an Int sequence (the loop var is `Int`); a
                // float bound → a float sequence. Both kinds are accepted; the
                // direction/validation math runs on f64 via the SHARED `resolve_step`.
                let (lo, hi, bounds_int) = match (start_v.as_f64(), end_v.as_f64()) {
                    (Some(a), Some(b)) => {
                        (a, b, start_v.is_int_value() && end_v.is_int_value())
                    }
                    _ => {
                        return Err(
                            AsError::at("for-range bounds must be numbers", start.span).into()
                        )
                    }
                };
                let (step_v, step_int) = match step {
                    Some(e) => {
                        let sv = self.eval_expr(e, env).await?;
                        match sv.as_f64() {
                            Some(s) => (Some(s), sv.is_int_value()),
                            None => {
                                return Err(
                                    AsError::at("for-range step must be a number", e.span).into()
                                )
                            }
                        }
                    }
                    // Omitted step is the integral `±1`, so it never forces float.
                    None => (None, true),
                };
                // Validation panic anchored at the START bound's span (matching the
                // bounds panic above and the VM's range-setup op).
                let step_n = resolve_step(lo, hi, step_v, start.span)?;
                let yields_int = bounds_int && step_int;
                let mut i = lo;
                while range_has_next(i, hi, step_n, *inclusive) {
                    let child = env.child();
                    child
                        .define(var, range_counter_value(i, yields_int), false)
                        .map_err(AsError::new)?;
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                    i += step_n;
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
                let items: Vec<Value> = match iterable.kind() {
                    ValueKind::Array(arr) => arr.borrow().clone(),
                    ValueKind::Str(s) => s
                        .chars()
                        .map(|c| Value::str(c.to_string()))
                        .collect(),
                    // SRV §3.5: iterate a frozen array/string/set zero-copy (children
                    // yield as `Shared` views or scalars).
                    ValueKind::Shared(node) => match shared_iter_values(node) {
                        Some(items) => items,
                        None => {
                            return Err(AsError::at(
                                format!("value of type {} is not iterable", type_name(&iterable)),
                                iter.span,
                            )
                            .into())
                        }
                    },
                    _ => {
                        return Err(AsError::at(
                            format!("value of type {} is not iterable", type_name(&iterable)),
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
                    None => Value::nil(),
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
                is_worker,
                span,
                name_span: fn_name_span,
                ..
            } => {
                // `worker async fn` is not a valid combination (a worker already returns
                // an awaitable future, so `async` is redundant). `worker fn*` IS valid
                // (Spec B Task 6: a streaming generator running in a dedicated isolate) —
                // its is_worker/is_generator flags route the CALL to the streaming driver.
                if *is_worker && *is_async {
                    return Err(Control::Panic(AsError::at(
                        "worker functions cannot be async (a worker already returns an awaitable future; combine worker with fn* for a streaming generator, not async)".to_string(),
                        *span,
                    )));
                }
                let func = Value::function(std::rc::Rc::new(crate::value::Function {
                    name: Some(name.clone()),
                    params: params.clone(),
                    ret: ret.clone(),
                    body: body.clone(),
                    closure: env.clone(),
                    is_async: *is_async,
                    is_generator: *is_generator,
                    is_worker: *is_worker,
                    // §6.3 paranoid: carry the fn name-token span for fn_rets lookup.
                    name_span: Some(*fn_name_span),
                }));
                env.define(name, func, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::Enum { name, variants, .. } => {
                let mut map = indexmap::IndexMap::new();
                let mut schemas = indexmap::IndexMap::new();
                for v in variants {
                    // ADT: build the per-variant schema (empty for unit/scalar-backed).
                    let schema = crate::value::VariantSchema {
                        fields: v
                            .payload
                            .iter()
                            .map(|f| (f.name.clone(), f.ty.clone()))
                            .collect(),
                    };
                    let variant = if schema.has_payload() {
                        // A payload variant interns as an unsaturated CONSTRUCTOR
                        // (`ctor: true`); calling it constructs the payload value.
                        Value::enum_variant(std::rc::Rc::new(crate::value::EnumVariant {
                            enum_name: name.clone(),
                            name: v.name.clone(),
                            value: Value::nil(),
                            payload: None,
                            ctor: true,
                        def: None,
                        }))
                    } else {
                        let backing = match &v.value {
                            Some(e) => self.eval_expr(e, env).await?,
                            None => Value::nil(),
                        };
                        Value::enum_variant(std::rc::Rc::new(crate::value::EnumVariant {
                            enum_name: name.clone(),
                            name: v.name.clone(),
                            value: backing,
                            payload: None,
                            ctor: false,
                        def: None,
                        }))
                    };
                    map.insert(v.name.clone(), variant);
                    schemas.insert(v.name.clone(), schema);
                }
                let def = Value::enum_(std::rc::Rc::new(crate::value::EnumDef {
                    name: name.clone(),
                    variants: map,
                    variant_schemas: schemas,
                }));
                env.define(name, def, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::Class {
                name,
                superclass,
                fields,
                methods,
                is_worker,
                ..
            } => {
                let parent = match superclass {
                    Some(sup_name) => match env.get(sup_name).map(|v| v.into_kind()) {
                        Some(OwnedKind::Class(c)) => Some(c),
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
                let mut static_method_map = indexmap::IndexMap::new();
                for m in methods {
                    // `init` must be a synchronous constructor: `C()` returns an
                    // instance, not a future, so there is no caller to `await` an
                    // async constructor, and a generator constructor makes no sense.
                    // Reject `async fn init` / `fn* init` (SP1 §3) — identical message
                    // on both engines; the blessed pattern is a static async factory.
                    if !m.is_static && m.name == "init" && (m.is_async || m.is_generator) {
                        return Err(AsError::at(
                            "init must be a synchronous constructor; use a static \
                             async factory (e.g. `static async fn create()`)",
                            m.name_span,
                        )
                        .into());
                    }
                    let method = std::rc::Rc::new(crate::value::Method {
                        params: m.params.clone(),
                        ret: m.ret.clone(),
                        body: m.body.clone(),
                        is_async: m.is_async,
                        is_generator: m.is_generator,
                        is_worker: m.is_worker,
                    });
                    if m.is_static {
                        // `from` is reserved on classes (collides with the built-in
                        // typed-parse `.from`) — declaring `static fn from` is an error
                        // (SP1 §3), identical on both engines.
                        if m.name == "from" {
                            return Err(AsError::at(
                                "'from' is reserved on classes",
                                m.name_span,
                            )
                            .into());
                        }
                        static_method_map.insert(m.name.clone(), method);
                    } else {
                        method_map.insert(m.name.clone(), method);
                    }
                }
                let class = Value::class(std::rc::Rc::new(crate::value::Class {
                    name: name.clone(),
                    superclass: parent,
                    fields: field_map,
                    methods: method_map,
                    static_methods: static_method_map,
                    def_env: env.clone(),
                    is_worker: *is_worker,
                }));
                env.define(name, class, false).map_err(AsError::new)?;
                // Invalidate any verdict cached against a now-reusable class pointer.
                self.bump_iface_cache_gen();
                Ok(Flow::Normal)
            }
            Stmt::Interface {
                name,
                extends,
                methods,
                ..
            } => {
                // IFACE §4: build the descriptor holding own_methods + extends NAMES
                // only (no flatten — interfaces forward-reference as late-bound
                // module-globals; flatten happens lazily on first conformance check).
                let mut own_methods = indexmap::IndexMap::new();
                for m in methods {
                    let arity = m.params.iter().filter(|p| !p.rest).count();
                    let has_rest = m.params.iter().any(|p| p.rest);
                    own_methods.insert(
                        m.name.clone(),
                        crate::value::MethodReq { arity, has_rest },
                    );
                }
                let def = Value::interface(std::rc::Rc::new(crate::value::InterfaceDef {
                    name: name.clone(),
                    own_methods,
                    extends: extends.clone(),
                    def_env: env.clone(),
                    flat: std::cell::RefCell::new(None),
                }));
                env.define(name, def, false).map_err(AsError::new)?;
                // Invalidate any verdict cached against a now-reusable interface pointer.
                self.bump_iface_cache_gen();
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
                // SP6 §6: the SHARED three-way classifier drives both engines.
                // `Std` → the static registry; `Relative`/`Package` → the SAME
                // existing file-module loader (a package's resolved target is just
                // a file under a different root); `UnknownPackage` → a Tier-2
                // error with the message identical on both engines. The resolved
                // `target` is owned (cloned out of the resolver borrow), so the
                // borrow is never held across the loader `.await`.
                let entry = match self.classify_specifier(source) {
                    SpecifierKind::Std => self.load_std_module(source)?,
                    SpecifierKind::Relative(resolved) => self.load_module(&resolved).await?,
                    SpecifierKind::Package { target, .. } => self.load_module(&target).await?,
                    SpecifierKind::UnknownPackage(key) => {
                        return Err(AsError::new(format!(
                            "unknown package '{key}' — add it with 'ascript add'"
                        ))
                        .into());
                    }
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
                            let v = entry.env.get(name).unwrap_or(Value::nil());
                            env.define(name, v, false).map_err(AsError::new)?;
                        }
                    }
                    crate::ast::ImportNames::Namespace(alias) => {
                        let mut map = indexmap::IndexMap::new();
                        for name in entry.exports.borrow().iter() {
                            map.insert(name.clone(), entry.env.get(name).unwrap_or(Value::nil()));
                        }
                        env.define(
                            alias,
                            Value::object(map),
                            false,
                        )
                        .map_err(AsError::new)?;
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Defer { call, awaited, span } => {
                // DEFER §3.1: evaluate callee/receiver + args NOW (at the defer
                // statement), then push the captured entry onto the innermost active
                // defer list. The CALL ITSELF is deferred to body exit (LIFO).
                //
                // `call` is guaranteed `ExprKind::Call { callee, args }` by the parser.
                // Never hold a `RefCell` borrow across `.await`.
                let ExprKind::Call { callee, args, .. } = &call.kind else {
                    // Parser guarantee — structurally unreachable.
                    return Err(AsError::at("defer: internal error: not a call", *span).into());
                };
                let entry = match &callee.kind {
                    // `defer a?.m(args)` — optional-chain: nil receiver → no-op, per §3.1.
                    ExprKind::OptMember { object, name } => {
                        let recv = self.eval_expr(object, env).await?;
                        if recv == Value::nil() {
                            // Spec §3.1: nil opt-chain → whole defer is a no-op;
                            // arg expressions are NOT evaluated.
                            return Ok(Flow::Normal);
                        }
                        let call_args = self.eval_call_args(args, env).await?;
                        DeferEntry {
                            kind: DeferKind::Method {
                                recv,
                                name: name.as_str().into(),
                            },
                            args: call_args,
                            awaited: *awaited,
                            span: *span,
                        }
                    }
                    // `defer o.m(args)` — method call; receiver captured at statement.
                    ExprKind::Member { object, name } => {
                        let recv = self.eval_expr(object, env).await?;
                        let call_args = self.eval_call_args(args, env).await?;
                        DeferEntry {
                            kind: DeferKind::Method {
                                recv,
                                name: name.as_str().into(),
                            },
                            args: call_args,
                            awaited: *awaited,
                            span: *span,
                        }
                    }
                    // `defer f(args)` — plain call; callee evaluated at statement.
                    _ => {
                        let callee_v = self.eval_expr(callee, env).await?;
                        let call_args = self.eval_call_args(args, env).await?;
                        DeferEntry {
                            kind: DeferKind::Call { callee: callee_v },
                            args: call_args,
                            awaited: *awaited,
                            span: *span,
                        }
                    }
                };
                // DEFER §5.1: append to the CURRENT activation's defer list, resolved
                // via the env chain (`env.defer_scope()` walks parents to the nearest
                // installed list). This is concurrency-sound: the `env` threaded through
                // `exec`/`eval_expr` is THIS activation's call env, so the resolved list
                // is always THIS activation's — never another concurrently-suspended
                // activation's (the bug an `Interp`-level stack's `last()` had). No
                // `.await` follows; the borrow is short-lived and dropped before return.
                if let Some(list) = env.defer_scope() {
                    list.borrow_mut().push(entry);
                    #[cfg(any(test, feature = "fuzzgen", fuzzing))]
                    crate::vm::defer_metrics::defer_metrics::ENTRIES_PUSHED
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                // If no defer list is installed (a bare env with no driver), the entry
                // is silently dropped. In practice run_body / the program/module/REPL
                // drivers always install one on the activation env first.
                Ok(Flow::Normal)
            }
        }
    }

    /// SP9 §1: native re-entry guard for the tree-walker expression evaluator. A
    /// deeply nested SOURCE expression (`((((…))))`) recurses `eval_expr→eval_expr`
    /// without passing through `run_body`, so the per-call `run_body` stack guard
    /// does NOT cover it. Grow the native stack here too — but only at a coarse
    /// checkpoint (every `STACK_CHECK_INTERVAL` nesting levels), so the cheap probe
    /// runs once per checkpoint instead of once per expression (avoids a `Box::pin`
    /// on every `eval_expr` call — the tree-walker hot path). The interval × the
    /// largest per-`eval_expr` frame stays well under `RED_ZONE`, so the guard still
    /// re-grows before any segment overflows. Inert until the stack runs low.
    pub async fn eval_expr(&self, expr: &Expr, env: &Environment) -> Result<Value, Control> {
        // A coarse checkpoint: only the levels that are a multiple of the interval
        // pay the (boxed) grow wrapper; all others go straight to the inner
        // evaluator. `expr_depth` is the live nesting counter incremented in
        // `eval_expr_inner` below.
        const STACK_CHECK_INTERVAL: u32 = 16;
        if self.expr_depth.get().is_multiple_of(STACK_CHECK_INTERVAL) {
            crate::vm::stack::grow_future(self.eval_expr_inner(expr, env)).await
        } else {
            self.eval_expr_inner(expr, env).await
        }
    }

    #[async_recursion(?Send)]
    async fn eval_expr_inner(&self, expr: &Expr, env: &Environment) -> Result<Value, Control> {
        // SP3 §B / O1: bound EXPRESSION nesting (deeply nested `((((…))))`, long
        // binary chains) on its OWN counter — NOT `call_depth`. Counting expr
        // nesting against the per-call counter would double-count each logical call
        // on the tree-walker (the call sub-expression's `eval_expr` frames are live
        // alongside the `run_body` call frame), making it trip at ~MAX/2 while the
        // VM trips at MAX — a byte-identical-oracle violation on ordinary recursion.
        // One increment per nested `eval_expr`; decremented on every exit path
        // (return / `?` / panic). A `Cell`, never held across an `.await`.
        let _expr_depth = DepthGuard::enter(&self.expr_depth, EXPR_NEST_LIMIT, expr.span)?;
        match &expr.kind {
            ExprKind::Int(i) => Ok(Value::int(*i)),
            ExprKind::Float(n) => Ok(Value::float(*n)),
            ExprKind::Str(s) => Ok(Value::str(s.as_str())),
            ExprKind::Bool(b) => Ok(Value::bool_(*b)),
            ExprKind::Nil => Ok(Value::nil()),
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
                        return if l == Value::nil() {
                            self.eval_expr(rhs, env).await
                        } else {
                            Ok(l)
                        };
                    }
                    // `x instanceof int|float|number|string|bool` (NUM §6): the RHS
                    // is a bare reserved-type-name identifier that must NOT be
                    // evaluated as a value (it is not a binding). Recognize it BEFORE
                    // evaluating the RHS and route to the subtype check. Byte-identical
                    // to the VM, which pre-resolves the name at compile time.
                    BinOp::InstanceOf => {
                        if let ExprKind::Ident(name) = &rhs.kind {
                            if crate::interp::is_reserved_instanceof_type_name(name) {
                                let l = self.eval_expr(lhs, env).await?;
                                let yes = crate::interp::instanceof_reserved_type(&l, name)
                                    .unwrap_or(false);
                                return Ok(Value::bool_(yes));
                            }
                        }
                    }
                    _ => {}
                }

                let l = self.eval_expr(lhs, env).await?;
                let r = self.eval_expr(rhs, env).await?;

                // IFACE §5.2: `instanceof` routes through the shared `&self`
                // `eval_instanceof` (class → nominal `is_instance_of`; interface →
                // structural `conforms`) — the SAME path the VM takes via
                // `eval_binop_adaptive`. (Not through `apply_binop`, so its InstanceOf
                // arm is unreachable.)
                if let BinOp::InstanceOf = op {
                    return self.eval_instanceof(l, r, expr.span);
                }

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
                Ok(Value::function(std::rc::Rc::new(crate::value::Function {
                    name: None,
                    params: params.clone(),
                    ret: None,
                    body: body_stmts,
                    closure: env.clone(),
                    is_async: *is_async,
                    is_generator: *is_generator,
                    // Arrows are never `worker` (no `worker` arrow syntax).
                    is_worker: false,
                    name_span: None, // anonymous arrow: no name token
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
                            if let ValueKind::Array(a) = v.kind() {
                                values.extend(a.borrow().iter().cloned())
                            } else {
                                return Err(AsError::at(
                                    format!(
                                        "can only spread an array into an array, got {}",
                                        type_name(&v)
                                    ),
                                    x.span,
                                )
                                .into());
                            }
                        }
                    }
                }
                Ok(Value::array(values))
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
                            if let ValueKind::Object(o) = v.kind() {
                                for (k, val) in o.entries() {
                                    map.insert(k.to_string(), val);
                                }
                            } else {
                                return Err(AsError::at(
                                    format!(
                                        "can only spread an object into an object, got {}",
                                        type_name(&v)
                                    ),
                                    x.span,
                                )
                                .into());
                            }
                        }
                    }
                }
                Ok(Value::object(map))
            }
            ExprKind::Map(entries) => {
                let mut map = indexmap::IndexMap::with_capacity(entries.len());
                for entry in entries {
                    // Evaluate key then value, left-to-right.
                    let key_val = self.eval_expr(&entry.key, env).await?;
                    let key = crate::value::MapKey::from_value(&key_val).ok_or_else(|| {
                        AsError::at(
                            format!("cannot use {} as a map key", type_name(&key_val)),
                            entry.key.span,
                        )
                    })?;
                    let value = self.eval_expr(&entry.value, env).await?;
                    // Later-key-wins: an IndexMap insert overwrites the value
                    // while keeping the key's first-seen position.
                    map.insert(key, value);
                }
                Ok(Value::map(map))
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
                Ok(Value::str(out))
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
                // Drive the future to completion; a panic/propagation raised in
                // the spawned task re-surfaces here (cross-task propagation).
                // `await` on a non-future is identity (back-compat: `await 5` == 5).
                if let ValueKind::Future(f) = v.kind() {
                    f.get().await
                } else {
                    Ok(v)
                }
            }
            ExprKind::Yield(operand) => {
                let v = match operand {
                    Some(e) => self.eval_expr(e, env).await?,
                    None => Value::nil(),
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
            // RANGES FEATURE, Phase 3. The value-range path materializes an
            // `array<number>` honoring the inclusive (`..=`) boundary and an
            // optional `step k` (sign honored as direction), via the SHARED
            // `materialize_range_stepped`/`resolve_step` so it is byte-identical to
            // the VM and the for-range loop. When `step` is omitted the direction is
            // inferred from the bounds, so a bare descending range counts DOWN
            // (`10..1` → [10, 9, …, 2], `10..=1` → [10, 9, …, 1]).
            ExprKind::Range {
                start,
                end,
                inclusive,
                step,
            } => {
                let start_v = self.eval_expr(start, env).await?;
                let end_v = self.eval_expr(end, env).await?;
                let step_v = match step {
                    Some(e) => Some(self.eval_expr(e, env).await?),
                    None => None,
                };
                materialize_range_stepped(
                    &start_v,
                    &end_v,
                    *inclusive,
                    step_v.as_ref(),
                    expr.span,
                )
            }
            ExprKind::Paren(inner) => self.eval_expr(inner, env).await,
            ExprKind::Try(inner) => {
                let v = self.eval_expr(inner, env).await?;
                // Must be a 2-element Result pair [value, err].
                let arr = match v.kind() {
                    ValueKind::Array(a) if a.borrow().len() == 2 => a.clone(),
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
                if err == Value::nil() {
                    Ok(value)
                } else {
                    // Early-return [nil, err] from the enclosing function.
                    Err(Control::Propagate(make_pair(Value::nil(), err)))
                }
            }
            ExprKind::Unwrap(inner) => {
                let v = self.eval_expr(inner, env).await?;
                let arr = match v.kind() {
                    ValueKind::Array(a) if a.borrow().len() == 2 => a.clone(),
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
                if err == Value::nil() {
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
                // A value pattern compares the subject against the pattern value with the
                // SAME equality the `==` operator uses (Option-C: a value pattern is an
                // `==` test). For pure structural kinds this is the plain `==`, but for the
                // numeric tower it must apply NUM's cross-kind coercion — in particular
                // `Decimal(-1)` equals `Int(-1)` (as `decimal(-1) == -1` is `true`). Using
                // bare Rust `PartialEq` (`v == *subject`) here would treat a Decimal subject
                // as DISTINCT from an int/float pattern, diverging from both the `==`
                // operator AND the VM (which compiles a value pattern to `Op::Eq`). Route
                // through the shared `decimal_cross_eq` so all four modes agree.
                Ok(decimal_cross_eq(&v, subject, e.span)?)
            }
            Pattern::Range {
                start,
                end,
                inclusive,
                step,
            } => {
                // RANGES FEATURE, Phase 5 (strided membership, spec §3.7). A
                // non-number subject OR non-number bound is a (non-panic) mismatch,
                // exactly as before. A `step k` clause turns the test into strided
                // membership anchored at `start`, via the SHARED helpers so it is
                // byte-identical to the VM: `resolve_step` validates (a `step 0` /
                // non-finite / direction-mismatch pattern PANICS like iteration),
                // then `range_pattern_contains` tests in-bounds + on-the-stride.
                // With `step` omitted the membership degenerates to the plain
                // in-bounds test, so existing no-step patterns are UNCHANGED.
                // NUM §4: a number subject/bound is Int OR Float; a non-number is a
                // (non-panic) mismatch. The membership math is exact-on-f64.
                let n = match subject.as_f64() {
                    Some(n) => n,
                    None => return Ok(false),
                };
                let lo = match self.eval_expr(start, env).await?.as_f64() {
                    Some(x) => x,
                    None => return Ok(false),
                };
                let hi = match self.eval_expr(end, env).await?.as_f64() {
                    Some(x) => x,
                    None => return Ok(false),
                };
                // Resolve the step's `f64` (None when omitted). A non-number step
                // expression is a Tier-2 type error, mirroring iteration.
                let step_v = match step {
                    Some(s) => match self.eval_expr(s, env).await?.as_f64() {
                        Some(x) => Some(x),
                        None => return Err(AsError::at("range step must be a number", s.span).into()),
                    },
                    None => None,
                };
                // Validate the step (shared with iteration / the VM); a bad
                // EXPLICIT step PANICS here with the byte-identical message, at the
                // START bound's span (matching the for-range / value-range panics).
                // `resolve_step` is run only to surface that panic; the membership
                // helper takes the raw `Option` so a plain pattern keeps its
                // pre-existing no-stride behavior.
                if step_v.is_some() {
                    resolve_step(lo, hi, step_v, start.span)?;
                }
                Ok(range_pattern_contains(n, lo, hi, step_v, *inclusive))
            }
            Pattern::Array(pats, rest) => {
                // Snapshot the subject array (do not hold a borrow across awaits).
                let items: Vec<Value> = match subject.kind() {
                    ValueKind::Array(a) => a.borrow().iter().cloned().collect(),
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
                        Value::array(remainder),
                    ));
                }
                Ok(true)
            }
            Pattern::Object(entries, rest) => {
                // Snapshot the subject's fields (Object or Instance).
                let fields: indexmap::IndexMap<String, Value> = match subject.kind() {
                    ValueKind::Object(o) => o
                        .entries()
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v))
                        .collect(),
                    ValueKind::Instance(i) => i.borrow().to_index_map(),
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
                        Value::object(remaining),
                    ));
                }
                Ok(true)
            }
            Pattern::Variant {
                enum_name,
                variant,
                fields,
            } => {
                self.match_variant_pattern(enum_name, variant, fields, subject, bindings, env)
                    .await
            }
        }
    }

    /// ADT: match a `Pattern::Variant` against `subject`. Tag-test (the subject must
    /// be an `EnumVariant` of `variant`, and of `enum_name` when qualified), then
    /// destructure the payload positionally (by index) or by named field. A subject
    /// that is not a matching variant is a (non-panic) mismatch. Byte-identical to
    /// the VM's `compile_variant_pattern` lowering.
    #[async_recursion(?Send)]
    async fn match_variant_pattern(
        &self,
        enum_name: &Option<Rc<str>>,
        variant: &Rc<str>,
        fields: &crate::ast::VariantPatFields,
        subject: &Value,
        bindings: &mut Vec<(Rc<str>, Value)>,
        env: &Environment,
    ) -> Result<bool, Control> {
        use crate::ast::VariantPatFields;
        // The subject must be an EnumVariant whose name == `variant` (and whose
        // `enum_name` == `enum_name` when the pattern is qualified). A constructor
        // (`ctor: true`) is not a constructed value and never matches.
        let ev = match subject.kind() {
            ValueKind::EnumVariant(ev) if !ev.ctor => ev,
            _ => return Ok(false),
        };
        if ev.name.as_str() != variant.as_ref() {
            return Ok(false);
        }
        if let Some(en) = enum_name {
            if ev.enum_name.as_str() != en.as_ref() {
                return Ok(false);
            }
        }
        match fields {
            VariantPatFields::Positional(pats) => {
                // Snapshot the payload's values IN DECLARATION ORDER (do not hold a
                // borrow across awaits). A positional pattern binds by position against
                // EITHER a positional payload OR a named one (named values in
                // insertion = declaration order) — so `Circle(r)` binds the single
                // `radius` field, `Rect(w, h)` binds `w`/`h` by position (ADT §3.3). A
                // unit payload (`None`) cannot match.
                let items: Vec<Value> = match &ev.payload {
                    Some(crate::value::Payload::Positional(a)) => {
                        a.borrow().iter().cloned().collect()
                    }
                    Some(crate::value::Payload::Named(o)) => {
                        o.entries().into_iter().map(|(_, v)| v).collect()
                    }
                    None => return Ok(false),
                };
                if items.len() != pats.len() {
                    return Ok(false);
                }
                for (p, item) in pats.iter().zip(items.iter()) {
                    if !self.match_pattern(p, item, bindings, env).await? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            VariantPatFields::Named(entries) => {
                // Snapshot the named payload's fields. A non-named payload cannot
                // match a named pattern.
                let payload_obj = match &ev.payload {
                    Some(crate::value::Payload::Named(o)) => o.clone(),
                    _ => return Ok(false),
                };
                for (key, subpat) in entries {
                    let field = match payload_obj.get(key.as_ref()) {
                        Some(v) => v,
                        None => return Ok(false),
                    };
                    match subpat {
                        // `Rect(w)` shorthand → bind field `w` (mirrors object `{key}`).
                        None => bindings.push((key.clone(), field)),
                        // `Rect(w: ww)` → match field `w` against sub-pattern `ww`.
                        Some(p) => {
                            if !self.match_pattern(p, &field, bindings, env).await? {
                                return Ok(false);
                            }
                        }
                    }
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
                if sc || obj == Value::nil() {
                    return Ok((Value::nil(), true));
                }
                Ok((self.read_member(&obj, name, object.span)?, false))
            }
            ExprKind::Member { object, name } => {
                let (obj, sc) = self.eval_chain(object, env).await?;
                if sc {
                    return Ok((Value::nil(), true));
                }
                Ok((self.read_member(&obj, name, object.span)?, false))
            }
            ExprKind::Index { object, index } => {
                let (obj, sc) = self.eval_chain(object, env).await?;
                if sc {
                    return Ok((Value::nil(), true));
                }
                let idx = self.eval_expr(index, env).await?;
                // Shared with the bytecode VM (`Op::GetIndex`) so the two engines
                // cannot drift on index-read semantics or panic messages.
                let v = index_get(&obj, &idx, object.span, expr.span)?;
                Ok((v, false))
            }
            ExprKind::Call { callee, args, elide_args } => {
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
                        return Ok((Value::nil(), true));
                    }
                    // Call-position hooks (schema fluent-chain, workflow `ctx.<m>()`,
                    // `WorkerClass.spawn`, frozen `Value::shared` reads, actor-handle
                    // dispatch). The hook ENUMERATION lives in `member_call_is_hook` and
                    // the DISPATCH in `call_method_recv` — the SAME routine the deferred
                    // `DeferKind::Method` drain re-enters (spec §3.1), so the two paths
                    // can never drift. All hooks take POSITIONAL args (no hook accepts
                    // named args), evaluated receiver-first then args — matching the
                    // prior per-hook `eval_call_args` ordering exactly.
                    if self.member_call_is_hook(&recv, name) {
                        let values = self.eval_call_args(args, env).await?;
                        let v = self.call_method_recv(recv, name, values, expr.span).await?;
                        return Ok((v, false));
                    }
                    // Fallback — byte-for-byte with the prior
                    // `eval_chain(callee) → eval_args → call_value` path: read
                    // the member FIRST (which can error — nil receiver, bad
                    // enum-variant prop, …), and only THEN evaluate the args, so
                    // a member-read error preempts arg evaluation / side effects.
                    // (This is the only path that supports NAMED args.)
                    // ELIDE §4.3: member calls are ineligible for elision (elide_args
                    // is always false here by construction — the marking pass only
                    // marks plain non-member Call nodes); pass false defensively.
                    let callee_v = self.read_member(&recv, name, object.span)?;
                    let (values, names) = self.eval_call_args_named(args, env).await?;
                    let v = self.call_value_named(callee_v, values, names, expr.span, false).await;
                    return Ok((v?, false));
                }

                let (callee_v, sc) = self.eval_chain(callee, env).await?;
                if sc {
                    return Ok((Value::nil(), true));
                }
                let (values, names) = self.eval_call_args_named(args, env).await?;
                // ELIDE §4.3: thread the elide_args flag from the AST node.
                // When true (set by the marking pass for a statically-proven call),
                // per-param type-contract checks are skipped inside call_value_elided
                // → call_function → run_body → check_call_args.
                let v = self.call_value_named(callee_v, values, names, expr.span, *elide_args).await;
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
                    if let ValueKind::Array(arr) = v.kind() {
                        values.extend(arr.borrow().iter().cloned())
                    } else {
                        return Err(AsError::at(
                            format!(
                                "can only spread an array as call arguments, got {}",
                                type_name(&v)
                            ),
                            x.span,
                        )
                        .into());
                    }
                }
                // A named argument is only meaningful in an enum-variant constructor
                // call (the `call_value` general paths handle it via
                // `eval_call_args_named`). On the schema / workflow-ctx / actor /
                // worker-spawn native-dispatch paths it has no meaning → clean error.
                crate::ast::CallArg::Named { name, value } => {
                    return Err(AsError::at(
                        format!("unexpected named argument '{}:' in this call", name),
                        value.span,
                    )
                    .into());
                }
            }
        }
        Ok(values)
    }

    /// ADT §3.2: evaluate call args, returning both the flattened values and a
    /// parallel list of per-value names (`None` = positional / spread element,
    /// `Some` = a `name: expr` named argument). Used by the general `call_value`
    /// dispatch paths so an enum-variant constructor can resolve named fields. A
    /// spread expands to multiple positional (`None`) entries, exactly as the
    /// positional `eval_call_args` does.
    #[async_recursion(?Send)]
    async fn eval_call_args_named(
        &self,
        args: &[crate::ast::CallArg],
        env: &Environment,
    ) -> Result<(Vec<Value>, Vec<Option<std::rc::Rc<str>>>), Control> {
        let mut values = Vec::new();
        let mut names: Vec<Option<std::rc::Rc<str>>> = Vec::new();
        for a in args {
            match a {
                crate::ast::CallArg::Pos(x) => {
                    values.push(self.eval_expr(x, env).await?);
                    names.push(None);
                }
                crate::ast::CallArg::Spread(x) => {
                    let v = self.eval_expr(x, env).await?;
                    if let ValueKind::Array(arr) = v.kind() {
                        for item in arr.borrow().iter() {
                            values.push(item.clone());
                            names.push(None);
                        }
                    } else {
                        return Err(AsError::at(
                            format!(
                                "can only spread an array as call arguments, got {}",
                                type_name(&v)
                            ),
                            x.span,
                        )
                        .into());
                    }
                }
                crate::ast::CallArg::Named { name, value } => {
                    values.push(self.eval_expr(value, env).await?);
                    names.push(Some(name.clone()));
                }
            }
        }
        Ok((values, names))
    }

    /// ADT §3.2: dispatch a call whose arguments MAY include named args. If any arg
    /// is named, the only valid callee is an enum-variant constructor (named-field
    /// construction); otherwise it is a clean Tier-2 error. With no named args this
    /// is byte-identical to the plain `call_value` path.
    async fn call_value_named(
        &self,
        callee: Value,
        values: Vec<Value>,
        names: Vec<Option<std::rc::Rc<str>>>,
        span: Span,
        // ELIDE §4.3: threaded from the `ExprKind::Call` evaluator. Named-arg calls
        // are ineligible for elision (the eligibility predicate excludes them), so
        // when any name is Some this is effectively always false. Positional calls
        // thread it through to `call_value_elided`.
        elide_contracts: bool,
    ) -> Result<Value, Control> {
        if names.iter().any(|n| n.is_some()) {
            if let ValueKind::EnumVariant(ev) = callee.kind() {
                self.construct_variant_args(ev, values, &names, span).await
            } else {
                Err(AsError::at(
                    format!(
                        "named arguments are only valid for enum-variant construction, \
                         not for {}",
                        type_name(&callee)
                    ),
                    span,
                )
                .into())
            }
        } else {
            self.call_value_elided(callee, values, span, elide_contracts).await
        }
    }

    // pub(crate): shared with the bytecode VM (`Op::GetProp`/`Op::GetPropOpt`)
    // so member-access semantics (fields, methods→BoundMethod, enum variants,
    // native handles, nil-receiver errors) have ONE implementation.
    pub(crate) fn read_member(
        &self,
        obj: &Value,
        name: &str,
        span: Span,
    ) -> Result<Value, AsError> {
        match obj.kind() {
            ValueKind::Object(map) => Ok(map.get(name).unwrap_or(Value::nil())),
            ValueKind::Enum(e) => {
                let variant = e.variants.get(name).cloned().ok_or_else(|| {
                    AsError::at(format!("enum {} has no variant '{}'", e.name, name), span)
                })?;
                // ADT: reading a PAYLOAD-variant member yields a CONSTRUCTOR carrying a
                // back-reference to this `EnumDef` (so a first-class `let mk =
                // Shape.Circle` validates on call). The interned map entry has
                // `def: None` (no `Rc` cycle); we clone it here with `def: Some`.
                match variant.kind() {
                    ValueKind::EnumVariant(ev) if ev.ctor => Ok(Value::enum_variant(
                        std::rc::Rc::new(crate::value::EnumVariant {
                            enum_name: ev.enum_name.clone(),
                            name: ev.name.clone(),
                            value: Value::nil(),
                            payload: None,
                            ctor: true,
                            def: Some(e.clone()),
                        }),
                    )),
                    _ => Ok(variant),
                }
            }
            ValueKind::EnumVariant(v) => match name {
                "name" => Ok(Value::str(v.name.as_str())),
                // ADT §3.4: `.value` of a unit/scalar variant is the backing scalar
                // (unchanged); of a PAYLOAD variant it is the payload-as-data — the
                // STORED `Cc` Array (positional) / Object (named) handle (stable
                // identity, no per-access allocation).
                "value" => match &v.payload {
                    None => Ok(v.value.clone()),
                    Some(crate::value::Payload::Positional(a)) => Ok(Value::array_cell(a.clone())),
                    Some(crate::value::Payload::Named(o)) => Ok(Value::object_cell(o.clone())),
                },
                // ADT §3.4: named-payload field-access sugar — `c.radius` reads the
                // named field directly off the payload Object.
                other => {
                    if let Some(crate::value::Payload::Named(o)) = &v.payload {
                        if let Some(fv) = o.get(other) {
                            return Ok(fv);
                        }
                    }
                    Err(AsError::at(
                        format!("enum variant has no property '{}'", other),
                        span,
                    ))
                }
            },
            ValueKind::Instance(inst) => {
                let b = inst.borrow();
                if let Some(v) = b.get(name) {
                    return Ok(v);
                }
                match crate::value::find_method(&b.class, name) {
                    Some((method, def_class)) => Ok(Value::bound_method(std::rc::Rc::new(
                        crate::value::BoundMethod {
                            receiver: obj.clone(),
                            method,
                            defining_class: def_class,
                            name: name.to_string(),
                        },
                    ))),
                    None => Ok(Value::nil()),
                }
            }
            ValueKind::Super(s) => match &s.start {
                Some(start) => match crate::value::find_method(start, name) {
                    Some((method, def_class)) => Ok(Value::bound_method(std::rc::Rc::new(
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
            ValueKind::Native(n) => {
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
                    return Ok(Value::str(id));
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
                Ok(Value::native_method(std::rc::Rc::new(
                    crate::value::NativeMethod {
                        receiver: n.clone(),
                        method: name.to_string(),
                    },
                )))
            }
            ValueKind::Generator(g) => match name {
                // `gen.next` / `gen.close` are bound generator methods.
                "next" => Ok(Value::generator_method(Rc::new(
                    crate::value::GeneratorMethodData {
                        handle: g.clone(),
                        name: "next",
                    },
                ))),
                "close" => Ok(Value::generator_method(Rc::new(
                    crate::value::GeneratorMethodData {
                        handle: g.clone(),
                        name: "close",
                    },
                ))),
                other => Err(AsError::at(
                    format!("generator has no property '{}' (try 'next')", other),
                    span,
                )),
            },
            // Class-level member read (`C.name`), generalized for SP1 §3 static
            // methods: (1) a user `static_methods` entry walking the superclass
            // chain → a bound static callable, ELSE (2) the built-in `from`, ELSE
            // (3) "class X has no static member 'name'". `ClassMethod` carries the
            // owned name; `call_value` dispatches it.
            ValueKind::Class(c) => {
                if crate::value::find_static_method(c, name).is_some() || name == "from" {
                    Ok(Value::class_method(Rc::new(crate::value::ClassMethodData {
                        class: c.clone(),
                        name: name.into(),
                    })))
                } else {
                    Err(AsError::at(
                        format!("class {} has no static member '{}'", c.name, name),
                        span,
                    ))
                }
            }
            // SRV §3.5: a frozen `Shared` reads a named field of a frozen
            // object/instance → the child (as a `Shared` view or scalar); a missing
            // field → nil (matching `Object`). A frozen regex exposes `.source`.
            ValueKind::Shared(node) => shared_read_member(node, name, span),
            ValueKind::Nil => Err(AsError::at(
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
        self.call_value_elided(callee, args, span, false).await
    }

    /// ELIDE §4.3: the real body of `call_value`. When `elide_contracts` is true,
    /// per-param type-contract checks are skipped inside the plain-`Function` branch
    /// (the only branch a proven call site can reach — worker/generator/async paths are
    /// excluded by the ELIDE eligibility predicate). Every other branch ignores the flag
    /// (passes `false` onward) as a defensive measure: a stray `true` from a future
    /// marking-pass bug can never skip a check that matters outside the plain-fn path.
    ///
    /// Arity errors, rest collection, and default evaluation are NEVER skipped —
    /// only per-param type-contract checks.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_value_elided(
        &self,
        callee: Value,
        args: Vec<Value>,
        span: Span,
        elide_contracts: bool,
    ) -> Result<Value, Control> {
        match callee.into_kind() {
            // A VM closure (`native → VM` bridge): a native higher-order stdlib
            // function (e.g. `array.map`, a sort comparator, `recover`) is calling
            // a user callback that the VM produced. Re-enter the VM to run it on a
            // fresh Fiber. Upgrade the registered `vm` weak to an owned `Rc<Vm>`
            // FIRST so no `RefCell` borrow is held across the await. A
            // `Value::closure` can only exist if the VM created it, so the VM is
            // always registered here; a missing VM is a wiring bug (clear panic,
            // not UB).
            // Defensive: Closure → VM path, elide_contracts ignored (VM handles it
            // via Op::CallElided; the tree-walker never produces a Closure callee).
            OwnedKind::Closure(c) => {
                let vm = self
                    .vm()
                    .expect("VM not registered for closure call (Interp::set_vm not called)");
                vm.call_value(Value::closure(c), args, span).await
            }
            // Defensive: builtins, constructors, bound methods, native methods,
            // generator methods, enum constructors, class static methods — none of
            // these are plain `Function` callees, so elide_contracts is always false
            // for them regardless of the flag (the ELIDE eligibility predicate only
            // proves plain non-async/non-generator/non-worker fn calls).
            OwnedKind::Builtin(name) => self.call_builtin(&name, &args, span).await,
            OwnedKind::Function(func) => {
                self.call_function(func, args, span, elide_contracts).await
            }
            OwnedKind::Class(class) => self.construct(class, args, span).await,
            OwnedKind::BoundMethod(bm) => self.invoke_method(&bm, args, span).await,
            OwnedKind::NativeMethod(m) => self.call_native_method(m, args, span).await,
            OwnedKind::GeneratorMethod(gm) => {
                self.call_generator_method(&gm.handle, gm.name, args, span)
                    .await
            }
            // ADT: calling a payload-variant CONSTRUCTOR (`Shape.Circle(2.0)`)
            // validates arity + field types and produces a constructed variant.
            // Calling a UNIT variant is an error.
            OwnedKind::EnumVariant(ev) => self.construct_variant(&ev, args, span).await,
            OwnedKind::ClassMethod(cm) => {
                let (c, name) = (&cm.class, &cm.name);
                // A user `static fn` (SP1 §3) takes precedence over the built-in
                // `from` only if it exists; we resolved the name at read time, but
                // re-resolve here so the carrier is self-contained. A static name
                // that shadows `from` is impossible — `static fn from` is rejected.
                if let Some((method, defining)) = crate::value::find_static_method(c, name) {
                    self.call_static_method(method, defining, args, span, name)
                        .await
                } else if &**name == "from" {
                    let obj = args.first().cloned().unwrap_or(Value::nil());
                    let strict = matches!(args.get(1).map(|v| v.kind()), Some(ValueKind::Bool(true)));
                    self.validate_into(c, &obj, strict, "", span)
                        .await
                        .map_err(Control::from)
                } else {
                    Err(AsError::at(
                        format!("class {} has no static member '{}'", c.name, name),
                        span,
                    )
                    .into())
                }
            }
            _ => Err(AsError::at("value is not callable", span).into()),
        }
    }

    /// ADT: validate + construct a payload-variant call (`Shape.Circle(2.0)`). The
    /// callee `ev` must be a CONSTRUCTOR (`ctor: true`, carrying its `EnumDef` via
    /// `def`). Validates arity, coerces + type-checks each arg against the declared
    /// field type (the SAME `coerce_field`/`check_type` path classes use), then builds
    /// a constructed `EnumVariant` (positional → `Cc<ArrayCell>`, named →
    /// `Cc<ObjectCell>`). A non-constructor (unit) variant call is an error.
    pub(crate) async fn construct_variant(
        &self,
        ev: &std::rc::Rc<crate::value::EnumVariant>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        // Positional-only entry (the generic `call_value` dispatch, which has no
        // arg names): every argument is positional.
        let arg_names: Vec<Option<std::rc::Rc<str>>> = vec![None; args.len()];
        self.construct_variant_args(ev, args, &arg_names, span).await
    }

    /// ADT §3.2: the full variant-construction validator, taking a parallel list of
    /// per-argument names (`None` = positional arg, `Some` = `name: expr`). It
    /// enforces the spec's named/positional rules:
    ///
    /// - A POSITIONAL variant (`Pair(int, int)`) accepts only positional args; a
    ///   named arg on it is an error.
    /// - A NAMED variant: named args (`Rect(w: 3.0, h: 4.0)`) construct it
    ///   order-independently (each declared field must be supplied exactly once —
    ///   missing / duplicate / unknown → a clean Tier-2 panic). A POSITIONAL call of
    ///   a named variant is accepted ONLY for the single-field convenience
    ///   (`Circle(2.0)`); a multi-field named variant called positionally is the
    ///   spec error `requires named fields (w:, h:)`.
    /// - Mixing named + positional args in one call is rejected.
    ///
    /// Each resolved field is type-checked against its declared type (the same
    /// `check_type` contract classes/functions use) in declaration order.
    #[async_recursion(?Send)]
    pub(crate) async fn construct_variant_args(
        &self,
        ev: &std::rc::Rc<crate::value::EnumVariant>,
        args: Vec<Value>,
        arg_names: &[Option<std::rc::Rc<str>>],
        span: Span,
    ) -> Result<Value, Control> {
        debug_assert_eq!(args.len(), arg_names.len());
        // A unit / constructed (already-saturated) variant is not callable.
        if !ev.ctor {
            return Err(AsError::at(
                format!(
                    "{}.{} is a unit variant and takes no payload",
                    ev.enum_name, ev.name
                ),
                span,
            )
            .into());
        }
        // The constructor must carry its owning `EnumDef` (populated on the read path).
        let def = ev.def.as_ref().ok_or_else(|| {
            AsError::at(
                format!(
                    "internal: variant constructor {}.{} has no schema (compiler invariant)",
                    ev.enum_name, ev.name
                ),
                span,
            )
        })?;
        let schema = def.variant_schemas.get(ev.name.as_str()).ok_or_else(|| {
            AsError::at(
                format!(
                    "internal: enum {} has no schema for variant '{}'",
                    ev.enum_name, ev.name
                ),
                span,
            )
        })?;

        let has_named = arg_names.iter().any(|n| n.is_some());
        let all_named = arg_names.iter().all(|n| n.is_some());

        // Reorder the supplied args into DECLARATION order. `ordered[i]` is the value
        // for `schema.fields[i]`. For named args we resolve by field name (so call
        // order is irrelevant); for positional args we keep the call order.
        let ordered: Vec<Value> = if has_named {
            // A named arg is only valid on a NAMED variant.
            if !schema.is_named() {
                return Err(AsError::at(
                    format!(
                        "{}.{} is a positional variant and takes positional arguments, \
                         not named fields",
                        ev.enum_name, ev.name
                    ),
                    span,
                )
                .into());
            }
            // Mixing named + positional args in one call is rejected.
            if !all_named {
                return Err(AsError::at(
                    format!(
                        "{}.{}: arguments must be all named or all positional, not mixed",
                        ev.enum_name, ev.name
                    ),
                    span,
                )
                .into());
            }
            // Map each declared field name → its supplied value, detecting duplicate
            // and unknown fields. Build in declaration order; a missing field errors.
            let mut supplied: indexmap::IndexMap<std::rc::Rc<str>, Value> =
                indexmap::IndexMap::with_capacity(args.len());
            for (name_opt, val) in arg_names.iter().zip(args.into_iter()) {
                // `all_named` guarantees `Some`.
                let Some(n) = name_opt else { continue };
                // Unknown field?
                let known = schema
                    .fields
                    .iter()
                    .any(|(fname, _)| fname.as_deref() == Some(n.as_ref()));
                if !known {
                    return Err(AsError::at(
                        format!("{}.{} has no field '{}'", ev.enum_name, ev.name, n),
                        span,
                    )
                    .into());
                }
                if supplied.insert(n.clone(), val).is_some() {
                    return Err(AsError::at(
                        format!("{}.{}: duplicate field '{}'", ev.enum_name, ev.name, n),
                        span,
                    )
                    .into());
                }
            }
            // Pull each declared field's value in order; a missing field errors.
            let mut ordered = Vec::with_capacity(schema.fields.len());
            for (fname, _) in &schema.fields {
                // A named variant's fields all have `Some` names (parse invariant).
                let Some(fname) = fname else {
                    return Err(AsError::at(
                        format!(
                            "internal: named variant {}.{} has a positional field",
                            ev.enum_name, ev.name
                        ),
                        span,
                    )
                    .into());
                };
                match supplied.shift_remove(fname.as_ref()) {
                    Some(v) => ordered.push(v),
                    None => {
                        return Err(AsError::at(
                            format!(
                                "{}.{} is missing field '{}'",
                                ev.enum_name, ev.name, fname
                            ),
                            span,
                        )
                        .into());
                    }
                }
            }
            ordered
        } else {
            // Fully positional call. A multi-field NAMED variant requires named args.
            if schema.is_named() && schema.fields.len() > 1 {
                let field_list = schema
                    .fields
                    .iter()
                    .filter_map(|(n, _)| n.as_ref().map(|n| format!("{}:", n)))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(AsError::at(
                    format!(
                        "{}.{} requires named fields ({})",
                        ev.enum_name, ev.name, field_list
                    ),
                    span,
                )
                .into());
            }
            // Arity check (positional).
            if args.len() != schema.fields.len() {
                return Err(AsError::at(
                    format!(
                        "{}.{} expects {} field{}, got {}",
                        ev.enum_name,
                        ev.name,
                        schema.fields.len(),
                        if schema.fields.len() == 1 { "" } else { "s" },
                        args.len()
                    ),
                    span,
                )
                .into());
            }
            args
        };

        // Type-check each field against its declared type in declaration order, the
        // SAME `check_type` contract classes/functions use (handles scalars, `T?`,
        // nested containers `array<T>`/`map<K,V>`, and enum-typed recursive fields).
        // A mismatch is the byte-identical recoverable field-path panic. (v1 does not
        // auto-coerce a raw Object into a class-typed payload field — pattern
        // destructuring is the primary path; field types are validated, not rewritten.)
        let mut coerced: Vec<Value> = Vec::with_capacity(ordered.len());
        for ((fname, fty), arg) in schema.fields.iter().zip(ordered.into_iter()) {
            let field_label = match fname {
                Some(n) => format!("{}.{}.{}", ev.enum_name, ev.name, n),
                None => format!("{}.{}", ev.enum_name, ev.name),
            };
            if !check_type(&arg, fty) {
                return Err(AsError::at(
                    format!("{}: expected {}, got {}", field_label, fty, type_name(&arg)),
                    span,
                )
                .into());
            }
            coerced.push(arg);
        }
        // Build the payload: named → an Object keyed by field name; positional → an
        // Array. Both behind a `Cc` (the cycle-collected payload container).
        let payload = if schema.is_named() {
            let mut map = indexmap::IndexMap::new();
            for ((fname, _), val) in schema.fields.iter().zip(coerced.into_iter()) {
                // `is_named()` guarantees every field name is `Some`.
                if let Some(n) = fname {
                    map.insert(n.to_string(), val);
                }
            }
            crate::value::Payload::Named(crate::value::ObjectCell::new(map))
        } else {
            crate::value::Payload::Positional(crate::value::ArrayCell::new(coerced))
        };
        Ok(Value::enum_variant(std::rc::Rc::new(
            crate::value::EnumVariant {
                enum_name: ev.enum_name.clone(),
                name: ev.name.clone(),
                value: Value::nil(),
                payload: Some(payload),
                ctor: false,
                def: None,
            },
        )))
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
                let input = args.into_iter().next().unwrap_or(Value::nil());
                // resume drives the body to its next yield; Err surfaces a body
                // panic to the consumer, None is the done sentinel (→ nil).
                match g.resume(input).await? {
                    Some(v) => Ok(v),
                    None => Ok(Value::nil()),
                }
            }
            "close" => {
                // Drop the body future: no further values; `next` now returns nil.
                g.close();
                Ok(Value::nil())
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
        // FFI §4 (BLOCKER 3): re-check the capability governing this handle BEFORE
        // operating it, so a `caps.drop` HOLDS for handles opened before the drop
        // (e.g. `socket.read()`/`listener.accept()` denied after `drop("net")`).
        // Gate-12: a single `Copy` bitset compare on the default all-granted path —
        // zero per-op cost, byte-identical. The denial uses the same recoverable
        // Tier-2 panic the central gate raises (naming the capability). A handle whose
        // kind maps to NO cap (a pure in-memory native) stays ungated.
        let cap_bits = self.caps_bits(); // Copy snapshot — no borrow across the awaits below.
        if !cap_bits.all_granted() {
            if let Some(cap) = m.receiver.kind.governing_cap() {
                self.require_cap(cap, m.receiver.kind.type_name(), &m.method, &args, span)?;
            }
        }
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
        #[cfg(feature = "postgres")]
        {
            if matches!(m.receiver.kind, crate::value::NativeKind::PostgresConnection) {
                return self.call_postgres_method(&m, args, span).await;
            }
        }
        #[cfg(feature = "redis")]
        {
            if matches!(m.receiver.kind, crate::value::NativeKind::RedisConnection) {
                return self.call_redis_method(&m, args, span).await;
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
        #[cfg(feature = "ai")]
        {
            use crate::value::NativeKind::*;
            if matches!(m.receiver.kind, AiProvider | AiModel | AiStream | AiTextStream | AiTool) {
                return crate::stdlib::ai::call_method(self, &m, args, span).await;
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
            if matches!(m.receiver.kind, Lru) {
                return self.call_lru_method(&m, &args, span);
            }
            if matches!(m.receiver.kind, Events) {
                return self.call_events_method(&m, args, span).await;
            }
        }
        #[cfg(feature = "resilience")]
        {
            if matches!(m.receiver.kind, crate::value::NativeKind::Resilience) {
                return self.call_resilience_native_method(&m, args, span).await;
            }
        }
        #[cfg(feature = "telemetry")]
        {
            use crate::value::NativeKind::*;
            if matches!(
                m.receiver.kind,
                TelemetrySpan | TelemetryInstrument | TelemetryNoop
            ) {
                return crate::stdlib::telemetry::call_method(
                    self,
                    &m.receiver,
                    &m.method,
                    args,
                    span,
                )
                .await;
            }
        }
        #[cfg(feature = "ffi")]
        {
            use crate::value::NativeKind::*;
            // FFI handle methods. The `ffi` capability already re-checked above via
            // `governing_cap` (all three FFI kinds → Cap::Ffi), so operating an open
            // handle is denied after `caps.drop("ffi")`.
            match m.receiver.kind {
                ForeignLib if m.method == "symbol" => {
                    return self.ffi_lib_symbol(m.receiver.id, &args, span);
                }
                ForeignSymbol if m.method == "call" => {
                    return self.ffi_symbol_call(m.receiver.id, &args, span);
                }
                ForeignPtr if m.method == "read_cstr" => {
                    return self.ffi_read_cstr_ptr(m.receiver.id, span);
                }
                ForeignLib | ForeignSymbol | ForeignPtr => {
                    return Err(AsError::at(
                        format!(
                            "{} handle has no method '{}'",
                            m.receiver.kind.type_name(),
                            m.method
                        ),
                        span,
                    )
                    .into());
                }
                _ => {}
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
        match iterable.kind() {
            ValueKind::Generator(g) => {
                loop {
                    // resume drives the body to its next yield; Err surfaces a body
                    // panic, None ends iteration.
                    let item = match g.resume(Value::nil()).await? {
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
            ValueKind::Native(n) => {
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
                    let bound = Value::native_method(std::rc::Rc::new(crate::value::NativeMethod {
                        receiver: n.clone(),
                        method: method.to_string(),
                    }));
                    let pair = self.call_value(bound, Vec::new(), span).await?;
                    // The recv/next contract returns a `[value, err]` pair.
                    let (value, err) = match pair.kind() {
                        ValueKind::Array(a) if a.borrow().len() == 2 => {
                            let b = a.borrow();
                            (b[0].clone(), b[1].clone())
                        }
                        // Defensive: a non-pair return ends iteration.
                        _ => break,
                    };
                    if err != Value::nil() {
                        // Surface a stream error as a Tier-2 panic at the loop site.
                        let msg = error_message(&err);
                        return Err(
                            AsError::at(format!("for await stream error: {}", msg), span).into(),
                        );
                    }
                    if value == Value::nil() {
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
            _ => Err(AsError::at(
                format!("value of type {} is not async-iterable", type_name(&iterable)),
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
        // ELIDE §4.3: when true, per-param type-contract checks are skipped. Arity,
        // defaults, and rest collection are NEVER skipped. Method-invoke callers
        // always pass false (methods are not elidable in v1 — the marking pass only
        // marks plain non-member Call nodes).
        elide_contracts: bool,
    ) -> Result<Value, Control> {
        // SP3 §B: `run_body` is the single funnel EVERY script call (function,
        // method, generator-step body, async body) passes through — and it is the
        // actual native-stack deepener (`#[async_recursion]`). Guarding it here
        // bounds the recursion at one logical-call increment per call (the VM's
        // matching unit is one CallFrame push). Acquired BEFORE binding args / exec
        // so the over-limit panic anchors at the call span; decremented on every
        // exit path by the guard's `Drop`.
        let _depth = DepthGuard::enter(&self.call_depth, MAX_CALL_DEPTH, span)?;
        // SP3 §B / O1: a call boundary opens a FRESH expression-nesting context
        // (mirroring the VM's per-body `compile_expr` depth reset), so a caller's
        // live `eval_expr` frames do NOT count against the callee's body nesting.
        // This keeps deep recursion bounded SOLELY by `call_depth` on both engines.
        let _expr_reset = ExprDepthReset::enter(&self.expr_depth);
        let BodySpec { params, ret, body, name_span: fn_name_span } = spec;
        // Arity + parameter contracts + rest collection. Shared verbatim with the
        // bytecode VM (`src/vm/run.rs` CALL) so both engines bind args identically.
        // IFACE: thread the callee frame env so a `Type::Named` param contract resolves
        // env-aware (interface → structural conforms; class → nominal).
        // ELIDE §4.3: elide_contracts is threaded through from call_value_elided.
        let bound = check_call_args(params, args, span, what, Some(self), Some(call_env), elide_contracts)?;
        let defaults = bound.defaults.clone();
        // Bind the SUPPLIED params (and the rest array) — but NOT the omitted
        // defaulted positions, whose `bound.values` entries are placeholders. They
        // are bound below, AFTER their default is evaluated, so each default sees
        // the earlier params and `define` is called exactly once per name.
        for (i, (p, a)) in params.iter().zip(bound.values.into_iter()).enumerate() {
            if defaults.contains(&i) {
                continue;
            }
            call_env.define(&p.name, a, true).map_err(AsError::new)?;
        }
        // Evaluate omitted trailing defaults LEFT-TO-RIGHT in the callee frame, so
        // a default sees earlier already-bound params (and outer scope/globals).
        // Each default value is contract-checked against the param's type, then
        // bound. Never hold a borrow across the `.await`.
        for i in defaults {
            let p = &params[i];
            let def = p
                .default
                .as_ref()
                .expect("default range only covers defaulted params");
            let dv = self.eval_expr(def, call_env).await?;
            if let Some(ty) = &p.ty {
                // IFACE: env-aware so a defaulted interface/class-typed param resolves.
                if !self.check_type_env(&dv, ty, call_env)? {
                    return Err(contract_panic(ty, &dv, span));
                }
            }
            call_env.define(&p.name, dv, true).map_err(AsError::new)?;
        }
        // SP9 §1: `run_body` is the single native re-entry funnel every script call
        // passes through (`#[async_recursion]`, the actual native-stack deepener).
        // Grow the native stack per poll so deep recursion (functions / methods /
        // HOF callbacks) reaches the logical `MAX_CALL_DEPTH` cap cleanly instead of
        // SIGABRTing the native stack first — matching the VM's re-entry guards.
        //
        // DEFER §5.1: install a fresh defer list ON THIS CALL'S ENV (the activation
        // boundary). Any `defer` statement executed in the body resolves THIS list via
        // `env.defer_scope()` (env-chain walk), so concurrently-suspended activations
        // never cross-contaminate (the `Interp`-stack `last()` bug). The list is owned
        // by `call_env`; we keep the `Rc` to drain it at frame exit.
        let defer_list = call_env.install_defer_scope();
        let mut outcome: Result<Value, Control> =
            match crate::vm::stack::grow_future(self.exec(body, call_env)).await {
                Ok(Flow::Return(v)) => Ok(v),
                Ok(Flow::Normal) => Ok(Value::nil()),
                Ok(Flow::Break) => {
                    // Break outside a loop: propagate the error (no drain — the body
                    // never reached a clean exit). The defer list drops with call_env.
                    return Err(AsError::at("'break' outside of a loop", span).into());
                }
                Ok(Flow::Continue) => {
                    return Err(AsError::at("'continue' outside of a loop", span).into());
                }
                // `?` propagation: function returns a `[nil, err]` pair. Store it as
                // Ok so defers still run (spec §3.3).
                Err(Control::Propagate(v)) => Ok(v),
                Err(Control::Panic(e)) => Err(Control::Panic(e)),
                // exit() skips defers — unwind immediately (spec §3.3).
                Err(Control::Exit(code)) => {
                    return Err(Control::Exit(code));
                }
            };
        // DEFER §3.3: drain the defer list (LIFO). `_depth` is still held here
        // (spec §3.8) — the DepthGuard's Drop runs after this block. Panics from
        // deferred calls are merged into `outcome` via `merge_defer_panic` (§3.6).
        let entries = std::mem::take(&mut *defer_list.borrow_mut());
        if !entries.is_empty() {
            self.run_defers(entries, &mut outcome).await;
        }
        // DEFER §3.7: return-type contract check happens AFTER defers run, so a
        // deferred call observes the function's final state first. Only runs when
        // `outcome` is still an `Ok` value (no panic/propagate from a deferred call).
        let result = outcome?;
        if let Some(ty) = ret {
            // IFACE: env-aware return-type contract (interface/class names resolve via
            // the callee frame's env chain → def env → module globals).
            if !self.check_type_env(&result, ty, call_env)? {
                // §6.3 paranoid: if the fn's name span is in the proven fn_rets set,
                // the checker certified this return would always pass — escalate.
                if let Some(ns) = fn_name_span {
                    if let Some(e) = self.maybe_paranoid_escalate(ty, &result, ns) {
                        return Err(e);
                    }
                }
                return Err(contract_panic(ty, &result, span));
            }
        }
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // DEFER §3.3–3.4: defer-frame lifecycle + drain helpers.
    // -----------------------------------------------------------------------

    /// Execute one [`DeferEntry`]: call the captured callee with the captured args.
    /// For `DeferKind::Method` re-enters the member-call hook path so schema/shared/
    /// workflow hooks fire (spec §3.1). For `awaited=true` drives a returned
    /// `Value::future` to completion; for `awaited=false` a `Value::future` result is
    /// the §3.4 Tier-2 error. A `Propagate` from the call is treated as discarded
    /// (the result of a deferred call is discarded per §3.2). Never holds a `RefCell`
    /// borrow across `.await`.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn exec_defer_entry(
        &self,
        entry: DeferEntry,
    ) -> Result<(), Control> {
        let result = match entry.kind {
            DeferKind::Call { callee } => {
                self.call_value(callee, entry.args, entry.span).await
            }
            DeferKind::Method { recv, name } => {
                // Re-enter the member-call evaluator to preserve schema / shared /
                // workflow call-position hooks (spec §3.1 — pre-binding via
                // `read_member` would silently skip them).
                self.call_method_recv(recv, &name, entry.args, entry.span).await
            }
        };
        let result_v = match result {
            Ok(v) => v,
            // A Tier-1 `?`-propagation from a deferred call: the result is discarded
            // per §3.2 (the deferred call's own `run_body` converts it to Ok already,
            // but guard against other callee kinds).
            Err(Control::Propagate(_)) => return Ok(()),
            // Panic or Exit propagates up to `run_defers` for handling.
            Err(e) => return Err(e),
        };
        if entry.awaited {
            // `defer await f()` — if the call returned a future, drive it now (§3.4).
            if let ValueKind::Future(f) = result_v.kind() {
                f.get().await.map(|_| ())?;
            }
            // Non-future + awaited is the identity rule (the language-wide rule).
        } else if let ValueKind::Future(_) = result_v.kind() {
            // `defer f()` returned a future — §3.4 loud Tier-2 error.
            return Err(Control::Panic(AsError::at(
                "deferred call returned a future that would be cancelled on drop — use 'defer await f()' or do async cleanup before exit",
                entry.span,
            )));
        }
        // Non-future result is discarded (§3.2).
        Ok(())
    }

    /// Does a member CALL `recv.name(...)` route to a call-position hook (schema,
    /// workflow-ctx, worker-spawn, shared, actor) rather than the ordinary
    /// `read_member → call_value` path? Pure predicate over the receiver + method name
    /// (no args, no side effects). This is the SINGLE place that enumerates the hook
    /// receivers, consulted by BOTH `eval_chain` (to decide whether to take the hook
    /// path with positional args, vs the fallback with named args + member-read-first
    /// ordering) and `call_method_recv` (which then performs the dispatch). Keeping the
    /// predicate and the dispatch adjacent means the two call sites cannot drift on
    /// WHICH receivers are hooks (spec §3.1).
    pub(crate) fn member_call_is_hook(&self, recv: &Value, name: &str) -> bool {
        if crate::stdlib::schema::is_schema_value(recv)
            && crate::stdlib::schema::is_schema_method(name)
        {
            return true;
        }
        #[cfg(feature = "resilience")]
        if crate::stdlib::resilience::is_resilience_value(recv)
            && crate::stdlib::resilience::is_resilience_method(name)
        {
            return true;
        }
        #[cfg(feature = "workflow")]
        if crate::stdlib::workflow::is_ctx_value(recv)
            && crate::stdlib::workflow::is_ctx_method(name)
        {
            return true;
        }
        if let ValueKind::Class(c) = recv.kind() {
            if c.is_worker && name == "spawn" {
                return true;
            }
        }
        if matches!(recv.kind(), ValueKind::Shared(_)) {
            return true;
        }
        if let ValueKind::Native(n) = recv.kind() {
            if n.kind == crate::value::NativeKind::WorkerActor {
                return true;
            }
        }
        false
    }

    /// Execute a member call on an already-evaluated receiver, routing through all
    /// call-position hooks (schema, workflow-ctx, worker-spawn, shared, actor) exactly
    /// as `eval_chain`'s `ExprKind::Call { callee: Member { … } }` arm does. This is
    /// the hook-dispatch chokepoint shared by `eval_chain` (the normal member-call
    /// path) and `exec_defer_entry` (the `DeferKind::Method` drain path, spec §3.1 —
    /// "re-enter the member-call evaluator"), so the hook logic has ONE location and
    /// the two paths can never drift. The arm ordering / dispatch here MUST match
    /// `member_call_is_hook`'s enumeration above.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn call_method_recv(
        &self,
        recv: Value,
        name: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        // Schema fluent-chain hook (spec §3.1).
        if crate::stdlib::schema::is_schema_value(&recv)
            && crate::stdlib::schema::is_schema_method(name)
        {
            let mut sargs = Vec::with_capacity(args.len() + 1);
            sargs.push(recv);
            sargs.extend(args);
            return self.call_schema(name, &sargs, span).await;
        }
        // Resilience policy call-site hook (RESIL §2.3).
        #[cfg(feature = "resilience")]
        if crate::stdlib::resilience::is_resilience_value(&recv)
            && crate::stdlib::resilience::is_resilience_method(name)
        {
            let mut rargs = Vec::with_capacity(args.len() + 1);
            rargs.push(recv);
            rargs.extend(args);
            return self.call_resilience_method(name, &rargs, span).await;
        }
        // Workflow ctx hook.
        #[cfg(feature = "workflow")]
        if crate::stdlib::workflow::is_ctx_value(&recv)
            && crate::stdlib::workflow::is_ctx_method(name)
        {
            let mut wargs = Vec::with_capacity(args.len() + 1);
            wargs.push(recv);
            wargs.extend(args);
            return self.call_workflow_ctx(name, &wargs, span).await;
        }
        // Worker-class `.spawn` hook.
        if let ValueKind::Class(c) = recv.kind() {
            if c.is_worker && name == "spawn" {
                return self.spawn_actor(c, args, span).await;
            }
        }
        // Shared-value member-call hook.
        if let ValueKind::Shared(node) = recv.kind() {
            return call_shared(node, name, &args, span);
        }
        // Actor-handle async method-call hook.
        if let ValueKind::Native(n) = recv.kind() {
            if n.kind == crate::value::NativeKind::WorkerActor {
                return self.actor_handle_call(n, name, args, span).await;
            }
        }
        // Fallback: read the member first (may error), then call the resolved value.
        let callee_v = self.read_member(&recv, name, span)?;
        self.call_value(callee_v, args, span).await
    }

    /// Drain all entries in `entries` LIFO, folding any panics into `outcome` via
    /// [`merge_defer_panic`] (spec §3.6 r1–r4). All entries run regardless of
    /// individual panics (r4). `Control::Exit` inside an entry aborts the drain
    /// and replaces `outcome` (§3.6 r5). Never holds a `RefCell` borrow across `.await`.
    pub(crate) async fn run_defers(
        &self,
        entries: Vec<DeferEntry>,
        outcome: &mut Result<Value, Control>,
    ) {
        #[cfg(any(test, feature = "fuzzgen", fuzzing))]
        if !entries.is_empty() {
            crate::vm::defer_metrics::defer_metrics::CHOKEPOINT_DRAINS
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // LIFO: entries were pushed in declaration order; drain in reverse.
        for entry in entries.into_iter().rev() {
            #[cfg(any(test, feature = "fuzzgen", fuzzing))]
            crate::vm::defer_metrics::defer_metrics::ENTRIES_DRAINED
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match self.exec_defer_entry(entry).await {
                Ok(()) => {}
                Err(Control::Exit(code)) => {
                    // Exit is uncatchable and skips remaining defers.
                    *outcome = Err(Control::Exit(code));
                    return;
                }
                Err(Control::Panic(e)) => merge_defer_panic(outcome, e),
                // A Propagate escaping exec_defer_entry (shouldn't happen — the
                // deferred call's run_body converts it to Ok; guard for totality).
                Err(Control::Propagate(_)) => {}
            }
        }
    }

    /// DEFER §5.1 REPL helper: drain a session-line defer list, reporting any
    /// resulting panic as a diagnostic (the REPL swallows errors to keep the loop
    /// alive). Folds panics via the same `run_defers` path so §3.6 merge rules hold.
    pub(crate) async fn drain_session_defers(
        &self,
        defer_list: &Rc<RefCell<Vec<DeferEntry>>>,
    ) {
        let entries = std::mem::take(&mut *defer_list.borrow_mut());
        if entries.is_empty() {
            return;
        }
        let mut outcome: Result<Value, Control> = Ok(Value::nil());
        self.run_defers(entries, &mut outcome).await;
        if let Err(Control::Panic(e)) = outcome {
            crate::diagnostics::report(&e);
        }
    }

    /// Like [`exec`] but installs + drains a top-level defer list on the PROGRAM env
    /// (spec §2.3, §5.1). Use this instead of `exec` at the program entry point so
    /// top-level `defer` statements run at program end. `exec` is kept list-free for
    /// `run_body` (which installs its own list on the call env) and for recursive
    /// sub-exec calls (blocks/loops) that resolve the enclosing activation's list via
    /// the env chain. `Control::Exit` from the body skips the drain.
    pub async fn exec_program(
        &self,
        program: &[Stmt],
        env: &Environment,
    ) -> Result<Flow, Control> {
        // DEFER §5.1: install the program-level defer list on the PROGRAM env. A
        // top-level `defer` resolves it via `env.defer_scope()`. (A function call
        // inside the program installs its OWN list on its call env, which shadows this
        // one for defers inside that function — exactly the per-activation semantics.)
        let defer_list = env.install_defer_scope();
        let body_result = self.exec(program, env).await;
        // Exit skips defers (spec §3.3).
        if let Err(Control::Exit(code)) = body_result {
            return Err(Control::Exit(code));
        }
        let mut outcome: Result<Value, Control> = match body_result {
            Ok(Flow::Return(v)) => Ok(v),
            Ok(Flow::Normal) => Ok(Value::nil()),
            Ok(Flow::Break) => Err(Control::Panic(AsError::at(
                "'break' outside of a loop",
                Span::new(0, 0),
            ))),
            Ok(Flow::Continue) => Err(Control::Panic(AsError::at(
                "'continue' outside of a loop",
                Span::new(0, 0),
            ))),
            Err(Control::Propagate(v)) => Ok(v), // top-level `?` → treat as normal
            Err(Control::Panic(e)) => Err(Control::Panic(e)),
            Err(Control::Exit(_)) => unreachable!("handled above"),
        };
        let entries = std::mem::take(&mut *defer_list.borrow_mut());
        if !entries.is_empty() {
            self.run_defers(entries, &mut outcome).await;
        }
        // Convert back to the Flow shape the entry points expect.
        match outcome {
            Ok(_) => Ok(Flow::Normal),
            Err(Control::Panic(e)) => Err(Control::Panic(e)),
            Err(Control::Propagate(v)) => Err(Control::Propagate(v)),
            Err(Control::Exit(code)) => Err(Control::Exit(code)),
        }
    }

    #[async_recursion(?Send)]
    async fn call_function(
        &self,
        func: Rc<crate::value::Function>,
        args: Vec<Value>,
        span: Span,
        // ELIDE §4.3: when true, per-param type-contract checks are skipped inside
        // `run_body` (arity, defaults, and rest collection are still enforced).
        // Worker, generator, and async branches ignore this flag (they either dispatch
        // to a separate isolate or spawn an independent future — the eligibility
        // predicate excludes them from elision). The plain sync-fn branch threads it
        // through to `run_function_body` → `run_body` → `check_call_args`.
        elide_contracts: bool,
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
        // A `worker fn*` (Spec B Task 6) is a STREAMING generator: its body runs in a
        // DEDICATED isolate and is consumed via a cross-thread demand-driven driver.
        // This precedes the local-generator branch (a `worker fn*` has BOTH flags) and
        // the `worker fn` branch (a streaming worker is not a request/response worker).
        // Inline-nesting (a `worker fn*` called from inside an isolate) is NOT special-
        // cased — it spawns a nested dedicated isolate (a generator inside a worker is a
        // rare composition; correctness over the extra thread).
        if func.is_worker && func.is_generator {
            let entry_name = func.name.clone().ok_or_else(|| {
                Control::Panic(AsError::at(
                    "worker fn* must be a named top-level function".to_string(),
                    span,
                ))
            })?;
            return self.spawn_worker_stream(&entry_name, args, span).await;
        }
        if func.is_generator {
            let vm = self.rc();
            let func = func.clone();
            let body: std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Control>>>> =
                Box::pin(
                    // Generators are excluded from ELIDE eligibility → always false.
                    async move { vm.run_function_body(func, args, call_env, span, what, false).await },
                );
            return Ok(Value::generator(Rc::new(
                crate::coro::GeneratorHandle::new(body),
            )));
        }
        // A `worker fn` is dispatched to a pooled isolate (Workers Spec A): build the
        // shippable code slice from the entry program's source and hand the args +
        // slice over a `Send` byte channel; the returned `Value::future` resolves with
        // the worker's result. Only bytes cross threads — the `!Send` runtime stays on
        // this thread. Inline-nesting (a worker fn called from inside an isolate) is
        // handled inside `dispatch_worker`. A worker fn cannot also be `async`/`fn*`
        // (the surface form has no such combination), so this precedes those branches.
        if func.is_worker {
            let entry_name = func
                .name
                .clone()
                .ok_or_else(|| Control::Panic(AsError::at(
                    "worker fn must be a named top-level function".to_string(),
                    span,
                )))?;
            // Inline-nesting: a worker fn called from inside an isolate runs locally
            // (no pool round-trip, no slice build) — the entry is already a global.
            if crate::worker::pool::in_isolate() {
                return crate::worker::dispatch_worker_inline(self, &entry_name, args, span);
            }
            let slice =
                crate::worker::build_code_slice_from_source(self, &entry_name, None)?;
            return crate::worker::dispatch_worker(self, slice, args, span);
        }
        // A script `async fn` is scheduled eagerly: build the body future, spawn it
        // onto the current-thread LocalSet, and hand back a `Value::future`
        // immediately. `await` later drives it; the top-level drain ensures even an
        // unawaited call runs to completion. Non-async functions run inline.
        if func.is_async {
            let vm = self.rc();
            let func = func.clone();
            let fut = crate::task::SharedFuture::new();
            // The task resolves the *cell* (not a `SharedFuture` clone) so it never
            // keeps the handle alive — letting the handle's `Drop` cancel the task
            // once the last `Value::future` is dropped (structured concurrency).
            let cell = fut.cell();
            // Track this task's lifetime for backpressure; the guard moves into the
            // task and decrements on completion OR cancel-on-drop.
            let guard = self.inflight_guard();
            // SP12: capture THIS task's current telemetry span so the spawned body
            // inherits the correct parent lineage (per-task isolation, spec §9.3).
            #[cfg(feature = "telemetry")]
            let telem_parent = crate::interp::telemetry_capture_current();
            // RESIL §5.1: capture THIS task's ambient locals (deadline/trace) so the
            // spawned body inherits them — COW-isolated from the parent's later scopes.
            let locals_parent = crate::interp::task_locals_capture();
            let handle = tokio::task::spawn_local(async move {
                let _g = guard;
                // The owned `func`/`call_env`/`what` live in `run_function_body`'s
                // frame, so the `BodySpec` borrow never escapes this `'static` task.
                // Async fns are excluded from ELIDE eligibility → always false.
                let body = vm.run_function_body(func, args, call_env, span, what, false);
                #[cfg(feature = "telemetry")]
                let body = crate::interp::telemetry_scope(telem_parent, body);
                let r = crate::interp::task_locals_scope(locals_parent, body).await;
                cell.resolve(r);
            });
            // Cancel-on-drop: dropping the last handle aborts this task.
            fut.set_abort(handle.abort_handle());
            // Cooperatively yield if many tasks are in flight, so cancelled/finished
            // ones get reaped (bounds memory in a tight un-awaited loop).
            self.maybe_yield_for_inflight().await;
            return Ok(Value::future(fut));
        }
        self.run_function_body(func, args, call_env, span, what, elide_contracts)
            .await
    }

    /// Run a (already-prepared) function body, owning the `Rc<Function>` for the
    /// whole frame so the `BodySpec` borrow stays local. Used both inline (sync
    /// functions) and from a spawned `'static` task (async functions).
    ///
    /// `elide_contracts` is threaded from ELIDE §4.3: when true, per-param
    /// type-contract checks are skipped inside `run_body`. Async and generator
    /// spawned tasks always call this with `false` (those paths are excluded from
    /// the ELIDE eligibility predicate).
    #[async_recursion(?Send)]
    async fn run_function_body(
        &self,
        func: Rc<crate::value::Function>,
        args: Vec<Value>,
        call_env: Environment,
        span: Span,
        what: String,
        elide_contracts: bool,
    ) -> Result<Value, Control> {
        let spec = BodySpec {
            params: &func.params,
            ret: &func.ret,
            body: &func.body,
            name_span: func.name_span,
        };
        self.run_body(spec, args, &call_env, span, &what, elide_contracts)
            .await
    }

    #[async_recursion(?Send)]
    async fn construct(
        &self,
        class: std::rc::Rc<crate::value::Class>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let instance = gcmodule::Cc::new(std::cell::RefCell::new(
            crate::value::Instance::from_dict(class.clone(), indexmap::IndexMap::new()),
        ));
        let inst_val = Value::instance(instance.clone());
        // Pre-populate declared-field defaults (merged base-class first so a
        // subclass default overrides). `init` may then override; `.from` (Task 4)
        // handles its own defaults. Each default evals lazily in the def env of
        // the class that declared it.
        for (fname, (schema, def_class)) in crate::value::merged_field_schema(&class) {
            if let Some(def) = &schema.default {
                // Eval into a local first (never hold the instance borrow across
                // `.await`).
                let dv = self.eval_expr(def, &def_class.def_env).await?;
                // IFACE: env-aware against the declaring class's def env (where an
                // interface-typed field annotation resolves).
                if !self.check_type_env(&dv, &schema.ty, &def_class.def_env)? {
                    return Err(contract_panic(&schema.ty, &dv, span));
                }
                instance.borrow_mut().insert(&fname, dv);
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
                // SP2 §5 records: no explicit `init` → auto-derive a positional
                // constructor over the declared fields (in merged base-first
                // schema order). Field defaults were already applied above; the
                // positional args OVERRIDE the supplied leading fields, each
                // contract-checked against its field type via the SHARED
                // `auto_init_bindings` helper (identical arity/contract messages to
                // the VM). A zero-field class with no args keeps today's behavior
                // (no fields → empty `params` → only `C()` is valid).
                let fields = crate::value::merged_field_schema(&class);
                let bindings = auto_init_bindings(&fields, &class.name, args, span)?;
                for (fname, v) in bindings {
                    instance.borrow_mut().insert(&fname, v);
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
        let map = match obj.kind() {
            ValueKind::Object(m) => m.clone(),
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
            let raw = map.get(fname);
            let mut val = raw.unwrap_or(Value::nil());
            if val == Value::nil() {
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
            // IFACE: env-aware against the declaring class's def env (so an
            // interface-typed field validates structurally). `validate_into` returns
            // `AsError`, so map the `conforms` cycle/bad-extends `Control` back.
            let ok = self
                .check_type_env(&val, &fs.ty, &def_class.def_env)
                .map_err(|c| control_to_aserror(c, span))?;
            if !ok {
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
            for k in map.keys_snapshot() {
                if !schema.contains_key(&k) {
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

        Ok(Value::instance(gcmodule::Cc::new(std::cell::RefCell::new(
            crate::value::Instance::from_dict(class.clone(), inst_fields),
        ))))
    }

    /// IFACE §5.2: the `instanceof` dispatch — the SINGLE source of truth both engines
    /// call (the tree-walker from `eval_expr`'s `BinOp::InstanceOf` arm; the VM from
    /// `eval_binop_adaptive`'s `InstanceOf` delegation). Branches on the RHS: a
    /// `Value::class(c)` runs `is_instance_of` (the UNCHANGED nominal `Rc::as_ptr` walk
    /// — bit-for-bit identical to pre-IFACE); a `Value::interface(i)` runs the
    /// structural `conforms` (lazy flatten + verdict cache); anything else is a Tier-2
    /// panic with the single "class or interface" message. Because BOTH engines route
    /// InstanceOf through here (and NOT through `apply_binop`, whose InstanceOf arm is
    /// now unreachable), the result + panic text are byte-identical across all four
    /// modes by construction.
    pub(crate) fn eval_instanceof(
        &self,
        l: Value,
        r: Value,
        span: Span,
    ) -> Result<Value, Control> {
        match r.kind() {
            ValueKind::Class(cls) => Ok(Value::bool_(crate::value::is_instance_of(&l, cls))),
            ValueKind::Interface(iface) => Ok(Value::bool_(self.conforms(&l, iface)?)),
            _ => Err(AsError::at(
                "instanceof requires a class or interface on the right-hand side",
                span,
            )
            .into()),
        }
    }

    /// IFACE §8 (G1): the env-aware runtime contract check. The free
    /// `check_type(value, ty)` is environment-free — its `Type::Named` arm matches a
    /// class by NAME-STRING up the value's own chain and can never see an
    /// `InterfaceDef`. This `&self` path resolves a `Type::Named` through the in-scope
    /// `env` (the same `env.get(name)` ladder `coerce_field`/`validate_into` use): a
    /// resolved `Value::interface(i)` runs the structural `conforms`; a resolved
    /// `Value::class(c)` runs nominal `is_instance_of` (by class identity); an
    /// unresolved name (or a non-class/non-interface binding) falls back to today's
    /// PERMISSIVE name-string match (the free `check_type` — the gradual escape).
    /// Composite arms (`Array`/`Optional`/`Union`/`Map`) recurse through this method so
    /// a nested `array<Reader>` resolves element-wise; every non-`Named` leaf delegates
    /// to the retained-unchanged free `check_type` (no behavior change for primitives).
    ///
    /// Returns `Ok(bool)`; the `conforms` lazy-flatten cycle / bad-extends panic rides
    /// the `Err(Control)`.
    pub(crate) fn check_type_env(
        &self,
        value: &Value,
        ty: &crate::ast::Type,
        env: &Environment,
    ) -> Result<bool, Control> {
        use crate::ast::Type;
        match ty {
            Type::Named(name) => match env.get(name).map(|v| v.into_kind()) {
                Some(OwnedKind::Interface(iface)) => self.conforms(value, &iface),
                Some(OwnedKind::Class(cls)) => {
                    Ok(crate::value::is_instance_of(value, &cls))
                }
                // Unresolved / non-class-non-interface name → today's permissive
                // name-string match (gradual: a forward annotation stays silent).
                _ => Ok(check_type(value, ty)),
            },
            Type::Optional(inner) => Ok(*value == Value::nil()
                || self.check_type_env(value, inner, env)?),
            Type::Union(a, b) => {
                Ok(self.check_type_env(value, a, env)? || self.check_type_env(value, b, env)?)
            }
            Type::Array(elem) => match value.kind() {
                ValueKind::Array(arr) => {
                    let items: Vec<Value> = arr.borrow().clone();
                    for it in &items {
                        if !self.check_type_env(it, elem, env)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                _ => Ok(false),
            },
            Type::Map(k, v) => match value.kind() {
                ValueKind::Map(m) => {
                    let entries: Vec<(crate::value::MapKey, Value)> =
                        m.borrow().iter().map(|(mk, mv)| (mk.clone(), mv.clone())).collect();
                    for (mk, mv) in &entries {
                        if !check_type(&mk.to_value(), k)
                            || !self.check_type_env(mv, v, env)?
                        {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                _ => Ok(false),
            },
            // Every other leaf (primitives, Result/Tuple/Future/Fn/Object/…) has no
            // name to resolve → delegate to the unchanged free `check_type`.
            _ => Ok(check_type(value, ty)),
        }
    }

    /// IFACE §5.1: the structural conformance predicate — the runtime source of truth
    /// both engines call (the tree-walker via the `instanceof` eval site, the VM via the
    /// same path through `eval_binop_adaptive`). `true` iff `v` is a `Value::instance`
    /// whose class exposes every method `iface` requires (by name + arity, v1). Only a
    /// class instance can conform; every other LHS (number, bare object, enum, nil, …)
    /// is `Ok(false)`. The `Result` carries the lazy-`flatten` cycle / bad-`extends`
    /// Tier-2 panic. Consults the per-isolate verdict cache (§5.3) above the flatten.
    pub(crate) fn conforms(
        &self,
        v: &Value,
        iface: &Rc<crate::value::InterfaceDef>,
    ) -> Result<bool, Control> {
        let ValueKind::Instance(inst) = v.kind() else {
            // Only class instances can conform in v1 (objects/enums/natives → false).
            return Ok(false);
        };
        let class = inst.borrow().class.clone();
        // Verdict cache: pure memo keyed by (class ptr, iface ptr). Same answer hot or
        // cold — active in all modes. The borrow is short-lived (no await inside).
        let key = (
            Rc::as_ptr(&class) as usize,
            Rc::as_ptr(iface) as usize,
        );
        let gen = self.iface_cache_gen.get();
        // A hit is trusted ONLY when its stored generation still matches: a pointer reused
        // by a later, different descriptor carries an older generation (the define that
        // created it bumped the counter) and is treated as a miss. See the field doc.
        if let Some(&(entry_gen, verdict)) = self.iface_verdict_cache.borrow().get(&key) {
            if entry_gen == gen {
                return Ok(verdict);
            }
        }
        let methods = self.flatten_interface(iface)?;
        // VM classes keep an EMPTY `Class.methods` (their compiled methods live in the
        // VM's per-class side table keyed by `Rc::as_ptr`), so for a VM-registered class
        // the method-presence/arity check routes through the VM; a tree-walker class
        // uses the shared `find_method`. Cached out of the borrow so we never hold a VM
        // borrow across the loop.
        let vm = self.vm();
        let mut verdict = true;
        for (name, req) in methods.iter() {
            // Prefer the VM side table when the class is VM-registered.
            let satisfied = vm
                .as_ref()
                .and_then(|vm| vm.class_method_satisfies(&class, name, req))
                .unwrap_or_else(|| match crate::value::find_method(&class, name) {
                    Some((method, _)) => arity_compatible(&method, req),
                    None => false,
                });
            if !satisfied {
                verdict = false;
                break;
            }
        }
        self.iface_verdict_cache
            .borrow_mut()
            .insert(key, (gen, verdict));
        Ok(verdict)
    }

    /// Bump the `instanceof Interface` verdict-cache generation. Called at every runtime
    /// class/interface DEFINITION so a freed-then-reallocated descriptor at a reused
    /// address can never read a stale verdict (see the `iface_verdict_cache` field doc).
    /// Pure-memo invalidation — never changes observable behavior.
    pub(crate) fn bump_iface_cache_gen(&self) {
        self.iface_cache_gen
            .set(self.iface_cache_gen.get().wrapping_add(1));
    }

    /// IFACE §4: lazily flatten an interface's transitive required method set (own +
    /// every transitively-extended interface's, own-wins on name collision). Memoized
    /// into `iface.flat`; subsequent calls reuse it. Resolves each `extends` NAME
    /// through the interface's `def_env` (late-bound module-globals → forward references
    /// resolve). A cyclic `extends` is caught by a visited-pointer set → a recoverable
    /// Tier-2 panic; an `extends` name resolving to a non-interface / nothing is its own
    /// recoverable Tier-2 panic.
    pub(crate) fn flatten_interface(
        &self,
        iface: &Rc<crate::value::InterfaceDef>,
    ) -> Result<Rc<indexmap::IndexMap<String, crate::value::MethodReq>>, Control> {
        let mut visited: Vec<(*const crate::value::InterfaceDef, String)> = Vec::new();
        self.flatten_interface_inner(iface, &mut visited)
    }

    fn flatten_interface_inner(
        &self,
        iface: &Rc<crate::value::InterfaceDef>,
        // (identity pointer, name) — the pointer drives cycle detection; the name builds
        // the chain message WITHOUT an `unsafe` deref of a raw pointer.
        visited: &mut Vec<(*const crate::value::InterfaceDef, String)>,
    ) -> Result<Rc<indexmap::IndexMap<String, crate::value::MethodReq>>, Control> {
        // Memo hit: return the cached flattened set (borrow dropped immediately).
        if let Some(flat) = iface.flat.borrow().as_ref() {
            return Ok(flat.clone());
        }
        // Cycle guard: re-entering an interface already on the resolution stack is a
        // recoverable Tier-2 panic naming the chain.
        let ptr = Rc::as_ptr(iface);
        if visited.iter().any(|(p, _)| *p == ptr) {
            let mut chain: Vec<String> = visited.iter().map(|(_, n)| n.clone()).collect();
            chain.push(iface.name.clone());
            return Err(AsError::new(format!(
                "cyclic interface extends: {}",
                chain.join(" -> ")
            ))
            .into());
        }
        visited.push((ptr, iface.name.clone()));
        // Resolve each `extends` name, recursively union (base-first, own-wins).
        let mut flat: indexmap::IndexMap<String, crate::value::MethodReq> = indexmap::IndexMap::new();
        for ext_name in &iface.extends {
            match iface.def_env.get(ext_name) {
                Some(other) if matches!(other.kind(), ValueKind::Interface(_)) => {
                    let ValueKind::Interface(parent) = other.kind() else {
                        unreachable!("guarded by the matches! above")
                    };
                    let parent_flat = self.flatten_interface_inner(parent, visited)?;
                    for (k, v) in parent_flat.iter() {
                        flat.insert(k.clone(), v.clone());
                    }
                }
                Some(other) => {
                    visited.pop();
                    return Err(AsError::new(format!(
                        "interface '{}' extends '{}' which is a {}, not an interface",
                        iface.name,
                        ext_name,
                        type_name(&other)
                    ))
                    .into());
                }
                None => {
                    visited.pop();
                    return Err(AsError::new(format!(
                        "interface '{}' extends unknown name '{}'",
                        iface.name, ext_name
                    ))
                    .into());
                }
            }
        }
        // Own requirements win over any inherited with the same name.
        for (k, v) in iface.own_methods.iter() {
            flat.insert(k.clone(), v.clone());
        }
        visited.pop();
        let rc = Rc::new(flat);
        *iface.flat.borrow_mut() = Some(rc.clone());
        Ok(rc)
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
                if val == Value::nil() {
                    Ok(Value::nil())
                } else {
                    self.coerce_field(inner, val, env, strict, path, span).await
                }
            }
            Type::Named(name) => match (val.kind(), env.get(name).map(|v| v.into_kind())) {
                (ValueKind::Object(_), Some(OwnedKind::Class(c))) => {
                    self.validate_into(&c, &val, strict, path, span).await
                }
                _ => Ok(val),
            },
            Type::Array(elem) => match val.kind() {
                ValueKind::Array(a) => {
                    let items: Vec<Value> = a.borrow().clone();
                    let mut out = Vec::with_capacity(items.len());
                    for (i, it) in items.into_iter().enumerate() {
                        let p = format!("{}[{}]", path, i);
                        out.push(self.coerce_field(elem, it, env, strict, &p, span).await?);
                    }
                    Ok(Value::array(out))
                }
                _ => Ok(val),
            },
            Type::Map(_, vty) => match val.kind() {
                ValueKind::Map(m) => {
                    let entries: Vec<(crate::value::MapKey, Value)> = m
                        .borrow()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    let out = crate::value::MapCell::new(indexmap::IndexMap::new());
                    for (k, v) in entries {
                        let p = format!("{}[{}]", path, k.to_value());
                        let cv = self.coerce_field(vty, v, env, strict, &p, span).await?;
                        out.borrow_mut().insert(k, cv);
                    }
                    Ok(Value::map_cell(out))
                }
                // A raw Object (e.g. a JSON dictionary) coerces into a Map at the
                // `.from` boundary: each string key becomes a `MapKey::Str` and
                // each value is recursively coerced through the declared value
                // type. Insertion order is preserved. This closes the gap where a
                // parsed-JSON `map<K, Class>` field would otherwise be an Object
                // and fail the `map<K,V>` contract.
                ValueKind::Object(o) => {
                    // entries() works for both slab-mode and dict-mode objects.
                    let entries: Vec<(String, Value)> = o
                        .entries()
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v))
                        .collect();
                    let out = crate::value::MapCell::new(indexmap::IndexMap::new());
                    for (k, v) in entries {
                        let p = format!("{}[{}]", path, k);
                        let cv = self.coerce_field(vty, v, env, strict, &p, span).await?;
                        out.borrow_mut()
                            .insert(crate::value::MapKey::Str(k.as_str().into()), cv);
                    }
                    Ok(Value::map_cell(out))
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
        let super_ref = Value::super_(std::rc::Rc::new(crate::value::SuperRef {
            receiver: bm.receiver.clone(),
            start: bm.defining_class.superclass.clone(),
        }));
        call_env
            .define("super", super_ref, false)
            .map_err(AsError::new)?;
        // A generator method (`fn*` / `async fn*`) is NOT run inline and is NOT
        // spawned as a task — it takes the SAME consumer-driven path standalone
        // generators do (see `call_function`): its body is built into a boxed future
        // on a `GeneratorHandle` and driven by `gen.next(v)` / `for await`. `self`
        // and `super` are already in `call_env`, so a `yield self.x` body sees the
        // bound instance. Both sync and async generator methods take this path (the
        // body may itself `await`).
        if bm.method.is_generator {
            let vm = self.rc();
            let method = bm.method.clone();
            let name = bm.name.clone();
            let body: std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Control>>>> =
                Box::pin(async move {
                    vm.run_method_body(method, args, call_env, span, name).await
                });
            return Ok(Value::generator(Rc::new(
                crate::coro::GeneratorHandle::new(body),
            )));
        }
        // An async method, like an async free function, is scheduled eagerly and
        // returns a `Value::future`. We move owned copies (the `Rc<Method>`, name,
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
            #[cfg(feature = "telemetry")]
            let telem_parent = crate::interp::telemetry_capture_current();
            // RESIL §5.1: inherit the spawning task's ambient locals (deadline/trace).
            let locals_parent = crate::interp::task_locals_capture();
            let handle = tokio::task::spawn_local(async move {
                let _g = guard;
                // Owned `method`/`call_env`/`name` keep the `BodySpec` borrow inside
                // `run_method_body`'s frame, so nothing escapes the `'static` task.
                let body = vm.run_method_body(method, args, call_env, span, name);
                #[cfg(feature = "telemetry")]
                let body = crate::interp::telemetry_scope(telem_parent, body);
                let r = crate::interp::task_locals_scope(locals_parent, body).await;
                cell.resolve(r);
            });
            fut.set_abort(handle.abort_handle());
            self.maybe_yield_for_inflight().await;
            return Ok(Value::future(fut));
        }
        let spec = BodySpec {
            params: &bm.method.params,
            ret: &bm.method.ret,
            body: &bm.method.body,
            name_span: None, // methods are not in fn_rets
        };
        self.run_body(spec, args, &call_env, span, &bm.name, false).await
    }

    /// Dispatch a STATIC method `C.name(args)` (SP1 §3). Unlike `invoke_method`
    /// there is NO receiver: the call env is the DEFINING class's `def_env` child
    /// (so the class name and sibling statics resolve), with NO `self`/`super`
    /// binding. Async statics return a `Value::future`; `static fn*` returns a
    /// `Value::generator` — reusing the same machinery as instance methods.
    #[async_recursion(?Send)]
    async fn call_static_method(
        &self,
        method: Rc<crate::value::Method>,
        defining: Rc<crate::value::Class>,
        args: Vec<Value>,
        span: Span,
        name: &str,
    ) -> Result<Value, Control> {
        let call_env = defining.def_env.child();
        // A `static worker fn` dispatches to a pooled isolate (Workers Spec A). The
        // entry global is the bare method name; the code slice carries the class name
        // for diagnostics + future class binding.
        if method.is_worker {
            if crate::worker::pool::in_isolate() {
                return crate::worker::dispatch_worker_inline(self, name, args, span);
            }
            let slice = crate::worker::build_code_slice_for_static_method_from_source(
                self,
                &defining.name,
                name,
            )?;
            return crate::worker::dispatch_worker(self, slice, args, span);
        }
        if method.is_generator {
            let vm = self.rc();
            let m = method.clone();
            let what = name.to_string();
            let body: std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Control>>>> =
                Box::pin(async move { vm.run_method_body(m, args, call_env, span, what).await });
            return Ok(Value::generator(Rc::new(
                crate::coro::GeneratorHandle::new(body),
            )));
        }
        if method.is_async {
            let vm = self.rc();
            let m = method.clone();
            let what = name.to_string();
            let fut = crate::task::SharedFuture::new();
            let cell = fut.cell();
            let guard = self.inflight_guard();
            #[cfg(feature = "telemetry")]
            let telem_parent = crate::interp::telemetry_capture_current();
            // RESIL §5.1: inherit the spawning task's ambient locals (deadline/trace).
            let locals_parent = crate::interp::task_locals_capture();
            let handle = tokio::task::spawn_local(async move {
                let _g = guard;
                let body = vm.run_method_body(m, args, call_env, span, what);
                #[cfg(feature = "telemetry")]
                let body = crate::interp::telemetry_scope(telem_parent, body);
                let r = crate::interp::task_locals_scope(locals_parent, body).await;
                cell.resolve(r);
            });
            fut.set_abort(handle.abort_handle());
            self.maybe_yield_for_inflight().await;
            return Ok(Value::future(fut));
        }
        let spec = BodySpec {
            params: &method.params,
            ret: &method.ret,
            body: &method.body,
            name_span: None, // methods are not in fn_rets
        };
        self.run_body(spec, args, &call_env, span, name, false).await
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
            name_span: None, // methods are not in fn_rets
        };
        self.run_body(spec, args, &call_env, span, &what, false).await
    }

    /// FFI §4.5a — `run_in_worker(fn, input, opts?)`: dispatch a `worker fn` and, when
    /// `opts.caps` is present, run it on a fresh DEDICATED (single-tenant) isolate
    /// carrying the reduced `CapSet`. Without `opts.caps` it falls through to the
    /// ordinary pooled `worker fn` dispatch (the caller's caps floor rides as usual).
    ///
    /// The keystone: a cap-reduced job NEVER touches the shared pool (whose reused
    /// `Interp` cannot hold a durable drop, §4.5a) — it gets its own heap + `Interp`, so
    /// the reduced set is a real, memory-isolated sandbox and an in-plugin `caps.drop`
    /// is terminal. Byte-identical across engines (the slice build + dispatch are the
    /// SAME mechanism `worker fn` uses; only the isolate lifecycle + caps differ).
    async fn call_run_in_worker(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let callee = args.first().cloned().unwrap_or(Value::nil());
        // The single payload arg (an array/object/scalar — structured-clone-sendable).
        let input = args.get(1).cloned().unwrap_or(Value::nil());
        let opts = args.get(2).cloned().unwrap_or(Value::nil());

        // The callee must be a NAMED `worker fn` (we ship its slice by name).
        let entry_name = worker_fn_dispatch_name(&callee).ok_or_else(|| {
            Control::Panic(AsError::at(
                "run_in_worker expects a named `worker fn` as its first argument".to_string(),
                span,
            ))
        })?;

        // Parse `opts.caps` into a reduced CapSet (None → no caps option → pooled path).
        let reduced = self.caps_from_run_opts(&opts, span)?;

        // The args list shipped to the worker is the single `input` payload.
        let worker_args = vec![input];

        // Inline-nesting (called from inside an isolate) is not a sandbox spawn — run the
        // entry locally like any nested worker fn (the enclosing slice already ships it).
        // A nested call CANNOT honor a `caps` reduction (an inline run shares the
        // enclosing isolate's `Interp`, so there is no separate cap boundary to install).
        // Silently ignoring an explicit `{caps}` would be a security footgun, so REFUSE
        // it loudly — the author must spawn the cap-reduced worker from the top level.
        if crate::worker::pool::in_isolate() {
            if reduced.is_some() {
                return Err(AsError::at(
                    "run_in_worker with a `caps` option is not supported from inside a \
                     worker (a nested inline run shares the enclosing isolate's \
                     capabilities); spawn the cap-reduced worker from the top level"
                        .to_string(),
                    span,
                )
                .into());
            }
            return crate::worker::dispatch_worker_inline(self, &entry_name, worker_args, span);
        }

        // Use the `.aso`-aware slice builder: `run_in_worker` is the SAME shared method
        // on both engines (no VM-specific override like a bare `worker fn` call has), so
        // it must build the slice from retained source OR the stored `.aso` bytes to stay
        // four-mode byte-identical (it previously failed under `ascript run x.aso`).
        let slice = crate::worker::build_code_slice_for_interp(self, &entry_name)?;
        match reduced {
            // Cap-reduced → DEDICATED single-tenant isolate carrying the reduced CapSet.
            Some(caps) => {
                crate::worker::dispatch_worker_dedicated(self, slice, worker_args, caps, span)
            }
            // No caps option → ordinary pooled dispatch (caller's floor rides as usual).
            None => crate::worker::dispatch_worker(self, slice, worker_args, span),
        }
    }

    /// Parse a `run_in_worker` `opts.caps` value into a reduced [`CapSet`], or `None`
    /// when no `caps` option is present (→ the pooled path). The `caps` shape mirrors
    /// the manifest/CLI: `{ deny: ["ffi", ...], net: {deny, allow}, fs: {deny, allow} }`.
    /// An unknown cap name / malformed shape is a Tier-2 panic (programmer error).
    fn caps_from_run_opts(
        &self,
        opts: &Value,
        span: Span,
    ) -> Result<Option<crate::stdlib::caps::CapSet>, Control> {
        let opts_obj = match opts.kind() {
            ValueKind::Object(o) => o,
            ValueKind::Nil => return Ok(None),
            _ => return Ok(None),
        };
        let caps_val = match opts_obj.get("caps") {
            Some(v) => v,
            None => return Ok(None),
        };
        let caps_obj = match caps_val.kind() {
            ValueKind::Object(o) => o.clone(),
            _ => {
                return Err(AsError::at(
                    "run_in_worker: opts.caps must be an object (e.g. { deny: [\"ffi\"] })"
                        .to_string(),
                    span,
                )
                .into())
            }
        };
        // Start from the CALLER's current caps (denial is monotone — a worker can only
        // ever be MORE restricted than its dispatcher), then subtract opts.caps.deny.
        let mut set = self.caps();
        if let Some(OwnedKind::Array(deny)) = caps_obj.get("deny").map(|v| v.into_kind()) {
            for name in deny.borrow().iter() {
                let name = match name.kind() {
                    ValueKind::Str(s) => s.to_string(),
                    _ => {
                        return Err(AsError::at(
                            "run_in_worker: opts.caps.deny must be an array of capability names"
                                .to_string(),
                            span,
                        )
                        .into())
                    }
                };
                match crate::stdlib::caps::cap_name(&name) {
                    Some(cap) => set.deny(cap),
                    None => {
                        return Err(AsError::at(
                            format!("run_in_worker: unknown capability '{name}' in opts.caps.deny"),
                            span,
                        )
                        .into())
                    }
                }
            }
        }
        // A bare `{}` caps object (no deny) still routes to the dedicated path — the
        // author explicitly asked for an isolate (the reduced set just equals the floor).
        Ok(Some(set))
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
                Ok(Value::nil())
            }
            "Ok" => {
                let value = args.first().cloned().unwrap_or(Value::nil());
                Ok(make_pair(value, Value::nil()))
            }
            "Err" => {
                let msg = args.first().cloned().unwrap_or(Value::nil());
                Ok(make_pair(Value::nil(), make_error(msg)))
            }
            "assert" => {
                let cond = args.first().cloned().unwrap_or(Value::nil());
                if cond.is_truthy() {
                    Ok(Value::nil())
                } else {
                    let msg = match args.get(1) {
                        Some(v) if v.as_str().is_some() => v.as_str().unwrap().to_string(),
                        Some(v) => v.to_string(),
                        None => "assertion failed".to_string(),
                    };
                    Err(AsError::at(msg, span).into())
                }
            }
            "recover" => {
                let callee = args.first().cloned().unwrap_or(Value::nil());
                match self.call_value(callee, Vec::new(), span).await {
                    Ok(v) => Ok(make_pair(v, Value::nil())),
                    Err(Control::Panic(e)) => Ok(make_pair(
                        Value::nil(),
                        make_error(Value::str(e.message)),
                    )),
                    // A `?` propagation inside `fn` is already converted to fn's return
                    // value by call_function, so this is unreachable in practice; pass it through.
                    Err(Control::Propagate(v)) => Err(Control::Propagate(v)),
                    // exit() is NOT catchable by recover — pass it through unchanged.
                    Err(Control::Exit(code)) => Err(Control::Exit(code)),
                }
            }
            "run_in_worker" => self.call_run_in_worker(args, span).await,
            "exit" => {
                // exit(code?) — default 0; code must be an integer in 0..=255.
                let code: i32 = match args.first() {
                    None => 0,
                    // NUM §4: accept BOTH numeric subtypes; the code must be an
                    // integer in 0..=255 either way.
                    Some(v) if v.is_number() => {
                        let n = v.as_f64().unwrap_or(f64::NAN);
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
                    Some(v) if v.as_str().is_some() => v.as_str().unwrap().to_string(),
                    Some(v) => v.to_string(),
                    None => "<unnamed>".to_string(),
                };
                let func = args.get(1).cloned().unwrap_or(Value::nil());
                // Register only; `ascript test` runs these via run_registered_tests.
                self.tests.borrow_mut().push((name, func));
                Ok(Value::nil())
            }
            "len" => {
                let v = args.first().cloned().unwrap_or(Value::nil());
                let n = match v.kind() {
                    ValueKind::Str(s) => s.chars().count(),
                    ValueKind::Array(a) => a.borrow().len(),
                    ValueKind::Object(o) => o.len(),
                    ValueKind::Map(m) => m.borrow().len(),
                    ValueKind::Set(s) => s.borrow().len(),
                    ValueKind::Bytes(b) => b.borrow().len(),
                    // SRV §3.5: `len()` of a frozen `Shared` reads the frozen
                    // container's length (zero-copy).
                    ValueKind::Shared(node) => match shared_len(node) {
                        Some(n) => n,
                        None => {
                            return Err(AsError::at(
                                format!(
                                    "len() expects a string, array, object, map, set, or bytes, got {}",
                                    type_name(&v)
                                ),
                                span,
                            )
                            .into())
                        }
                    },
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
                // NUM §4: a length/count is an `Int`.
                Ok(Value::int(n as i64))
            }
            "type" => {
                let v = args.first().cloned().unwrap_or(Value::nil());
                Ok(Value::str(type_name(&v)))
            }
            // NUM §4: `int(x)` conversion.
            //   float → int, truncated TOWARD ZERO (a non-finite float is a Tier-2 panic);
            //   int    → identity;
            //   string → parse, returning a Tier-1 `[int, err]` pair (bad input is a value,
            //            not a bug);
            //   bool   → 0/1 (matching `convert.toNumber`'s bool coercion).
            "int" => {
                let v = args.first().cloned().unwrap_or(Value::nil());
                if v.is_int_value() {
                    return Ok(v);
                }
                match v.kind() {
                    ValueKind::Float(f) => {
                        if !f.is_finite() {
                            return Err(AsError::at(
                                format!("cannot convert non-finite float {} to int", format_number(f)),
                                span,
                            )
                            .into());
                        }
                        let t = f.trunc();
                        // `i64::MAX as f64` rounds UP to 2^63 (= 9223372036854775808.0),
                        // which is OUT of i64 range, so a `<=` bound would admit 2^63 and
                        // `as i64` would silently saturate to i64::MAX. Use a STRICT upper
                        // bound: `-(i64::MIN as f64)` is exactly 2^63, and `<` excludes it
                        // while still admitting the largest representable in-range float
                        // (2^63 − 2048). The lower bound is exact (`i64::MIN as f64` == −2^63).
                        if t >= i64::MIN as f64 && t < -(i64::MIN as f64) {
                            Ok(Value::int(t as i64))
                        } else {
                            Err(AsError::at(
                                format!("float {} is out of range for int (i64)", format_number(f)),
                                span,
                            )
                            .into())
                        }
                    }
                    ValueKind::Bool(b) => Ok(Value::int(if b { 1 } else { 0 })),
                    ValueKind::Str(s) => match s.trim().parse::<i64>() {
                        Ok(n) => Ok(make_pair(Value::int(n), Value::nil())),
                        Err(_) => Ok(make_pair(
                            Value::nil(),
                            make_error(Value::str(
                                format!("cannot parse '{}' as an int", s),
                            )),
                        )),
                    },
                    _ => Err(AsError::at(
                        format!("int() cannot convert {}", type_name(&v)),
                        span,
                    )
                    .into()),
                }
            }
            // NUM §4: `float(x)` conversion.
            //   int    → exact f64;
            //   float  → identity;
            //   string → parse, returning a Tier-1 `[float, err]` pair;
            //   bool   → 0.0/1.0.
            "float" => {
                let v = args.first().cloned().unwrap_or(Value::nil());
                if matches!(v.kind(), ValueKind::Float(_)) {
                    return Ok(v);
                }
                match v.kind() {
                    ValueKind::Int(i) => Ok(Value::float(i as f64)),
                    ValueKind::Bool(b) => Ok(Value::float(if b { 1.0 } else { 0.0 })),
                    ValueKind::Str(s) => match s.trim().parse::<f64>() {
                        Ok(n) => Ok(make_pair(Value::float(n), Value::nil())),
                        Err(_) => Ok(make_pair(
                            Value::nil(),
                            make_error(Value::str(
                                format!("cannot parse '{}' as a float", s),
                            )),
                        )),
                    },
                    _ => Err(AsError::at(
                        format!("float() cannot convert {}", type_name(&v)),
                        span,
                    )
                    .into()),
                }
            }
            "range" => {
                // NUM §4: `range(..)` accepts Int OR Float args and, like the
                // language `a..b` value-range, yields an Int array when EVERY
                // provided argument is an Int (a float arg makes the whole sequence
                // float). A missing arg defaults to `Int(0)`/`Int(1)`, which keeps
                // the all-int case integral.
                let want_num = |i: usize, default: f64| -> Result<(f64, bool), Control> {
                    match args.get(i) {
                        Some(v) => match v.as_f64() {
                            Some(n) => Ok((n, v.is_int_value())),
                            None => Err(AsError::at(
                                format!(
                                    "range() expects number arguments, got {}",
                                    type_name(v)
                                ),
                                span,
                            )
                            .into()),
                        },
                        // An omitted bound/step is the integral default.
                        None => Ok((default, true)),
                    }
                };
                let ((start, s_int), (end, e_int), (step, k_int)) = match args.len() {
                    1 => ((0.0, true), want_num(0, 0.0)?, (1.0, true)),
                    2 => (want_num(0, 0.0)?, want_num(1, 0.0)?, (1.0, true)),
                    3 => (want_num(0, 0.0)?, want_num(1, 0.0)?, want_num(2, 1.0)?),
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
                let yields_int = s_int && e_int && k_int;
                let mut out = Vec::new();
                let mut i = start;
                if step > 0.0 {
                    while i < end {
                        out.push(range_counter_value(i, yields_int));
                        i += step;
                    }
                } else {
                    while i > end {
                        out.push(range_counter_value(i, yields_int));
                        i += step;
                    }
                }
                Ok(Value::array(out))
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
                // Shared index-write dispatch (with the VM's `Op::SetIndex`) so the
                // two engines apply identical index-assignment semantics + panic
                // messages. `object.span` anchors the non-array error; `target.span`
                // (the whole index expr) anchors OOB / object-index-type errors.
                Ok(index_set(&obj, &idx, value, object.span, target.span)?)
            }
            ExprKind::Member { object, name } => {
                let obj = self.eval_expr(object, env).await?;
                self.set_member(&obj, name, value, object.span, value_span)
            }
            _ => Err(AsError::at("invalid assignment target", target.span).into()),
        }
    }

    /// Run the declared field-type CONTRACT for an instance field write (SHAPE Task
    /// 3.4). The SINGLE source of truth both engines reach: the tree-walker via
    /// `set_member` (above) and the VM via `vm_set_prop` (which performs the slab
    /// transition itself but routes the contract through HERE), so the contract
    /// verdict + the `contract_panic` message/span are byte-identical by
    /// construction. A no-op when `name` is not a declared field (undeclared adds
    /// carry no contract).
    pub(crate) fn check_instance_field_contract(
        &self,
        class: &std::rc::Rc<crate::value::Class>,
        name: &str,
        value: &Value,
        value_span: Span,
    ) -> Result<(), Control> {
        if let Some(schema) = lookup_field_schema(class, name) {
            // IFACE: env-aware against the class's def env so an interface-typed
            // field assignment validates structurally.
            if !self.check_type_env(value, &schema.ty, &class.def_env)? {
                return Err(contract_panic(&schema.ty, value, value_span));
            }
        }
        Ok(())
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
        match obj.kind() {
            ValueKind::Object(map) => {
                check_not_frozen(obj, value_span)?;
                map.borrow_mut().insert(name.to_string(), value.clone());
                Ok(value)
            }
            ValueKind::Instance(inst) => {
                check_not_frozen(obj, value_span)?;
                let class = inst.borrow().class.clone();
                // The field-type CONTRACT chokepoint — SHARED by the tree-walker
                // (here) and the VM (`vm_set_prop`, which calls this same helper),
                // so the panic message/span are byte-identical by construction.
                self.check_instance_field_contract(&class, name, &value, value_span)?;
                // The tree-walker's instances are always DICT (shape 0), so `insert`
                // is the plain dict path (a new key appends; an existing key updates
                // in place). The VM never reaches THIS write for a slab instance — it
                // runs the contract above via `vm_set_prop`, then performs the precise
                // slab transition itself (`vm_instance_insert`).
                inst.borrow_mut().insert(name, value.clone());
                Ok(value)
            }
            // SRV §3.8: a member-assign to a frozen `Shared` is the shipped
            // `cannot mutate a frozen {kind}` panic (reusing `frozen_kind` via
            // `check_not_frozen`, NOT a bespoke string) — byte-identical to the VM
            // `store_property` guard. (`check_not_frozen` always errors here since a
            // Shared object/array reports a frozen kind.)
            ValueKind::Shared(_) => {
                check_not_frozen(obj, value_span)?;
                // A Shared scalar reports no frozen kind → fall through to the
                // generic "cannot set property" (a scalar was never assignable).
                Err(AsError::at(
                    format!("cannot set property '{}' on this value", name),
                    obj_span,
                )
                .into())
            }
            _ => Err(AsError::at(
                format!("cannot set property '{}' on this value", name),
                obj_span,
            )
            .into()),
        }
    }
}

/// `object.freeze` (SP2 §4): guard a container mutation. If `v` is a frozen
/// container, raise the Tier-2 panic `"cannot mutate a frozen <kind>"` anchored at
/// the mutation-site `span`; otherwise `Ok(())`. Called at the START of every
/// user-visible mutation path on BOTH engines (tree-walker `index_set`/`set_member`
/// plus stdlib mutators; VM `SetIndex`/`vm_set_prop`) so the diagnostic is
/// byte-identical.
pub(crate) fn check_not_frozen(v: &Value, span: Span) -> Result<(), Control> {
    if let Some(kind) = crate::value::frozen_kind(v) {
        return Err(AsError::at(format!("cannot mutate a frozen {}", kind), span).into());
    }
    Ok(())
}

/// Pure unary-operator dispatch shared by the tree-walker (`ExprKind::Unary`) and
/// the bytecode VM (`Op::Neg`/`Op::Not`). `span` anchors the Tier-2 panic so both
/// engines emit byte-identical diagnostics.
pub(crate) fn apply_unop(op: UnOp, v: Value, span: Span) -> Result<Value, Control> {
    match op {
        UnOp::Neg => match v.kind() {
            // `-int` is checked: `-i64::MIN` overflows → Tier-2 panic (NUM §3.2).
            ValueKind::Int(i) => match i.checked_neg() {
                Some(n) => Ok(Value::int(n)),
                None => Err(AsError::at("integer overflow in '-'", span).into()),
            },
            ValueKind::Float(n) => Ok(Value::float(-n)),
            ValueKind::Decimal(d) => Ok(Value::decimal(-**d)),
            _ => Err(AsError::at(
                format!("cannot negate a non-number, got {}", type_name(&v)),
                span,
            )
            .into()),
        },
        UnOp::Not => Ok(Value::bool_(!v.is_truthy())),
        // `~x` — int bitwise NOT (NUM §3.2). Int-only: a float (or any non-int)
        // operand is a Tier-2 panic.
        UnOp::BitNot => match v.kind() {
            ValueKind::Int(i) => Ok(Value::int(!i)),
            ValueKind::Float(_) => {
                Err(AsError::at("bitwise op requires int operands, got float", span).into())
            }
            _ => Err(AsError::at(
                format!("cannot apply ~ to a non-int, got {}", type_name(&v)),
                span,
            )
            .into()),
        },
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
/// Materialize a range value `[lo .. hi)` (exclusive) or `[lo ..= hi]` (inclusive)
/// into an eager `array<number>` with an omitted (±1) step whose DIRECTION is
/// inferred from the bounds (so a bare `10..1` counts DOWN to `[10, 9, …, 2]`).
/// Both bounds must be `Number`;
/// otherwise raise the same Tier-2 `"range bounds must be numbers"` panic the
/// tree-walker and VM emit. Shared by `apply_binop` (`Op::Range`) and the VM's
/// `Op::RangeInclusive` so both engines are byte-identical.
pub(crate) fn materialize_range(
    l: &Value,
    r: &Value,
    inclusive: bool,
    span: Span,
) -> Result<Value, Control> {
    materialize_range_stepped(l, r, inclusive, None, span)
}

/// Resolve the effective step for a range `lo..hi` (the SINGLE source of truth for
/// direction + validation, shared verbatim by the tree-walker and the bytecode VM
/// so their behavior and panic messages can never drift).
///
/// - `step_v == Some(s)`: the step's SIGN is honored as the direction.
///   - `s` is `0`/`NaN`/`±Infinity` → Tier-2 panic *"step must be a finite,
///     non-zero number"* (no interpolation).
///   - `lo != hi` and `sign(s) != sign(hi - lo)` → Tier-2 panic *"step `<s>` moves
///     away from end (`<hi>`); range can never progress"*. The `<s>`/`<hi>` are
///     formatted via `Value::float` Display (`format_number`) so both engines
///     produce a byte-identical string.
/// - `step_v == None`: the omitted-step default infers direction from the bounds
///   (sequence semantics, spec §3.1): `+1.0` when `hi >= lo`, `-1.0` when
///   `hi < lo`. So a bare `10..1` counts down. `lo == hi` is the empty (or, for
///   `..=`, single-element) range.
pub(crate) fn resolve_step(
    lo: f64,
    hi: f64,
    step_v: Option<f64>,
    span: Span,
) -> Result<f64, Control> {
    match step_v {
        Some(s) => {
            if s == 0.0 || !s.is_finite() {
                return Err(AsError::at("step must be a finite, non-zero number", span).into());
            }
            if lo != hi && (s > 0.0) != (hi > lo) {
                // `{s}`/`{hi}` MUST match the engines' canonical number formatting
                // (the `Value::float` Display path) so the message is byte-identical.
                return Err(AsError::at(
                    format!(
                        "step {} moves away from end ({}); range can never progress",
                        format_number(s),
                        format_number(hi),
                    ),
                    span,
                )
                .into());
            }
            Ok(s)
        }
        // Omitted step: the direction is inferred from the bounds (sequence
        // semantics, spec §3.1) — ascending `+1` when `hi >= lo`, descending
        // `-1` when `hi < lo`. `lo == hi` takes the `+1` branch and yields the
        // empty (or single-element, when inclusive) range, which is correct.
        None => Ok(if hi >= lo { 1.0 } else { -1.0 }),
    }
}

/// Format a `Value::float`'s `f64` exactly as the interpreter/VM display it
/// (`impl Display for Value` → `write!("{}", n)`), so a number interpolated into a
/// range panic message is identical across both engines.
pub(crate) fn format_number(n: f64) -> String {
    Value::float(n).to_string()
}

/// NUM §4: produce a range loop counter `Value`. When `yields_int` is true the
/// f64 counter `i` is converted to a `Value::int` (an integral in-range range step
/// always lands the counter on an exact integer within `i64` range, so this is
/// lossless); a counter that is somehow non-integral or out of `i64` range falls
/// back to `Value::float` rather than producing a wrong int. Otherwise the counter
/// stays a `Value::float`. This is the SINGLE shared decision so the tree-walker
/// and the VM (which seeds Int slots when the bounds+step are Int) agree.
pub(crate) fn range_counter_value(i: f64, yields_int: bool) -> Value {
    if yields_int {
        if let Some(n) = (Value::float(i)).as_int_exact() {
            return Value::int(n);
        }
    }
    Value::float(i)
}

/// Materialize a range value `[lo .. hi)` / `[lo ..= hi]` into an eager
/// `array<number>`, honoring the resolved/validated `step` (direction-aware). The
/// step (`None` → omitted default) is resolved through `resolve_step` so direction,
/// validation, and panic messages are shared with the for-range loop and the VM.
pub(crate) fn materialize_range_stepped(
    l: &Value,
    r: &Value,
    inclusive: bool,
    step: Option<&Value>,
    span: Span,
) -> Result<Value, Control> {
    let (lo, hi, bounds_int) = match (l.as_f64(), r.as_f64()) {
        (Some(a), Some(b)) => (a, b, l.is_int_value() && r.is_int_value()),
        _ => return Err(AsError::at("range bounds must be numbers", span).into()),
    };
    // The step (when present) must be a number; its KIND is preserved so an Int
    // materialized range requires Int bounds AND an Int step (`0..10 step 2` → Int,
    // `0..10 step 2.0` → Float). An omitted step is the integral `±1`.
    let (step_v, step_int) = match step {
        Some(sv) => match sv.as_f64() {
            Some(s) => (Some(s), sv.is_int_value()),
            None => return Err(AsError::at("range step must be a number", span).into()),
        },
        None => (None, true),
    };
    let resolved = resolve_step(lo, hi, step_v, span)?;
    let yields_int = bounds_int && step_int;
    let mut items = Vec::new();
    let mut i = lo;
    while range_has_next(i, hi, resolved, inclusive) {
        items.push(range_counter_value(i, yields_int));
        i += resolved;
    }
    Ok(Value::array(items))
}

/// The direction-aware loop predicate shared by the for-range loop and value
/// materialization (and mirrored in the VM's `Op::RangeHasNext`): with a positive
/// step iterate while `i < hi` (or `i <= hi` when inclusive); with a negative step
/// while `i > hi` (or `i >= hi`).
pub(crate) fn range_has_next(i: f64, hi: f64, step: f64, inclusive: bool) -> bool {
    if step > 0.0 {
        if inclusive {
            i <= hi
        } else {
            i < hi
        }
    } else if inclusive {
        i >= hi
    } else {
        i > hi
    }
}

/// Match-range membership, shared verbatim by the tree-walker (`Pattern::Range`)
/// and the VM (`Op::MatchRange`) so the two engines can never drift.
///
/// `step_v` is the pattern's step: `None` when the `step` clause is OMITTED,
/// `Some(k)` for an explicit `step k`. When `k` was given it MUST already be the
/// resolved/validated value from [`resolve_step`] (the caller runs that first so a
/// `step 0` / non-finite / direction-mismatch pattern PANICS with the byte-identical
/// message iteration uses).
///
/// - **No step (`None`):** plain in-bounds membership — `x` between the bounds,
///   honoring `..`/`..=` and the bounds-inferred direction. This is exactly the
///   pre-existing plain-pattern behavior (NO stride test), so fractional subjects
///   like `match 2.5 { 1..=10 => … }` keep matching.
/// - **With step (`Some(k)`):** strided membership (spec §3.7) — in bounds AND on
///   the stride from the anchor `lo`: `q = (x − lo) / k` is a NON-NEGATIVE WHOLE
///   number (`q >= 0 && q.fract() == 0`). Anchor is `start`, so parity/offset
///   depends on where the range begins.
pub(crate) fn range_pattern_contains(
    x: f64,
    lo: f64,
    hi: f64,
    step_v: Option<f64>,
    inclusive: bool,
) -> bool {
    // Direction: from the explicit step's sign, else inferred from the bounds
    // (the same rule `resolve_step` uses, so in-bounds is direction-consistent).
    let step = step_v.unwrap_or(if hi >= lo { 1.0 } else { -1.0 });
    // In-bounds: upper edge via the shared iteration predicate; lower edge is the
    // anchor `lo` on the step's side (ascending: x >= lo; descending: x <= lo).
    let upper_ok = range_has_next(x, hi, step, inclusive);
    let lower_ok = if step > 0.0 { x >= lo } else { x <= lo };
    if !(upper_ok && lower_ok) {
        return false;
    }
    match step_v {
        // Plain pattern: in-bounds is the whole test (no stride), unchanged.
        None => true,
        // Stepped pattern: also require `x` on the stride from the anchor `lo`.
        Some(k) => {
            let q = (x - lo) / k;
            q >= 0.0 && q.fract() == 0.0
        }
    }
}

/// Dispatch order mirrors `eval_expr`'s `ExprKind::Binary` arm exactly:
/// Eq/Ne (cross-type decimal equality) → Range (eager `array<number>`) → string
/// concat (`+` on two `Str`) → decimal arithmetic/ordering (either operand a
/// `Decimal`) → the two-`Number` path → the generic "requires two numbers" error.
/// If `name` is a reserved scalar type name usable as an `instanceof` RHS (NUM §6),
/// test whether `lhs` is of that type. Returns `Some(bool)` for a recognized name,
/// `None` otherwise (so the caller falls back to the class-based `instanceof`).
///
/// Shared verbatim by the tree-walker (`ExprKind::Binary { InstanceOf }`) and the
/// VM (`Op::InstanceOf` with a pre-resolved type-name operand) so the two engines
/// are byte-identical.
pub(crate) fn instanceof_reserved_type(lhs: &Value, name: &str) -> Option<bool> {
    match name {
        "int" => Some(matches!(lhs.kind(), ValueKind::Int(_))),
        "float" => Some(matches!(lhs.kind(), ValueKind::Float(_))),
        "number" => Some(matches!(lhs.kind(), ValueKind::Int(_) | ValueKind::Float(_))),
        "string" => Some(matches!(lhs.kind(), ValueKind::Str(_))),
        "bool" => Some(matches!(lhs.kind(), ValueKind::Bool(_))),
        _ => None,
    }
}

/// Is `name` one of the reserved scalar type names recognized as an `instanceof`
/// RHS (NUM §6)? Used by both front-ends to decide whether to skip evaluating the
/// RHS expression and route to [`instanceof_reserved_type`] instead.
pub(crate) fn is_reserved_instanceof_type_name(name: &str) -> bool {
    matches!(name, "int" | "float" | "number" | "string" | "bool")
}

pub(crate) fn apply_binop(op: BinOp, l: Value, r: Value, span: Span) -> Result<Value, Control> {
    // Eq/Ne: cross-type Decimal↔Number comparison before generic `==`.
    match op {
        BinOp::Eq => {
            let eq = decimal_cross_eq(&l, &r, span)?;
            return Ok(Value::bool_(eq));
        }
        BinOp::Ne => {
            let eq = decimal_cross_eq(&l, &r, span)?;
            return Ok(Value::bool_(!eq));
        }
        _ => {}
    }

    // Range `a..b`: eager, half-open `array<number>` with step 1, matching
    // ForRange and the `range()` builtin. Returns an Array, so it must be handled
    // before the generic "two numbers → Number" path below.
    if let BinOp::Range = op {
        return materialize_range(&l, &r, false, span);
    }

    // IFACE §5.2: `instanceof` is intercepted by BOTH engines BEFORE `apply_binop`
    // (the tree-walker in `eval_expr`, the VM in `eval_binop_adaptive`) and routed
    // through the shared `&self` `Interp::eval_instanceof` — which needs the engine
    // state (`conforms`/verdict cache) `apply_binop` (a free fn) cannot reach. So this
    // arm is unreachable; keeping it documents the contract and traps a stray caller.
    if let BinOp::InstanceOf = op {
        unreachable!("instanceof is routed through Interp::eval_instanceof, not apply_binop");
    }

    // String concatenation: `+` joins two strings.
    if let BinOp::Add = op {
        if let (ValueKind::Str(a), ValueKind::Str(b)) = (l.kind(), r.kind()) {
            return Ok(Value::str(format!("{}{}", a, b)));
        }
    }

    // Decimal arithmetic/comparison: triggered when either operand is Decimal.
    // The other side is coerced (Number→Decimal; non-finite→Tier-2 panic;
    // non-number/non-decimal → fall through to error).
    if matches!((l.kind(), r.kind()), (ValueKind::Decimal(_), _) | (_, ValueKind::Decimal(_))) {
        use crate::stdlib::decimal::coerce_to_decimal;
        let da = coerce_to_decimal(&l, span)?;
        let db = coerce_to_decimal(&r, span)?;
        if let (Some(a), Some(b)) = (da, db) {
            // Decimal arithmetic uses the CHECKED rust_decimal ops: the bare
            // `+ - * / %` operators `panic!` on 96-bit-mantissa overflow (a hard
            // process abort, not catchable by `recover` and divergent from a
            // Tier-2 contract). `checked_*` returns `None` on overflow, which we
            // raise as a recoverable Tier-2 panic from this SHARED site (both
            // engines reach it → byte-identical).
            let dec_overflow = |what: &str| AsError::at(format!("decimal {what} overflowed"), span);
            let result = match op {
                BinOp::Add => {
                    Value::decimal(a.checked_add(b).ok_or_else(|| dec_overflow("addition"))?)
                }
                BinOp::Sub => Value::decimal(
                    a.checked_sub(b)
                        .ok_or_else(|| dec_overflow("subtraction"))?,
                ),
                BinOp::Mul => Value::decimal(
                    a.checked_mul(b)
                        .ok_or_else(|| dec_overflow("multiplication"))?,
                ),
                BinOp::Div => {
                    if b.is_zero() {
                        return Err(AsError::at("decimal division by zero", span).into());
                    }
                    Value::decimal(a.checked_div(b).ok_or_else(|| dec_overflow("division"))?)
                }
                BinOp::Mod => {
                    if b.is_zero() {
                        return Err(AsError::at("decimal remainder by zero", span).into());
                    }
                    Value::decimal(a.checked_rem(b).ok_or_else(|| dec_overflow("remainder"))?)
                }
                // Ordering: both operands are already finite Decimals here
                // (coerce_to_decimal above Tier-2-panics on a non-finite Number).
                // This is the INTENTIONAL asymmetry vs equality: `decimal ==
                // Infinity` is a lenient `false` (decimal_cross_eq), but `decimal
                // < Infinity` panics — there is no sensible order. See
                // decimal_cross_eq's doc.
                BinOp::Lt => Value::bool_(a < b),
                BinOp::Le => Value::bool_(a <= b),
                BinOp::Gt => Value::bool_(a > b),
                BinOp::Ge => Value::bool_(a >= b),
                // Pow: not defined for Decimal — Tier-2 panic.
                BinOp::Pow => {
                    return Err(AsError::at(
                        "exponentiation (**) is not supported for decimal; use math.pow or convert to number",
                        span,
                    )
                    .into())
                }
                // Bitwise/shift/wrapping (NUM §3.2) are int-ONLY — not defined for
                // decimal. A Tier-2 panic, consistent with the float-operand rejection.
                BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::WrapAdd
                | BinOp::WrapSub
                | BinOp::WrapMul => {
                    return Err(AsError::at(int_only_float_msg(op), span).into())
                }
                BinOp::Eq | BinOp::Ne | BinOp::Range | BinOp::InstanceOf => {
                    unreachable!("handled above")
                }
                BinOp::And | BinOp::Or | BinOp::Coalesce => {
                    unreachable!("short-circuit ops are not dispatched through apply_binop")
                }
            };
            return Ok(result);
        }
        // One operand was not a number or decimal — fall through to the generic
        // "operator requires two numbers or decimals" error.
    }

    // Int-only operators (NUM §3.2): bitwise (`& | ^`), shift (`<< >>`), and
    // wrapping (`+% -% *%`) reject a `float` operand BEFORE the promoting numeric
    // dispatch — a float can never participate. A float operand → the Tier-2 type
    // panic (`bitwise op requires int operands, got float` / the wrapping/shift
    // equivalents). This runs ahead of the `(Int,Float)`/`(Float,_)` arms so those
    // never see an int-only op. A non-number operand falls through to the generic
    // "operator requires two numbers" error below.
    if is_int_only_binop(op)
        && (matches!(l.kind(), ValueKind::Float(_)) || matches!(r.kind(), ValueKind::Float(_)))
    {
        return Err(AsError::at(int_only_float_msg(op), span).into());
    }

    // Type-directed numeric dispatch (NUM §3.2):
    //  - Int ⊕ Int      → the checked/truncating int table (`int_binop`).
    //  - Int ⊕ Float    → promote the int to f64, result is Float (ordering exact).
    //  - Float ⊕ Float  → the IEEE float path.
    // Comparison across {Int,Float} is EXACT (no lossy cast) per NUM §3.3.
    match (l.kind(), r.kind()) {
        (ValueKind::Int(a), ValueKind::Int(b)) => int_binop(op, a, b, span),
        // Mixed int/float: arithmetic promotes the int to float; ordering stays
        // exact (compare i64 against f64 without precision loss).
        (ValueKind::Int(i), ValueKind::Float(f)) => mixed_binop(op, i, f, false),
        (ValueKind::Float(f), ValueKind::Int(i)) => mixed_binop(op, i, f, true),
        (ValueKind::Float(a), ValueKind::Float(b)) => Ok(float_binop(op, a, b)),
        _ => Err(AsError::at(
            format!(
                "operator requires two numbers (or two decimals, or number and decimal), got {} and {}",
                type_name(&l),
                type_name(&r)
            ),
            span,
        )
        .into()),
    }
}

/// `true` for the int-ONLY binary operators (NUM §3.2): bitwise (`& | ^`), shift
/// (`<< >>`), and wrapping (`+% -% *%`). These reject a `float` operand outright —
/// promotion never applies. Shared by `apply_binop`'s pre-dispatch guard.
fn is_int_only_binop(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr
            | BinOp::WrapAdd
            | BinOp::WrapSub
            | BinOp::WrapMul
    )
}

/// The Tier-2 panic message when an int-only operator (NUM §3.2) sees a `float`
/// operand. Bitwise/shift use the spec's `bitwise op requires int operands, got
/// float`; wrapping uses the parallel wrapping message. Shared by both engines so
/// the diagnostic is byte-identical.
fn int_only_float_msg(op: BinOp) -> String {
    match op {
        BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul => {
            "wrapping op requires int operands, got float".to_string()
        }
        _ => "bitwise op requires int operands, got float".to_string(),
    }
}

/// `int ⊕ int` arithmetic and comparison (NUM §3.2/§3.3). This is the SINGLE
/// source of truth shared by the tree-walker and (via the generic `apply_binop`
/// fallback) the VM; the VM's specialized `ArithKind::Int` fast path MUST be
/// byte-identical to it, including which inputs panic.
///
/// Arithmetic overflow / division-or-remainder-by-zero are recoverable Tier-2
/// panics raised through the same `AsError::at(..).into()` path every other
/// `apply_binop` panic uses.
pub(crate) fn int_binop(op: BinOp, a: i64, b: i64, span: Span) -> Result<Value, Control> {
    let overflow =
        |o: &str| -> Control { AsError::at(format!("integer overflow in '{o}'"), span).into() };
    let result = match op {
        BinOp::Add => Value::int(a.checked_add(b).ok_or_else(|| overflow("+"))?),
        BinOp::Sub => Value::int(a.checked_sub(b).ok_or_else(|| overflow("-"))?),
        BinOp::Mul => Value::int(a.checked_mul(b).ok_or_else(|| overflow("*"))?),
        BinOp::Div => {
            if b == 0 {
                return Err(AsError::at("integer division by zero", span).into());
            }
            // `checked_div` is `None` only for `i64::MIN / -1` (overflow). Truncates
            // toward zero (NUM §3.2): `7/2==3`, `-7/2==-3`.
            Value::int(a.checked_div(b).ok_or_else(|| overflow("/"))?)
        }
        BinOp::Mod => {
            if b == 0 {
                return Err(AsError::at("integer remainder by zero", span).into());
            }
            // `checked_rem` is `None` only for `i64::MIN % -1` (overflow). Sign
            // follows the dividend (`-7 % 2 == -1`).
            Value::int(a.checked_rem(b).ok_or_else(|| overflow("%"))?)
        }
        BinOp::Pow => int_pow(a, b, span)?,
        // Comparison: int vs int is trivially exact.
        BinOp::Lt => Value::bool_(a < b),
        BinOp::Le => Value::bool_(a <= b),
        BinOp::Gt => Value::bool_(a > b),
        BinOp::Ge => Value::bool_(a >= b),
        // Bitwise (NUM §3.2): int two's-complement. Never overflow-trap.
        BinOp::BitAnd => Value::int(a & b),
        BinOp::BitOr => Value::int(a | b),
        BinOp::BitXor => Value::int(a ^ b),
        // Shifts (NUM §3.2): the rhs is the shift AMOUNT. `checked_shl`/`checked_shr`
        // return `None` ONLY when the amount is out of range (`< 0` or `>= 64`) —
        // bit-loss (e.g. `1 << 63 == i64::MIN`, `-1 << 1 == -2`) is a defined result,
        // not an overflow. `>>` is arithmetic (sign-extending) since `a` is `i64`.
        BinOp::Shl => Value::int(int_shift(a, b, true, span)?),
        BinOp::Shr => Value::int(int_shift(a, b, false, span)?),
        // Wrapping (NUM §3.2): two's-complement, never panic.
        BinOp::WrapAdd => Value::int(a.wrapping_add(b)),
        BinOp::WrapSub => Value::int(a.wrapping_sub(b)),
        BinOp::WrapMul => Value::int(a.wrapping_mul(b)),
        BinOp::Eq | BinOp::Ne | BinOp::Range | BinOp::InstanceOf => {
            unreachable!("handled above apply_binop's numeric dispatch")
        }
        BinOp::And | BinOp::Or | BinOp::Coalesce => {
            unreachable!("short-circuit ops are not dispatched through apply_binop")
        }
    };
    Ok(result)
}

/// `int << amount` / `int >> amount` (NUM §3.2). The single source of truth shared
/// by the tree-walker and the VM. The shift AMOUNT (`b`) must be `0..64`; an amount
/// `< 0` or `>= 64` is a recoverable Tier-2 panic (`shift amount out of range: <n>`),
/// matching `i64::checked_shl`/`checked_shr` semantics. Bit-loss does NOT trap:
/// `1 << 63 == i64::MIN`, `-1 << 1 == -2`. `>>` is arithmetic (sign-extending)
/// because `a` is a signed `i64`.
pub(crate) fn int_shift(a: i64, b: i64, left: bool, span: Span) -> Result<i64, Control> {
    // The amount must be a valid `u32` in `0..64`; `b < 0` (a negative amount) and
    // `b >= 64` both fail the `u32::try_from` + `checked_sh*` guard.
    let amount = u32::try_from(b).ok().filter(|n| *n < 64);
    let shifted = amount.and_then(|n| if left { a.checked_shl(n) } else { a.checked_shr(n) });
    match shifted {
        Some(v) => Ok(v),
        None => Err(AsError::at(format!("shift amount out of range: {b}"), span).into()),
    }
}

/// `int ** int` (NUM §3.2): a non-negative exponent ≤ `u32::MAX` uses
/// `i64::checked_pow` (overflow → panic); a negative exponent or an exponent
/// `> u32::MAX` computes as `float` via `powf` (the result is defined, never a
/// truncated-exponent wrong int).
fn int_pow(base: i64, exp: i64, span: Span) -> Result<Value, Control> {
    if (0..=i64::from(u32::MAX)).contains(&exp) {
        match base.checked_pow(exp as u32) {
            Some(v) => Ok(Value::int(v)),
            None => Err(AsError::at("integer overflow in '**'", span).into()),
        }
    } else {
        // Negative exponent OR exponent > u32::MAX → float result.
        Ok(Value::float((base as f64).powf(exp as f64)))
    }
}

/// `float ⊕ float` arithmetic and comparison (the IEEE path; unchanged from the
/// pre-NUM behavior). Never panics — IEEE handles `/0`, `NaN`, `inf`.
fn float_binop(op: BinOp, a: f64, b: f64) -> Value {
    match op {
        BinOp::Add => Value::float(a + b),
        BinOp::Sub => Value::float(a - b),
        BinOp::Mul => Value::float(a * b),
        BinOp::Div => Value::float(a / b),
        BinOp::Mod => Value::float(a % b),
        BinOp::Pow => Value::float(a.powf(b)),
        BinOp::Lt => Value::bool_(a < b),
        BinOp::Le => Value::bool_(a <= b),
        BinOp::Gt => Value::bool_(a > b),
        BinOp::Ge => Value::bool_(a >= b),
        BinOp::Eq | BinOp::Ne | BinOp::Range | BinOp::InstanceOf => {
            unreachable!("handled above apply_binop's numeric dispatch")
        }
        // Int-only ops (bitwise/shift/wrapping) never reach the float path: a float
        // operand is rejected by `apply_binop`'s int-only guard before dispatch.
        BinOp::BitAnd
        | BinOp::BitOr
        | BinOp::BitXor
        | BinOp::Shl
        | BinOp::Shr
        | BinOp::WrapAdd
        | BinOp::WrapSub
        | BinOp::WrapMul => {
            unreachable!("int-only op rejected before the float path (apply_binop guard)")
        }
        BinOp::And | BinOp::Or | BinOp::Coalesce => {
            unreachable!("short-circuit ops are not dispatched through apply_binop")
        }
    }
}

/// Mixed `int`/`float` (NUM §3.2/§3.3). For arithmetic, the int is promoted to
/// f64 and the float path runs (result is Float). For ORDERING, the comparison is
/// EXACT (no lossy `i as f64`): `int_cmp_float` compares the i64 against the f64
/// without precision loss. `int_first` is `true` when the float was the left
/// operand (so the operand order — and thus `<`/`>` direction — is preserved).
fn mixed_binop(op: BinOp, i: i64, f: f64, float_first: bool) -> Result<Value, Control> {
    use std::cmp::Ordering;
    // Exact ordering for the comparison operators. `ord` is the ordering of the
    // INT relative to the FLOAT; flip it when the float was the left operand.
    let cmp = |want_lt: bool, want_eq: bool, want_gt: bool| -> Value {
        match crate::value::int_cmp_float(i, f) {
            None => Value::bool_(false), // NaN is unordered: every `<`,`<=`,`>`,`>=` is false.
            Some(mut ord) => {
                if float_first {
                    ord = ord.reverse();
                }
                let hit = match ord {
                    Ordering::Less => want_lt,
                    Ordering::Equal => want_eq,
                    Ordering::Greater => want_gt,
                };
                Value::bool_(hit)
            }
        }
    };
    match op {
        BinOp::Lt => Ok(cmp(true, false, false)),
        BinOp::Le => Ok(cmp(true, true, false)),
        BinOp::Gt => Ok(cmp(false, false, true)),
        BinOp::Ge => Ok(cmp(false, true, true)),
        // Arithmetic: promote int → float, preserving operand order. The
        // resulting `float_binop` cannot reach its `unreachable!` arms — Eq/Ne/
        // Range/InstanceOf and the short-circuit ops are handled before the
        // numeric dispatch, and the comparison ops are handled above.
        _ => {
            let (a, b) = if float_first { (f, i as f64) } else { (i as f64, f) };
            Ok(float_binop(op, a, b))
        }
    }
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
    match (l.kind(), r.kind()) {
        // Decimal vs Decimal: use the inner value's own equality.
        (ValueKind::Decimal(a), ValueKind::Decimal(b)) => Ok(a == b),
        // NUM §4: Decimal vs Int — the int converts EXACTLY.
        (ValueKind::Decimal(a), ValueKind::Int(i)) | (ValueKind::Int(i), ValueKind::Decimal(a)) => {
            Ok(**a == rust_decimal::Decimal::from(i))
        }
        // Decimal vs Float (or vice-versa): coerce the number to decimal.
        (ValueKind::Decimal(a), ValueKind::Float(n)) | (ValueKind::Float(n), ValueKind::Decimal(a)) => {
            if !n.is_finite() {
                // A non-finite float can never equal a finite decimal (lenient
                // false; the ordering path panics instead — see fn doc comment).
                return Ok(false);
            }
            use rust_decimal::prelude::FromPrimitive;
            let b = rust_decimal::Decimal::from_f64(n).ok_or_else(|| {
                AsError::at("cannot convert number to decimal for comparison", span)
            })?;
            Ok(**a == b)
        }
        // All other pairs: generic structural equality.
        _ => Ok(l == r),
    }
}

fn array_index(v: &Value, span: Span) -> Result<usize, AsError> {
    // NUM §4: `Int` is the common (and canonical) index. A `Float` index is accepted
    // only when it is exactly integral (e.g. `arr[2.0]`); a non-integral float is the
    // Tier-2 panic `array index must be an int, got float`. Any number must be
    // non-negative; a non-number is `array index must be a number`.
    match v.kind() {
        ValueKind::Int(i) => {
            if i >= 0 {
                Ok(i as usize)
            } else {
                Err(AsError::at(
                    "array index must be a non-negative integer",
                    span,
                ))
            }
        }
        ValueKind::Float(n) => {
            if n.fract() != 0.0 {
                Err(AsError::at("array index must be an int, got float", span))
            } else if n >= 0.0 {
                Ok(n as usize)
            } else {
                Err(AsError::at(
                    "array index must be a non-negative integer",
                    span,
                ))
            }
        }
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
    match obj.kind() {
        ValueKind::Array(arr) => {
            let i = array_index(idx, index_span)?;
            let arr = arr.borrow();
            arr.get(i).cloned().ok_or_else(|| {
                AsError::at(
                    format!("index {} out of bounds (len {})", i, arr.len()),
                    index_span,
                )
            })
        }
        ValueKind::Object(map) => match idx.kind() {
            ValueKind::Str(key) => Ok(map.get(key.as_ref()).unwrap_or(Value::nil())),
            _ => Err(AsError::at("object index must be a string", index_span)),
        },
        // SRV §3.5: a frozen `Shared` indexes like the data it froze — an array by
        // int → the child sub-node (re-wrapped as `Shared` if a container, else a
        // materialized scalar); an object/map by key → the child; OOB/missing → nil.
        // Descending stays zero-copy (a sub-container reads as a `Shared` view).
        ValueKind::Shared(node) => shared_index_get(node, idx, obj_span, index_span),
        _ => Err(AsError::at("cannot index this value", obj_span)),
    }
}

/// Materialize a frozen child node as a `Value`: a scalar is cloned out cheaply, a
/// container stays zero-copy by re-wrapping its `Arc` as a fresh `Value::shared`.
pub(crate) fn shared_child_to_value(child: &crate::value::SharedValue) -> Value {
    use crate::value::SharedNode;
    match &**child {
        SharedNode::Nil => Value::nil(),
        SharedNode::Bool(b) => Value::bool_(*b),
        SharedNode::Int(i) => Value::int(*i),
        SharedNode::Float(f) => Value::float(*f),
        SharedNode::Decimal(d) => Value::decimal(*d),
        SharedNode::Str(s) => Value::str(Rc::from(&**s)),
        // Containers (and the opaque Regex/EnumVariant/Instance frozen nodes) stay
        // shared — re-wrap the SAME `Arc` (a pointer bump, no copy).
        _ => Value::shared(child.clone()),
    }
}

/// Materialize ONE level of a frozen `SharedNode` into a LIVE `Value` whose children
/// are `Value::shared` views (an `Arc` bump per child, never a deep copy). The std
/// serializers (json / msgpack / cbor / log's `to_json_lossy`) walk `Value`, so a raw
/// `Value::shared` otherwise hits their catch-all and a frozen container fails to
/// serialize (and poisons any live container that holds it). With this, a serializer
/// detects `Value::shared`, materializes one level, and recurses — re-materializing
/// each deeper level on demand (frozen graphs are acyclic by construction, so this
/// terminates without a `seen` guard).
///
/// Returns `None` for the node kinds that have no live-container analogue we can
/// faithfully rebuild — `Instance` (needs its `Class`), `EnumVariant`, `Regex` — so
/// the caller can fall back to its OWN live-value semantics for those (e.g. json's
/// `from_ascript` errors on a live instance, while `to_json_lossy` renders its
/// fields). This keeps a frozen value serializing byte-identically to its live kind.
///
/// Gated on `data` (json/yaml/toml + the log `to_json_lossy`); `binary` (msgpack/cbor)
/// depends on `data`, so this matches the union of all callers — no dead code under
/// `--no-default-features`.
#[cfg(feature = "data")]
pub(crate) fn shared_to_value_shallow(node: &crate::value::SharedNode) -> Option<Value> {
    use crate::value::SharedNode;
    Some(match node {
        SharedNode::Nil => Value::nil(),
        SharedNode::Bool(b) => Value::bool_(*b),
        SharedNode::Int(i) => Value::int(*i),
        SharedNode::Float(f) => Value::float(*f),
        SharedNode::Decimal(d) => Value::decimal(*d),
        SharedNode::Str(s) => Value::str(Rc::from(&**s)),
        SharedNode::Bytes(b) => Value::bytes(b.to_vec()),
        SharedNode::Array(a) => {
            Value::array(a.iter().map(shared_child_to_value).collect())
        }
        SharedNode::Object(o) => {
            let mut m = indexmap::IndexMap::with_capacity(o.len());
            for (k, v) in o.iter() {
                m.insert(k.to_string(), shared_child_to_value(v));
            }
            Value::object(m)
        }
        SharedNode::Map(mp) => {
            let mut m = indexmap::IndexMap::with_capacity(mp.len());
            for (k, v) in mp.iter() {
                m.insert(k.to_map_key(), shared_child_to_value(v));
            }
            Value::map(m)
        }
        SharedNode::Set(s) => {
            let mut set = indexmap::IndexSet::with_capacity(s.len());
            for k in s.iter() {
                set.insert(k.to_map_key());
            }
            Value::set(set)
        }
        SharedNode::Instance { .. }
        | SharedNode::EnumVariant { .. }
        | SharedNode::Regex { .. } => return None,
    })
}

/// Index-read into a frozen `Shared` node (SRV §3.5). Returns a scalar `Value` or a
/// `Value::shared` sub-view; an out-of-range/missing key is `nil` (matching the live
/// Array/Object/Map semantics).
pub(crate) fn shared_index_get(
    node: &crate::value::SharedNode,
    idx: &Value,
    obj_span: Span,
    index_span: Span,
) -> Result<Value, AsError> {
    use crate::value::{SharedKey, SharedNode};
    match node {
        SharedNode::Array(arr) => {
            let i = array_index(idx, index_span)?;
            arr.get(i).map(shared_child_to_value).ok_or_else(|| {
                AsError::at(
                    format!("index {} out of bounds (len {})", i, arr.len()),
                    index_span,
                )
            })
        }
        SharedNode::Object(map) | SharedNode::Instance { fields: map, .. } => match idx.kind() {
            ValueKind::Str(key) => Ok(map
                .iter()
                .find(|(k, _)| &**k == key.as_ref())
                .map(|(_, v)| shared_child_to_value(v))
                .unwrap_or(Value::nil())),
            _ => Err(AsError::at("object index must be a string", index_span)),
        },
        SharedNode::Map(map) => {
            let Some(key) = crate::value::MapKey::from_value(idx) else {
                return Ok(Value::nil());
            };
            let skey = SharedKey::from_map_key(&key);
            Ok(map
                .iter()
                .find(|(k, _)| *k == skey)
                .map(|(_, v)| shared_child_to_value(v))
                .unwrap_or(Value::nil()))
        }
        _ => Err(AsError::at("cannot index this value", obj_span)),
    }
}

/// Member-read into a frozen `Shared` node (SRV §3.5). A frozen object/instance
/// field → its child (Shared view / scalar); a frozen regex `.source` → the source
/// string; a missing field on an object/instance → nil (matching `Object`). A
/// member read on a frozen scalar/array/map/set is an error (no fields).
pub(crate) fn shared_read_member(
    node: &crate::value::SharedNode,
    name: &str,
    span: Span,
) -> Result<Value, AsError> {
    use crate::value::SharedNode;
    match node {
        SharedNode::Object(map) | SharedNode::Instance { fields: map, .. } => Ok(map
            .iter()
            .find(|(k, _)| &**k == name)
            .map(|(_, v)| shared_child_to_value(v))
            .unwrap_or(Value::nil())),
        SharedNode::EnumVariant {
            enum_name,
            name: variant,
            value,
        } => match name {
            "name" => Ok(Value::str(Rc::from(&**variant))),
            "value" => Ok(shared_child_to_value(value)),
            // Named-payload field-access sugar: read the field off the frozen payload
            // object (mirroring the live `EnumVariant` `.field` sugar).
            other => {
                if let SharedNode::Object(fields) = &**value {
                    if let Some((_, fv)) = fields.iter().find(|(k, _)| &**k == other) {
                        return Ok(shared_child_to_value(fv));
                    }
                }
                Err(AsError::at(
                    format!("enum variant {} has no property '{}'", enum_name, other),
                    span,
                ))
            }
        },
        SharedNode::Regex { source } => match name {
            "source" => Ok(Value::str(Rc::from(&**source))),
            other => Err(AsError::at(
                format!("frozen regex has no property '{}' (try 'source')", other),
                span,
            )),
        },
        _ => Err(AsError::at(
            format!("cannot read property '{}' of this value", name),
            span,
        )),
    }
}

/// The element count of a frozen container (`len()` of a `Shared`), or `None` if the
/// frozen node is not a lengthed kind (scalar/regex/enum-variant). SRV §3.5.
pub(crate) fn shared_len(node: &crate::value::SharedNode) -> Option<usize> {
    use crate::value::SharedNode;
    match node {
        SharedNode::Str(s) => Some(s.chars().count()),
        SharedNode::Bytes(b) => Some(b.len()),
        SharedNode::Array(a) => Some(a.len()),
        SharedNode::Object(o) => Some(o.len()),
        SharedNode::Map(m) => Some(m.len()),
        SharedNode::Set(s) => Some(s.len()),
        SharedNode::Instance { fields, .. } => Some(fields.len()),
        _ => None,
    }
}

/// The set of MUTATING method names that are a Tier-2 panic on a frozen `Shared`
/// (SRV §3.8). A frozen value is immutable, so any of these → `cannot mutate a
/// frozen {kind}` (the shipped `frozen_kind` message, NO bespoke string).
fn is_shared_mutating_method(name: &str) -> bool {
    matches!(
        name,
        "push"
            | "pop"
            | "shift"
            | "unshift"
            | "set"
            | "insert"
            | "delete"
            | "remove"
            | "clear"
            | "sort"
            | "reverse"
            | "add"
            | "extend"
            | "splice"
            | "fill"
    )
}

/// SRV §3.5/§3.8 — dispatch a member-CALL on a frozen `Value::shared`. The
/// read-only method surface (`has`/`get`/`keys`/`values`/`contains`/`len`) reads the
/// frozen tree; a MUTATING method name is the shipped `cannot mutate a frozen {kind}`
/// Tier-2 panic (§3.8); a frozen-INSTANCE user-method call gets the DISTINCT
/// `method '<name>' is not available on a frozen instance ...` diagnostic (§3.8).
pub(crate) fn call_shared(
    node: &crate::value::SharedNode,
    name: &str,
    args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    use crate::value::{MapKey, SharedKey, SharedNode};

    // §3.8: a mutating method on ANY frozen container → the shipped frozen-mutation
    // panic (reusing `frozen_kind`'s `{kind}`, no divergent wording).
    if is_shared_mutating_method(name) {
        let kind = node.mutable_container_kind().unwrap_or("value");
        return Err(AsError::at(format!("cannot mutate a frozen {}", kind), span).into());
    }

    // The read-only structural method set works on the frozen container's data.
    match name {
        "len" | "size" => {
            if let Some(n) = shared_len(node) {
                return Ok(Value::int(n as i64));
            }
        }
        "has" => {
            let key = args.first().cloned().unwrap_or(Value::nil());
            return Ok(Value::bool_(shared_has_key(node, &key)));
        }
        "contains" => {
            let key = args.first().cloned().unwrap_or(Value::nil());
            // Set membership (or array containment).
            return Ok(Value::bool_(shared_contains(node, &key)));
        }
        "get" => {
            // `get(k, default?)` — frozen object/map field, else the default (nil).
            let key = args.first().cloned().unwrap_or(Value::nil());
            let default = args.get(1).cloned().unwrap_or(Value::nil());
            let found = match node {
                SharedNode::Object(map) | SharedNode::Instance { fields: map, .. } => match key.kind() {
                    ValueKind::Str(s) => map
                        .iter()
                        .find(|(k, _)| &**k == s.as_ref())
                        .map(|(_, v)| shared_child_to_value(v)),
                    _ => None,
                },
                SharedNode::Map(map) => MapKey::from_value(&key).and_then(|mk| {
                    let sk = SharedKey::from_map_key(&mk);
                    map.iter()
                        .find(|(k, _)| *k == sk)
                        .map(|(_, v)| shared_child_to_value(v))
                }),
                _ => None,
            };
            return Ok(found.unwrap_or(default));
        }
        "keys" => {
            if let Some(keys) = shared_keys(node) {
                return Ok(Value::array(keys));
            }
        }
        "values" => {
            if let Some(vals) = shared_values(node) {
                return Ok(Value::array(vals));
            }
        }
        _ => {}
    }

    // §3.8: a user-method call on a frozen INSTANCE (methods are not shared across
    // isolates; freeze exposes fields only) — a DISTINCT diagnostic, NOT the mutation
    // panic (the user didn't try to write anything).
    if let SharedNode::Instance { .. } = node {
        return Err(AsError::at(
            format!(
                "method '{}' is not available on a frozen instance (methods are not \
                 shared across isolates; freeze exposes fields only)",
                name
            ),
            span,
        )
        .into());
    }

    Err(AsError::at(
        format!(
            "frozen {} has no method '{}'",
            node.kind_name(),
            name
        ),
        span,
    )
    .into())
}

/// Whether a frozen object/map/instance has `key` (the `.has(k)` method).
fn shared_has_key(node: &crate::value::SharedNode, key: &Value) -> bool {
    use crate::value::{MapKey, SharedKey, SharedNode};
    match node {
        SharedNode::Object(map) | SharedNode::Instance { fields: map, .. } => match key.kind() {
            ValueKind::Str(s) => map.iter().any(|(k, _)| &**k == s.as_ref()),
            _ => false,
        },
        SharedNode::Map(map) => MapKey::from_value(key)
            .map(|mk| {
                let sk = SharedKey::from_map_key(&mk);
                map.iter().any(|(k, _)| *k == sk)
            })
            .unwrap_or(false),
        SharedNode::Set(set) => MapKey::from_value(key)
            .map(|mk| {
                let sk = SharedKey::from_map_key(&mk);
                set.contains(&sk)
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// Whether a frozen set/array contains `key` (the `.contains(x)` method).
fn shared_contains(node: &crate::value::SharedNode, key: &Value) -> bool {
    use crate::value::{MapKey, SharedKey, SharedNode};
    match node {
        SharedNode::Set(set) => MapKey::from_value(key)
            .map(|mk| {
                let sk = SharedKey::from_map_key(&mk);
                set.contains(&sk)
            })
            .unwrap_or(false),
        SharedNode::Array(arr) => arr
            .iter()
            .any(|child| shared_child_to_value(child) == *key),
        _ => false,
    }
}

/// The `.keys()` of a frozen object/map (as a fresh `array`), or `None`.
fn shared_keys(node: &crate::value::SharedNode) -> Option<Vec<Value>> {
    use crate::value::SharedNode;
    match node {
        SharedNode::Object(map) | SharedNode::Instance { fields: map, .. } => {
            Some(map.iter().map(|(k, _)| Value::str(Rc::from(&**k))).collect())
        }
        SharedNode::Map(map) => Some(map.iter().map(|(k, _)| k.to_value()).collect()),
        _ => None,
    }
}

/// The `.values()` of a frozen object/map (as a fresh `array` of Shared/scalars).
fn shared_values(node: &crate::value::SharedNode) -> Option<Vec<Value>> {
    use crate::value::SharedNode;
    match node {
        SharedNode::Object(map) | SharedNode::Instance { fields: map, .. } => {
            Some(map.iter().map(|(_, v)| shared_child_to_value(v)).collect())
        }
        SharedNode::Map(map) => Some(map.iter().map(|(_, v)| shared_child_to_value(v)).collect()),
        _ => None,
    }
}

/// The `for ... of` element sequence over a frozen container (SRV §3.5): a frozen
/// array yields its elements (as `Shared`/scalar), a frozen string its chars, a
/// frozen set its elements. Returns `None` for a non-iterable frozen node.
pub(crate) fn shared_iter_values(node: &crate::value::SharedNode) -> Option<Vec<Value>> {
    use crate::value::SharedNode;
    match node {
        SharedNode::Array(a) => Some(a.iter().map(shared_child_to_value).collect()),
        SharedNode::Str(s) => Some(
            s.chars()
                .map(|c| Value::str(c.to_string()))
                .collect(),
        ),
        SharedNode::Set(s) => Some(s.iter().map(|k| k.to_value()).collect()),
        _ => None,
    }
}

/// Pure index-write dispatch (`obj[idx] = value`) shared by the tree-walker
/// (`assign_to`'s `Index` arm) and the bytecode VM (`Op::SetIndex`) so the two
/// engines cannot drift on index-assignment semantics or panic messages. There
/// is one implementation. Returns the assigned value (assignment is an
/// expression).
///
/// Semantics (mirroring the original inline `assign_to` arm exactly):
/// - `Array`: the index must be a non-negative integer `Number` (via
///   [`array_index`], anchored at `index_span`); an out-of-bounds index is a
///   Tier-2 panic (arrays do NOT grow), `"index {i} out of bounds (len {n})"`
///   at `index_span`.
/// - `Object`: the index must be a `Str` key (a new key is inserted); a
///   non-string index panics `"object index must be a string"` at `index_span`.
/// - anything else: `"cannot index-assign a non-array value"` at `obj_span`.
///
/// `obj_span` is the receiver's span (the tree-walker's `object.span`);
/// `index_span` is the whole index-expression's span (the tree-walker's
/// `target.span`). The VM passes its single instruction span for both.
pub(crate) fn index_set(
    obj: &Value,
    idx: &Value,
    value: Value,
    obj_span: Span,
    index_span: Span,
) -> Result<Value, AsError> {
    if let Some(kind) = crate::value::frozen_kind(obj) {
        return Err(AsError::at(
            format!("cannot mutate a frozen {}", kind),
            index_span,
        ));
    }
    match obj.kind() {
        ValueKind::Array(arr) => {
            let i = array_index(idx, index_span)?;
            let mut arr = arr.borrow_mut();
            if i >= arr.len() {
                return Err(AsError::at(
                    format!("index {} out of bounds (len {})", i, arr.len()),
                    index_span,
                ));
            }
            arr[i] = value.clone();
            Ok(value)
        }
        ValueKind::Object(map) => match idx.kind() {
            ValueKind::Str(key) => {
                map.borrow_mut().insert(key.to_string(), value.clone());
                Ok(value)
            }
            _ => Err(AsError::at("object index must be a string", index_span)),
        },
        _ => Err(AsError::at(
            "cannot index-assign a non-array value",
            obj_span,
        )),
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
            WsConnection => return Some("recv"),
            SseStream => return Some("next"),
            _ => {}
        }
    }
    #[cfg(feature = "ai")]
    {
        use crate::value::NativeKind::*;
        if matches!(kind, AiStream | AiTextStream) {
            return Some("next");
        }
    }
    let _ = kind;
    None
}

/// Human-readable type name for diagnostics.
/// Human-readable message for a Tier-1 error value. If `err` is an Object with a
/// `message` field, that field's value is rendered; otherwise the whole value is.
/// Single source of truth shared by `expr!` (Unwrap) and `for await` error paths.
pub(crate) fn error_message(err: &Value) -> String {
    match err.kind() {
        ValueKind::Object(o) => o
            .get("message")
            .map(|m| m.to_string())
            .unwrap_or_else(|| err.to_string()),
        _ => err.to_string(),
    }
}

/// FFI §4.5a: the dispatch name of a NAMED `worker fn` value, or `None` if `v` is not
/// a named worker fn. Handles both engine representations — a tree-walker
/// `Value::function` and a VM `Value::closure` (whose `proto` carries the name +
/// `is_worker`). `run_in_worker`'s first arg must resolve through this.
/// PAR §2.2: also used by `task.pmap`/`task.preduce` to validate and resolve the callback.
pub(crate) fn worker_fn_dispatch_name(v: &Value) -> Option<String> {
    match v.kind() {
        ValueKind::Function(f) if f.is_worker => f.name.as_ref().map(|n| n.to_string()),
        // A `static worker fn` is a class METHOD, not a top-level `worker fn` — it is not
        // shippable through the free-fn slice path (`class_name: None`), so it is NOT a
        // valid `run_in_worker` / `task.pmap` / `task.preduce` callback. Reject it at the
        // value gate (returns `None` → the caller's "expects a named `worker fn`" §2.2
        // panic) rather than letting it fail deep in `build_code_slice` with the internal
        // "not a top-level function" message. The VM carries the owning class on
        // `proto.owning_class`; the tree-walker already drops the worker flag on a static
        // `Class.method` access, so both engines reject identically (byte-identical panic).
        ValueKind::Closure(c) if c.proto.is_worker && c.proto.owning_class.is_none() => {
            c.proto.chunk.name.as_ref().map(|n| n.to_string())
        }
        _ => None,
    }
}

pub(crate) fn type_name(v: &Value) -> &'static str {
    match v.kind() {
        ValueKind::Nil => "nil",
        ValueKind::Bool(_) => "bool",
        ValueKind::Int(_) => "int",
        ValueKind::Float(_) => "float",
        ValueKind::Decimal(_) => "decimal",
        ValueKind::Str(_) => "string",
        ValueKind::Builtin(_) | ValueKind::Function(_) | ValueKind::Closure(_) => "function",
        ValueKind::Array(_) => "array",
        ValueKind::Object(_) => "object",
        ValueKind::Map(_) => "map",
        ValueKind::Set(_) => "set",
        ValueKind::Bytes(_) => "bytes",
        #[cfg(feature = "data")]
        ValueKind::Regex(_) => "regex",
        ValueKind::Native(n) => n.kind.type_name(),
        ValueKind::NativeMethod(_) => "function",
        ValueKind::Enum(_) => "enum",
        ValueKind::EnumVariant(_) => "enum variant",
        ValueKind::Class(_) => "class",
        ValueKind::Interface(_) => "interface",
        ValueKind::Instance(_) => "instance",
        ValueKind::BoundMethod(_) | ValueKind::Super(_) => "function",
        ValueKind::Future(_) => "future",
        ValueKind::Generator(_) => "generator",
        ValueKind::GeneratorMethod(..) => "function",
        ValueKind::ClassMethod(..) => "function",
        // SRV §3.5: a frozen `Shared` reports its UNDERLYING kind (a frozen routing
        // object is an `"object"`, a frozen array an `"array"`) — user code +
        // `instanceof` see it as the data it froze, not as a distinct "shared" type.
        ValueKind::Shared(n) => n.kind_name(),
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
/// returned slot holds the collected `Value::array_cell` of the trailing arguments.
///
/// This is the single source of truth for function-call argument checking; it is
/// shared by the tree-walker (`run_body`) and the bytecode VM (`vm::run` CALL) so
/// arity/contract/rest behavior — message wording AND span — is byte-identical
/// across both engines. `span` is the CALL-site span; `what` is the callee's
/// name/description (e.g. the function name, `"function"`, or a method name).
/// The outcome of [`check_call_args`]: the args bound into their param slots,
/// plus enough information for each engine to fill any OMITTED trailing defaulted
/// params in the callee frame (left-to-right, seeing earlier params).
///
/// `values` has length `params.len()`. Supplied positional args occupy
/// `0..supplied` (already contract-checked); the trailing missing-defaulted
/// positions (`defaults`) hold a placeholder `Value::nil()` to be OVERWRITTEN by
/// the engine's default evaluation; the rest param (last, if present) holds the
/// collected tail array. `supplied` is the count of supplied positional
/// (non-rest) args. `defaults` is the half-open range of param indices whose
/// default must be evaluated (empty when every positional param was supplied).
///
/// Defaults are NOT evaluated here: a default expression can reference earlier
/// params and run arbitrary code, so the engine (tree-walker `run_body` / VM
/// prologue) evaluates them in the callee frame. This keeps `check_call_args`
/// pure and synchronous.
pub(crate) struct BoundArgs {
    pub values: Vec<Value>,
    pub supplied: usize,
    pub defaults: std::ops::Range<usize>,
}

/// CALL §3.3: shared arity gate — the EXACT wording/branching used by both
/// `check_call_args` and `check_call_args_in_place`. "expected N" for exact
/// arity (no rest, no defaults), "at least N" or "at most N" otherwise.
/// Extracted so both callers carry byte-identical messages by construction.
fn check_call_arity(
    params: &[crate::ast::Param],
    n_positional: usize,
    has_rest: bool,
    min: usize,
    n_args: usize,
    span: Span,
    what: &str,
) -> Result<(), Control> {
    // Too few arguments.
    if n_args < min {
        // Preserve the EXACT pre-existing wording so goldens stay byte-identical:
        // exact-arity (no rest, no defaults → min == max == len) keeps the
        // "expected N" message; everything else (rest or defaults) uses "at least min".
        let msg = if !has_rest && min == params.len() {
            format!(
                "{} expected {} argument(s), got {}",
                what,
                params.len(),
                n_args
            )
        } else {
            format!(
                "{} expected at least {} argument(s), got {}",
                what, min, n_args
            )
        };
        return Err(AsError::at(msg, span).into());
    }
    // Too many arguments (only possible without a rest param, which is unbounded).
    if !has_rest && n_args > n_positional {
        let msg = if min == params.len() {
            // No defaults → exact arity; keep the existing wording.
            format!(
                "{} expected {} argument(s), got {}",
                what,
                params.len(),
                n_args
            )
        } else {
            format!(
                "{} expected at most {} argument(s), got {}",
                what, n_positional, n_args
            )
        };
        return Err(AsError::at(msg, span).into());
    }
    Ok(())
}

/// CALL §3.3: shared per-arg contract gate — the env-aware `check_type_env` /
/// `check_type` fallback + `contract_panic`. Extracted so both callers carry
/// byte-identical checks by construction.  Synchronous; never awaits.
fn check_param_contract(
    p: &crate::ast::Param,
    a: &Value,
    span: Span,
    interp: Option<&Interp>,
    env: Option<&Environment>,
) -> Result<(), Control> {
    if let Some(ty) = &p.ty {
        // IFACE: when the engine supplies its resolution env (BOTH engines do), a
        // `Type::Named` resolves env-aware (interface → structural `conforms`,
        // class → nominal); an env-less internal caller falls to the env-free
        // `check_type`, the exact pre-IFACE behavior — byte-identical for
        // non-interface code.
        let ok = match (interp, env) {
            (Some(itp), Some(e)) => itp.check_type_env(a, ty, e)?,
            _ => check_type(a, ty),
        };
        if !ok {
            // §6.3 paranoid: escalate if this call-site was proven.
            if let Some(itp) = interp {
                if let Some(e) = itp.maybe_paranoid_escalate(ty, a, span) {
                    return Err(e);
                }
            }
            return Err(contract_panic(ty, a, span));
        }
    }
    Ok(())
}

/// CALL §3: the borrowing twin of [`check_call_args`] — IDENTICAL arity and
/// contract checks over args already positioned on the operand stack, no Vec
/// consumed or produced. Caller guarantees `!has_rest` (rest collection
/// genuinely allocates a new tail array; those calls keep the Vec path).
///
/// Returns the **supplied** count (== `args.len()`, capped at `n_positional`).
/// The callee prologue (`Op::JumpIfArgSupplied`) uses `frame.argc` (set from
/// this value) to decide whether to evaluate each defaulted param.
pub(crate) fn check_call_args_in_place(
    params: &[crate::ast::Param],
    args: &[Value],
    span: Span,
    what: &str,
    interp: Option<&Interp>,
    env: Option<&Environment>,
    elide_contracts: bool,
) -> Result<usize, Control> {
    // Caller guarantees this; catch bugs early in debug builds.
    debug_assert!(!params.last().is_some_and(|p| p.rest));
    let n_positional = params.len(); // has_rest == false, so all params are positional
    let min = params
        .iter()
        .take_while(|p| p.default.is_none())
        .count();
    check_call_arity(params, n_positional, false, min, args.len(), span, what)?;
    // Contract-check the supplied args (left-to-right, same order as check_call_args).
    // ELIDE §4.4: per-param type checks are skipped when elide_contracts is true.
    if !elide_contracts {
        let supplied = args.len().min(n_positional);
        for (p, a) in params[..supplied].iter().zip(args[..supplied].iter()) {
            check_param_contract(p, a, span, interp, env)?;
        }
    }
    Ok(args.len().min(n_positional))
}

pub(crate) fn check_call_args(
    params: &[crate::ast::Param],
    args: Vec<Value>,
    span: Span,
    what: &str,
    // IFACE: the engine's env-aware contract resolution context. BOTH engines pass
    // `Some` (tree-walker: its `Environment` chains to the def env / module globals;
    // VM: `Some(&self.interp, &self.class_env())`, the shared module env where
    // `Op::DefineInterface`/`Op::Class` register descriptors) so an interface/class
    // `Type::Named` resolves and enforces conformance. The `Option` is retained for
    // env-less internal callers; `None` falls to the env-free `check_type` (the exact
    // pre-IFACE behavior, byte-identical for non-interface code).
    interp: Option<&Interp>,
    env: Option<&Environment>,
    // ELIDE §4.4: when true, skip per-param and rest-element type-contract checks.
    // Arity, defaults, and rest collection are NEVER skipped. Only CallElided dispatch
    // passes true; every other call site passes false.
    elide_contracts: bool,
) -> Result<BoundArgs, Control> {
    let has_rest = params.last().is_some_and(|p| p.rest);
    // Count of POSITIONAL params (excludes a trailing `...rest`).
    let n_positional = if has_rest {
        params.len() - 1
    } else {
        params.len()
    };
    // Min-arity = leading run of positional params with NO default (a required
    // param may not follow a defaulted one, enforced at parse/compile time, so
    // this is exactly the index of the first defaulted positional param).
    let min = params[..n_positional]
        .iter()
        .take_while(|p| p.default.is_none())
        .count();

    // Arity check — delegates to the shared core so both callers use identical wording.
    // ELIDE §4.4: arity is NEVER skipped (only type-contract checks are).
    check_call_arity(params, n_positional, has_rest, min, args.len(), span, what)?;

    let mut values: Vec<Value> = Vec::with_capacity(params.len());
    let mut it = args.into_iter();
    // Bind the supplied positional args (contract-checking each), capping at the
    // positional count so any surplus is collected by the rest param.
    let supplied = it.len().min(n_positional);
    for p in &params[..supplied] {
        let a = it.next().unwrap();
        // Per-param contract check — ELIDE §4.4: skipped when elide_contracts is true.
        if !elide_contracts {
            check_param_contract(p, &a, span, interp, env)?;
        }
        values.push(a);
    }
    // Placeholders for the omitted trailing defaulted positions (filled by the
    // engine in the callee frame).
    let defaults = supplied..n_positional;
    for _ in defaults.clone() {
        values.push(Value::nil());
    }
    // Collect the rest param's tail (any args beyond the positional count).
    if has_rest {
        let rest_p = &params[n_positional];
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
            // Rest-element type check — ELIDE §4.4: skipped when elide_contracts is true.
            // Note: eligibility excludes rest-param callees, so elide_contracts is never
            // true here in practice; the guard is kept for defence-in-depth.
            if !elide_contracts {
                if let Some(t) = elem_ty {
                    let ok = match (interp, env) {
                        (Some(itp), Some(e)) => itp.check_type_env(&a, t, e)?,
                        _ => check_type(&a, t),
                    };
                    if !ok {
                        return Err(contract_panic(t, &a, span));
                    }
                }
            }
            rest_vals.push(a);
        }
        values.push(Value::array(rest_vals,));
    }
    Ok(BoundArgs {
        values,
        supplied,
        defaults,
    })
}

/// Auto-derived positional constructor for a field-only class (SP2 §5, "records").
/// SHARED by both engines (`Interp::construct` and `Vm::vm_construct`) so the
/// arity check, error wording/span, and contract check cannot diverge.
///
/// `fields` is the class's ordered, merged-base-first field schema
/// (`merged_field_schema(class)`, iterated in declaration order). The auto-init
/// treats each field as a positional parameter: a field WITHOUT a default is a
/// required leading param; a field WITH a default is an optional trailing param.
/// Arity is validated with the SAME `check_call_args` logic used for functions —
/// `what` is the class name, so the message reads `"<Class> expected N
/// argument(s), got M"` (or `"at least"/"at most"` with defaults) and `span` is
/// the construct call site.
///
/// Returns the positional field bindings as `(field_name, value)` pairs for the
/// SUPPLIED args only (each already contract-checked against its field type).
/// Omitted trailing (defaulted) fields are NOT returned — the caller has already
/// applied their defaults to the fresh instance, and this auto-init only
/// OVERRIDES the supplied positions. Defaults are never evaluated here (the
/// synthesized params carry a sentinel default purely so `check_call_args`
/// computes the right min/max arity).
pub(crate) fn auto_init_bindings(
    fields: &indexmap::IndexMap<String, (crate::value::FieldSchema, std::rc::Rc<crate::value::Class>)>,
    class_name: &str,
    args: Vec<Value>,
    span: Span,
) -> Result<Vec<(String, Value)>, Control> {
    // Synthesize one positional `Param` per declared field, in declaration order.
    // A defaulted field carries a sentinel default expression so `check_call_args`
    // counts it toward `max` (total) but not `min` (required leading run); the
    // sentinel is NEVER evaluated (we only consume `supplied`).
    let params: Vec<crate::ast::Param> = fields
        .iter()
        .map(|(name, (schema, _))| crate::ast::Param {
            name: name.clone(),
            ty: Some(schema.ty.clone()),
            name_span: span,
            rest: false,
            default: if schema.default.is_some() {
                Some(crate::ast::Expr {
                    kind: crate::ast::ExprKind::Nil,
                    span,
                })
            } else {
                None
            },
        })
        .collect();
    // Reuse the function-call arity + per-arg contract logic verbatim so messages
    // and spans are byte-identical to a hand-written `init(x, y)`. This free fn has no
    // engine/env (constructor field contracts resolve names via validate_into / the
    // env-aware field-set path), so pass `None` — env-free `check_type` for any Named.
    let bound = check_call_args(&params, args, span, class_name, None, None, false)?;
    // Take only the supplied positional args (contract-checked by
    // `check_call_args`); pair each with its field name. Omitted defaulted fields
    // keep the default the caller already applied.
    let names: Vec<&String> = fields.keys().collect();
    Ok(bound
        .values
        .into_iter()
        .take(bound.supplied)
        .enumerate()
        .map(|(i, v)| (names[i].clone(), v))
        .collect())
}

pub(crate) fn check_type(value: &Value, ty: &crate::ast::Type) -> bool {
    use crate::ast::Type;
    match ty {
        Type::Any => true,
        // NUM §4/§5: `number` is the union `int | float`; `int`/`float` accept only
        // their own subtype.
        Type::Number => value.is_number(),
        Type::Int => matches!(value.kind(), ValueKind::Int(_)),
        Type::Float => matches!(value.kind(), ValueKind::Float(_)),
        Type::String => matches!(value.kind(), ValueKind::Str(_)),
        Type::Bool => matches!(value.kind(), ValueKind::Bool(_)),
        Type::Nil => matches!(value.kind(), ValueKind::Nil),
        Type::Object => matches!(value.kind(), ValueKind::Object(_)),
        // A VM-produced `Closure` is the bytecode analog of a tree-walker
        // `Function`; both are first-class callables, so `: fn` typing accepts
        // either. (The tree-walker never produces a `Closure`, so adding it here
        // is behavior-preserving for the tree-walker and closes a real contract
        // gap for the VM, which routes through this shared `check_type`.)
        Type::Fn => matches!(
            value.kind(),
            ValueKind::Function(_) | ValueKind::Closure(_) | ValueKind::Builtin(_)
        ),
        Type::Error => matches!(value.kind(), ValueKind::Object(_) | ValueKind::Nil),
        Type::Array(elem) => match value.kind() {
            ValueKind::Array(a) => a.borrow().iter().all(|v| check_type(v, elem)),
            _ => false,
        },
        Type::Result(inner) => match value.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                b.len() == 2
                    && (check_type(&b[0], inner) || matches!(b[0].kind(), ValueKind::Nil))
                    && check_type(&b[1], &Type::Error)
            }
            _ => false,
        },
        Type::Tuple(types) => match value.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                b.len() == types.len() && b.iter().zip(types.iter()).all(|(v, t)| check_type(v, t))
            }
            _ => false,
        },
        Type::Union(a, b) => check_type(value, a) || check_type(value, b),
        Type::Named(name) => match value.kind() {
            ValueKind::Instance(inst) => {
                let mut cur = Some(inst.borrow().class.clone());
                while let Some(c) = cur {
                    if &c.name == name {
                        return true;
                    }
                    cur = c.superclass.clone();
                }
                false
            }
            ValueKind::EnumVariant(v) => &v.enum_name == name,
            _ => false,
        },
        Type::Map(k, v) => match value.kind() {
            ValueKind::Map(m) => m
                .borrow()
                .iter()
                .all(|(mk, val)| check_type(&mk.to_value(), k) && check_type(val, v)),
            _ => false,
        },
        // A value satisfies `future<T>` iff it is a future. The inner `T` is the
        // type the future *resolves to*, which cannot be inspected until it is
        // awaited, so it is advisory/erased at the binding site.
        Type::Future(_) => matches!(value.kind(), ValueKind::Future(_)),
        // `T?` ≡ `T | nil`.
        Type::Optional(inner) => check_type(value, inner) || matches!(value.kind(), ValueKind::Nil),
        // TYPE §5.4: generics are RUNTIME-ERASED. A generic type PARAMETER (`T`)
        // carries no runtime obligation — it accepts every value, exactly like
        // `any`. The static checker enforces `T`'s consistency; the runtime does
        // not. (This is the ONLY behavioral surface generics expose at runtime, and
        // it is intentionally a no-op.)
        Type::Param(_) => true,
        // A parameterized function type (`fn(A) -> B`) is checked as a plain
        // callable at runtime — the param/return signature is erased (advisory,
        // static-only), so this is identical to `Type::Fn`.
        Type::FnSig(_, _) => matches!(
            value.kind(),
            ValueKind::Function(_) | ValueKind::Closure(_) | ValueKind::Builtin(_)
        ),
    }
}

/// IFACE §5.1: whether a concrete `method` can satisfy an interface requirement of
/// the given call-shape (`req`). The method conforms iff it can be CALLED with the
/// requirement's argument count:
///   `min_required <= req.arity <= declared_max`
/// where `min_required` counts params that are neither defaulted nor the rest param,
/// `declared_max` is the total params minus the rest param (or `∞` if the method has a
/// rest param). Additionally, a requirement that itself declares a rest param
/// (`req.has_rest`) requires the method to ALSO be variadic. Arity-only by design
/// (runtime-permissive; TYPE adds the static type tightening).
pub(crate) fn arity_compatible(
    method: &crate::value::Method,
    req: &crate::value::MethodReq,
) -> bool {
    arity_compatible_params(&method.params, req)
}

/// The param-slice core of [`arity_compatible`], shared by the tree-walker (a
/// `value::Method`) and the VM (a compiled `FnProto`'s params) so the two engines'
/// conformance verdicts cannot drift.
pub(crate) fn arity_compatible_params(
    params: &[crate::ast::Param],
    req: &crate::value::MethodReq,
) -> bool {
    let has_rest = params.iter().any(|p| p.rest);
    // A rest requirement needs a variadic method (only place `req.has_rest` matters).
    if req.has_rest && !has_rest {
        return false;
    }
    let min_required = params
        .iter()
        .filter(|p| !p.rest && p.default.is_none())
        .count();
    if req.arity < min_required {
        return false;
    }
    if has_rest {
        // Unbounded max — a rest param absorbs surplus args.
        return true;
    }
    let declared_max = params.iter().filter(|p| !p.rest).count();
    req.arity <= declared_max
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

    /// FFI §4.3: `require_cap` raises the recoverable Tier-2 denial panic when the
    /// cap is NOT granted, and passes when it is. Pure (no feature gate) — works in
    /// every config.
    #[test]
    fn require_cap_denies_when_dropped_and_allows_when_granted() {
        use crate::stdlib::caps::{Cap, CapSet};
        let interp = Interp::new();
        // Default = all granted → require passes.
        assert!(interp
            .require_cap(Cap::Ffi, "ffi", "open", &[], Span::new(0, 0))
            .is_ok());
        // Drop ffi → require raises the named denial panic, recoverable.
        let mut cs = CapSet::all_granted();
        cs.deny(Cap::Ffi);
        interp.set_caps(cs);
        match interp.require_cap(Cap::Ffi, "ffi", "open", &[], Span::new(0, 0)) {
            Err(Control::Panic(e)) => assert_eq!(e.message, "capability 'ffi' denied"),
            other => panic!("expected denial panic, got {other:?}"),
        }
        // A still-granted cap (net) still passes.
        assert!(interp
            .require_cap(Cap::Net, "net", "lookup", &[], Span::new(0, 0))
            .is_ok());
    }

    /// FFI §4.3 Gate-12: the default all-granted snapshot reports `all_granted()`
    /// (the single-flag short-circuit the gate uses to stay zero-cost).
    #[test]
    fn default_caps_are_all_granted_snapshot() {
        let interp = Interp::new();
        assert!(interp.caps_bits().all_granted());
    }

    /// FFI §4.3 end-to-end through the REAL dispatch site `call_stdlib`: with `env`
    /// dropped, a routed `env.get` call raises `capability 'env' denied` BEFORE the
    /// module body runs. This is the funnel both engines share, so byte-identity is
    /// by construction. DNS-egress (`net.lookup`) is the same dispatch-site
    /// mechanism (Task 9 e2e). A still-granted, ungated module is unaffected.
    #[cfg(feature = "sys")]
    #[tokio::test]
    async fn dropped_cap_denies_at_dispatch_site() {
        use crate::stdlib::caps::{Cap, CapSet};
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let mut cs = CapSet::all_granted();
        cs.deny(Cap::Env);
        interp.set_caps(cs);
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // `env` denied → the gate raises the recoverable denial panic.
                let denied = interp
                    .call_stdlib("env", "get", &[Value::str("PATH")], Span::new(0, 0))
                    .await;
                match denied {
                    Err(Control::Panic(e)) => {
                        assert_eq!(e.message, "capability 'env' denied")
                    }
                    other => panic!("expected env denial, got {other:?}"),
                }
                // A still-granted, ungated module (math) routes normally.
                let ok = interp
                    .call_stdlib("math", "abs", &[Value::int(-3)], Span::new(0, 0))
                    .await
                    .unwrap();
                assert_eq!(ok, Value::int(3));
            })
            .await;
    }

    /// FFI §4.3 [SECURITY]: DNS (`net.lookup`) is captured by the dispatch-site gate
    /// by construction — it routes through the `"net"` module string even though it
    /// is NOT a connect/bind site, so dropping `net` denies it. (Full end-to-end
    /// resolved-vs-denied value test is Task 9; this asserts the gate fires.)
    #[cfg(feature = "net")]
    #[tokio::test]
    async fn dropped_net_denies_dns_lookup() {
        use crate::stdlib::caps::{Cap, CapSet};
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let mut cs = CapSet::all_granted();
        cs.deny(Cap::Net);
        interp.set_caps(cs);
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let denied = interp
                    .call_stdlib(
                        "net",
                        "lookup",
                        &[Value::str("example.com")],
                        Span::new(0, 0),
                    )
                    .await;
                match denied {
                    Err(Control::Panic(e)) => {
                        assert_eq!(e.message, "capability 'net' denied")
                    }
                    other => panic!("expected net denial for DNS lookup, got {other:?}"),
                }
            })
            .await;
    }

    /// BLOCKER 2: `sqlite`/`postgres`/`redis` open OS resources (a DB file / a TCP
    /// socket) but were ABSENT from `required_cap`, so `--sandbox` left them reachable.
    /// With the matching cap dropped, the dispatch-site gate now denies them BEFORE
    /// any connect attempt — a clean recoverable Tier-2 panic naming the capability.
    #[cfg(any(feature = "sql", feature = "postgres", feature = "redis"))]
    #[tokio::test]
    async fn dropped_cap_denies_database_modules() {
        use crate::stdlib::caps::CapSet;
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        // `--sandbox` equivalent: drop every dangerous capability.
        let mut cs = CapSet::all_granted();
        cs.deny_all_dangerous();
        interp.set_caps(cs);
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // sqlite.open → Fs denied (would otherwise open/create a DB file).
                #[cfg(feature = "sql")]
                match interp
                    .call_stdlib("sqlite", "open", &[Value::str(":memory:")], Span::new(0, 0))
                    .await
                {
                    Err(Control::Panic(e)) => assert_eq!(e.message, "capability 'fs' denied"),
                    other => panic!("expected sqlite fs denial, got {other:?}"),
                }
                // postgres.connect → Net denied (would otherwise open a TCP socket).
                #[cfg(feature = "postgres")]
                match interp
                    .call_stdlib(
                        "postgres",
                        "connect",
                        &[Value::str("postgres://localhost/db")],
                        Span::new(0, 0),
                    )
                    .await
                {
                    Err(Control::Panic(e)) => assert_eq!(e.message, "capability 'net' denied"),
                    other => panic!("expected postgres net denial, got {other:?}"),
                }
                // redis.connect → Net denied (TCP egress).
                #[cfg(feature = "redis")]
                match interp
                    .call_stdlib(
                        "redis",
                        "connect",
                        &[Value::str("redis://localhost")],
                        Span::new(0, 0),
                    )
                    .await
                {
                    Err(Control::Panic(e)) => assert_eq!(e.message, "capability 'net' denied"),
                    other => panic!("expected redis net denial, got {other:?}"),
                }
            })
            .await;
    }

    /// FFI §4.4 Gate-12: with NO carve-out configured (`net_scope`/`fs_scope` are
    /// `None` — the default and the all-deny/all-grant cases), the stage-2 checks
    /// short-circuit to `Ok` with NO host comparison / NO path canonicalization.
    /// Proven by feeding a host/path that WOULD be denied under a carve-out and
    /// asserting it passes — the only way it can pass is if no comparison ran.
    #[test]
    fn stage2_checks_shortcircuit_when_no_carveout() {
        let interp = Interp::new();
        // No net carve-out → even a public address passes (no host comparison).
        assert!(interp.check_net_host("8.8.8.8", Span::new(0, 0)).is_ok());
        assert!(interp.check_net_host("example.com", Span::new(0, 0)).is_ok());
        // No fs carve-out → even a write to /etc passes (no canonicalization).
        assert!(interp
            .check_fs_path(std::path::Path::new("/etc/passwd"), true, Span::new(0, 0))
            .is_ok());
    }

    /// FFI §4.4 stage-2 enforcement (net): with a carve-out configured, the host
    /// is checked — loopback/allow-listed pass, public is denied with a host-named
    /// panic.
    #[test]
    fn stage2_net_carveout_enforces_allowlist() {
        use crate::stdlib::caps::{CapSet, NetDeny, NetScope};
        let interp = Interp::new();
        let mut cs = CapSet::all_granted();
        cs.set_net_scope(NetScope {
            deny: NetDeny::External,
            allow: vec!["api.internal".into()],
        });
        interp.set_caps(cs);
        // Loopback allowed.
        assert!(interp.check_net_host("127.0.0.1", Span::new(0, 0)).is_ok());
        // Allow-listed host allowed.
        assert!(interp.check_net_host("api.internal", Span::new(0, 0)).is_ok());
        // Public denied with a host-named message.
        match interp.check_net_host("8.8.8.8", Span::new(0, 0)) {
            Err(Control::Panic(e)) => {
                assert!(e.message.contains("net") && e.message.contains("8.8.8.8"), "{}", e.message)
            }
            other => panic!("expected net host denial, got {other:?}"),
        }
    }

    /// FFI §4.4: a `net` carve-out makes the dispatch gate DEFER (not deny-outright)
    /// so the stage-2 host check runs — verified end-to-end through `call_stdlib`
    /// for `net.lookup`: a public host is denied, loopback resolves.
    #[cfg(feature = "net")]
    #[tokio::test]
    async fn net_carveout_defers_then_stage2_denies_public_lookup() {
        use crate::stdlib::caps::{CapSet, NetDeny, NetScope};
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let mut cs = CapSet::all_granted();
        cs.set_net_scope(NetScope {
            deny: NetDeny::All,
            allow: vec!["localhost".into()],
        });
        interp.set_caps(cs);
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Public host: dispatch defers, stage-2 denies (host-named).
                let denied = interp
                    .call_stdlib(
                        "net",
                        "lookup",
                        &[Value::str("example.com")],
                        Span::new(0, 0),
                    )
                    .await;
                match denied {
                    Err(Control::Panic(e)) => assert!(
                        e.message.contains("example.com"),
                        "{}",
                        e.message
                    ),
                    other => panic!("expected stage-2 denial, got {other:?}"),
                }
                // localhost is allow-listed → resolves to a [ips, nil] pair.
                let ok = interp
                    .call_stdlib(
                        "net",
                        "lookup",
                        &[Value::str("localhost")],
                        Span::new(0, 0),
                    )
                    .await
                    .unwrap();
                if let ValueKind::Array(a) = ok.kind() {
                    assert_eq!(a.borrow()[1], Value::nil(), "lookup should succeed, err=nil");
                } else {
                    panic!("expected [ips, err] pair");
                }
            })
            .await;
    }

    /// BLOCKER 1: the net carve-out host allow-list was enforced ONLY at
    /// `tcp.connect`/`tcp.listen` + DNS — NOT at HTTP / UDP / WS / server. Under a
    /// `net = {deny:"all", allow:["localhost"]}` carve-out, an http/udp/ws/server
    /// op to a DISALLOWED host must be denied at the request/bind entry (host-named
    /// stage-2 panic) BEFORE any socket is opened. Hermetic: the gate fires before
    /// any real connect, so no network is needed.
    #[cfg(feature = "net")]
    #[tokio::test]
    async fn net_carveout_enforced_for_http_udp_ws_server() {
        use crate::stdlib::caps::{CapSet, NetDeny, NetScope};
        // deny=all, allow only loopback literal 127.0.0.1 (NOT "localhost", so a
        // 127.0.0.1 bind passes but any public host / hostname is denied).
        let make = || {
            let interp = std::rc::Rc::new(Interp::new());
            interp.install_self();
            let mut cs = CapSet::all_granted();
            cs.set_net_scope(NetScope {
                deny: NetDeny::All,
                allow: vec!["127.0.0.1".into()],
            });
            interp.set_caps(cs);
            interp
        };
        let denied = |msg: &str, host: &str| {
            assert!(
                msg.contains("net") && msg.contains(host),
                "expected host-named net denial for {host}, got: {msg}"
            );
        };
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // HTTP GET to a public host → denied (TCP egress to 8.8.8.8).
                let i = make();
                match i
                    .call_stdlib(
                        "net_http",
                        "get",
                        &[Value::str("http://8.8.8.8/")],
                        Span::new(0, 0),
                    )
                    .await
                {
                    Err(Control::Panic(e)) => denied(&e.message, "8.8.8.8"),
                    other => panic!("expected http net denial, got {other:?}"),
                }

                // UDP send-to a disallowed destination → denied.
                let i = make();
                // Bind is on the allowed loopback (passes), so this exercises send-to.
                let bind = i
                    .call_stdlib(
                        "net_udp",
                        "bind",
                        &[Value::str("127.0.0.1:0")],
                        Span::new(0, 0),
                    )
                    .await
                    .unwrap();
                let sock = if let ValueKind::Array(a) = bind.kind() {
                    a.borrow()[0].clone()
                } else {
                    panic!("bind should return a pair");
                };
                // Call `send(data, "8.8.8.8:53")` on the socket handle.
                let m = std::rc::Rc::new(crate::value::NativeMethod {
                    receiver: match sock.kind() {
                        ValueKind::Native(n) => n.clone(),
                        _ => panic!("expected a native udp socket"),
                    },
                    method: "send".into(),
                });
                match i
                    .call_native_method(
                        m,
                        vec![
                            Value::str("x"),
                            Value::str("8.8.8.8:53"),
                        ],
                        Span::new(0, 0),
                    )
                    .await
                {
                    Err(Control::Panic(e)) => denied(&e.message, "8.8.8.8"),
                    other => panic!("expected udp send net denial, got {other:?}"),
                }

                // WebSocket connect to a public host → denied.
                let i = make();
                match i
                    .call_stdlib(
                        "net_ws",
                        "connect",
                        &[Value::str("ws://8.8.8.8:9000/")],
                        Span::new(0, 0),
                    )
                    .await
                {
                    Err(Control::Panic(e)) => denied(&e.message, "8.8.8.8"),
                    other => panic!("expected ws net denial, got {other:?}"),
                }

                // HTTP server bind to a disallowed host (a non-loopback) → denied.
                let i = make();
                let server = i
                    .call_stdlib("http_server", "create", &[], Span::new(0, 0))
                    .await
                    .unwrap();
                let m = std::rc::Rc::new(crate::value::NativeMethod {
                    receiver: match server.kind() {
                        ValueKind::Native(n) => n.clone(),
                        _ => panic!("expected a server handle"),
                    },
                    method: "bind".into(),
                });
                match i
                    .call_native_method(
                        m,
                        vec![Value::str("0.0.0.0"), Value::int(0)],
                        Span::new(0, 0),
                    )
                    .await
                {
                    Err(Control::Panic(e)) => denied(&e.message, "0.0.0.0"),
                    other => panic!("expected server bind net denial, got {other:?}"),
                }
            })
            .await;
    }

    /// BLOCKER 3: operating an ALREADY-OPEN native handle must re-check the governing
    /// capability — `call_native_method` had ZERO cap checks, so a socket opened while
    /// `net` was granted kept working (and `accept()` even acquired NEW connections)
    /// after `caps.drop("net")`. Here: open a TCP listener (net granted), drop net,
    /// then `accept()` must be denied — "the drop holds".
    #[cfg(feature = "net")]
    #[tokio::test]
    async fn open_handle_rechecks_dropped_cap() {
        use crate::stdlib::caps::{Cap, CapSet};
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Open a listener while net is granted (hermetic loopback bind).
                let pair = interp
                    .call_stdlib(
                        "net_tcp",
                        "listen",
                        &[Value::str("127.0.0.1"), Value::int(0)],
                        Span::new(0, 0),
                    )
                    .await
                    .unwrap();
                let listener = if let ValueKind::Array(a) = pair.kind() {
                    assert_eq!(a.borrow()[1], Value::nil(), "listen should succeed");
                    a.borrow()[0].clone()
                } else {
                    panic!("listen should return a pair");
                };
                // Drop net AFTER the handle is open.
                let mut cs = CapSet::all_granted();
                cs.deny(Cap::Net);
                interp.set_caps(cs);
                // accept() on the open listener must now be denied — the per-handle
                // re-check fires before any new connection is acquired.
                let m = std::rc::Rc::new(crate::value::NativeMethod {
                    receiver: match listener.kind() {
                        ValueKind::Native(n) => n.clone(),
                        _ => panic!("expected a native listener"),
                    },
                    method: "accept".into(),
                });
                match interp.call_native_method(m, vec![], Span::new(0, 0)).await {
                    Err(Control::Panic(e)) => assert_eq!(e.message, "capability 'net' denied"),
                    other => panic!("expected net denial on accept after drop, got {other:?}"),
                }
            })
            .await;
    }

    /// BLOCKER 3: a handle whose `NativeKind` maps to NO capability (a pure in-memory
    /// native — e.g. an `events` emitter) stays UNGATED even after every cap is
    /// dropped, so dropping caps never breaks pure-compute handles (no over-deny).
    #[tokio::test]
    async fn ungated_handle_unaffected_by_drops() {
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // An events emitter is an in-memory handle (no OS resource).
                let emitter = interp
                    .call_stdlib("events", "new", &[], Span::new(0, 0))
                    .await
                    .unwrap();
                // Drop ALL caps.
                interp.caps_deny_all();
                // listenerCount() still works — the handle has no governing cap.
                let m = std::rc::Rc::new(crate::value::NativeMethod {
                    receiver: match emitter.kind() {
                        ValueKind::Native(n) => n.clone(),
                        _ => panic!("expected a native emitter"),
                    },
                    method: "listenerCount".into(),
                });
                let r = interp
                    .call_native_method(m, vec![Value::str("x")], Span::new(0, 0))
                    .await;
                assert!(r.is_ok(), "ungated handle must stay usable after dropAll: {r:?}");
            })
            .await;
    }

    /// FFI §4.4 fs carve-out end-to-end through `call_stdlib`: `deny="write",
    /// allow=[<tmp/cache>]` permits a read and a write under the allowed subtree but
    /// blocks a write elsewhere (the dispatch-site classifies arg 0 + read/write).
    #[cfg(feature = "sys")]
    #[tokio::test]
    async fn fs_carveout_blocks_write_outside_allow() {
        use crate::stdlib::caps::{CapSet, FsDeny, FsScope};
        let dir = std::env::temp_dir().join("ascript_caps_e2e");
        std::fs::create_dir_all(&dir).ok();
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let mut cs = CapSet::all_granted();
        cs.set_fs_scope(FsScope {
            deny: FsDeny::Write,
            allow: vec![dir.to_string_lossy().to_string()],
        });
        interp.set_caps(cs);
        let allowed = dir.join("ok.txt");
        let outside = std::env::temp_dir().join("ascript_caps_outside.txt");
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Write under the allowed subtree → passes the gate (returns a pair).
                let w_ok = interp
                    .call_stdlib(
                        "fs",
                        "write",
                        &[
                            Value::str(allowed.to_string_lossy().to_string()),
                            Value::str("hi"),
                        ],
                        Span::new(0, 0),
                    )
                    .await;
                assert!(w_ok.is_ok(), "write under allow should pass the gate");
                // Write OUTSIDE the allowed subtree → denied with a path-named panic.
                let w_deny = interp
                    .call_stdlib(
                        "fs",
                        "write",
                        &[
                            Value::str(outside.to_string_lossy().to_string()),
                            Value::str("nope"),
                        ],
                        Span::new(0, 0),
                    )
                    .await;
                match w_deny {
                    Err(Control::Panic(e)) => {
                        assert!(e.message.contains("fs") && e.message.contains("denied"), "{}", e.message)
                    }
                    other => panic!("expected fs write denial, got {other:?}"),
                }
                // A READ outside the subtree is still allowed (write-deny mode).
                let r_ok = interp
                    .call_stdlib(
                        "fs",
                        "exists",
                        &[Value::str(outside.to_string_lossy().to_string())],
                        Span::new(0, 0),
                    )
                    .await;
                assert!(r_ok.is_ok(), "read should be allowed in write-deny mode");
            })
            .await;
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_file(&outside).ok();
    }

    /// SP12 soft hook: with NO `telemetry.init` (and regardless of the feature),
    /// the SP11-facing `Interp::telemetry_*` hook is inert — `telemetry_active()`
    /// is false and `telemetry_span_start` returns `None`. This is the exact
    /// surface `std/ai` calls; it must compile and be inert in EVERY feature
    /// config (this test runs in both default and `--no-default-features`).
    #[test]
    fn telemetry_soft_hook_inert_without_init() {
        let interp = Interp::new();
        assert!(!interp.telemetry_active());
        assert!(interp
            .telemetry_span_start("op", vec![("k".into(), Value::float(1.0))])
            .is_none());
        // Setter/event/end on an arbitrary id are safe no-ops when inactive.
        interp.telemetry_span_set(0, "k", Value::nil());
        interp.telemetry_span_event(0, "e", vec![]);
        interp.telemetry_span_end(0, SpanStatus::Ok);
    }

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
    /// Uses `exec_program` so top-level `defer` statements drain at program end.
    async fn run(src: &str) -> String {
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async { interp.exec_program(&stmts, &env).await.expect("program panicked") })
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
            .run_until(crate::interp::ambient_root_scope(async {
                interp.exec(&stmts, &env).await.expect("program panicked")
            }))
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

    /// RESIL §5.5: a `log` record emitted inside a `withTrace` scope carries a
    /// `traceId` reserved field; outside any trace scope NO `traceId` is added.
    #[cfg(all(feature = "log", feature = "resilience"))]
    #[tokio::test]
    async fn log_carries_trace_id_inside_trace_scope() {
        let logs = run_logs(
            r#"
import * as log from "std/log"
import * as resilience from "std/resilience"
log.setFormat("json")
resilience.withTrace("t-1", () => {
    log.info("inside")
    return nil
})
log.info("outside")
"#,
        )
        .await;
        // The first record (inside the trace scope) carries the traceId.
        let inside = logs.lines().find(|l| l.contains("\"msg\":\"inside\"")).expect("inside record");
        assert!(inside.contains("\"traceId\":\"t-1\""), "inside should carry traceId: {inside}");
        // The second record (outside) has NO traceId field.
        let outside = logs.lines().find(|l| l.contains("\"msg\":\"outside\"")).expect("outside record");
        assert!(!outside.contains("traceId"), "outside must NOT carry traceId: {outside}");
    }

    /// RESIL Task 5.1 (§6.1): a breaker state transition emits a `log.debug`
    /// breadcrumb (behind the `log` feature) with the policy name + from/to.
    #[cfg(all(feature = "log", feature = "resilience"))]
    #[tokio::test]
    async fn breaker_transition_emits_log_breadcrumb() {
        let logs = run_logs(
            r#"
import * as log from "std/log"
import * as resilience from "std/resilience"
log.setLevel("debug")
log.setFormat("json")
let b = resilience.breaker({name: "svc", failureRate: 0.5, window: 2, minCalls: 2, cooldownMs: 999999, halfOpenMax: 1})
fn fail() { return [nil, {message: "x"}] }
b.call(fail)
b.call(fail)   // → opens (closed→open)
"#,
        )
        .await;
        let line = logs
            .lines()
            .find(|l| l.contains("breaker state transition"))
            .expect("a transition breadcrumb");
        assert!(line.contains("\"breaker\":\"svc\""), "policy name: {line}");
        assert!(line.contains("\"to\":\"open\""), "to=open: {line}");
        assert!(line.contains("\"from\":\"closed\""), "from=closed: {line}");
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
    /// Uses `exec_program` so top-level `defer` drains first (defers run before the panic
    /// escapes, matching the four-mode behaviour).
    async fn run_err(src: &str) -> AsError {
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        let r = local
            .run_until(async { interp.exec_program(&stmts, &env).await })
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
        fields.insert("pid".to_string(), Value::float(42.0));
        let h = interp.register_resource(
            crate::value::NativeKind::ChildProcess,
            fields,
            ResourceState::Closed,
        );
        assert_eq!(type_name(&h), "childProcess");
        assert_eq!(
            interp.read_member(&h, "pid", Span::new(0, 0)).unwrap(),
            Value::float(42.0)
        );
        let m = interp.read_member(&h, "wait", Span::new(0, 0)).unwrap();
        assert!(matches!(m.kind(), ValueKind::NativeMethod(_)));
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
        // NUM §3.3 (BREAKING): 0 and "" are now FALSY; a non-zero number is truthy.
        assert_eq!(run("print(0 ? \"t\" : \"f\")").await, "f\n");
        assert_eq!(run("print(1 ? \"t\" : \"f\")").await, "t\n");
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
        // NUM §4: `1e3` is a `float` (exponent ⇒ float) and prints `1000.0`; the
        // hex/binary/underscore int literals print with no decimal.
        assert_eq!(
            run("print(0xFF)\nprint(0b1010)\nprint(1e3)\nprint(1_000)\nprint(0xFF_FF)").await,
            "255\n10\n1000.0\n1000\n65535\n"
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
        // NUM §4: `type_name(5)` is now `"int"`.
        assert_eq!(
            err.message,
            "type contract violated: expected future<number>, got int (5)"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn int_float_conversion_builtins() {
        // NUM §4: `int(x)` / `float(x)` conversion builtins.
        // float → int truncates toward zero.
        assert_eq!(run("print(int(5.7))").await, "5\n");
        assert_eq!(run("print(int(-5.7))").await, "-5\n");
        assert_eq!(run("print(int(5.0))").await, "5\n");
        // int → int identity; type stays int.
        assert_eq!(run("print(int(5))").await, "5\n");
        assert_eq!(run("print(type(int(5.7)))").await, "int\n");
        // float(int) → exact f64, prints with a decimal.
        assert_eq!(run("print(float(3))").await, "3.0\n");
        assert_eq!(run("print(type(float(3)))").await, "float\n");
        // float → float identity.
        assert_eq!(run("print(float(2.5))").await, "2.5\n");
        // string parse returns a Tier-1 [value, err] pair.
        assert_eq!(run("print(int(\"42\"))").await, "[42, nil]\n");
        assert_eq!(run("print(type(int(\"42\")[0]))").await, "int\n");
        assert_eq!(run("print(float(\"3.5\"))").await, "[3.5, nil]\n");
        assert_eq!(run("print(type(float(\"3.5\")[0]))").await, "float\n");
        // bad string parse → [nil, err].
        let out = run("let r = int(\"x\")\nprint(r[0])\nprint(r[1] != nil)").await;
        assert_eq!(out, "nil\ntrue\n");
        let out = run("let r = float(\"nope\")\nprint(r[0])\nprint(r[1] != nil)").await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn int_conversion_out_of_range_boundary() {
        // Regression: `i64::MAX as f64` rounds UP to 2^63, so a `<=` bound would admit
        // the out-of-range value 2^63 and `as i64` would silently saturate to i64::MAX.
        // The strict bound must REJECT 2^63 with a clean out-of-range error...
        let out = run("print(recover(() => int(9223372036854775808.0))[1] != nil)").await;
        assert_eq!(out, "true\n", "int(2^63) must error, not silently saturate");
        // ...while still ADMITTING the largest representable in-range float (2^63 − 2048).
        assert_eq!(
            run("print(int(9223372036854773760.0))").await,
            "9223372036854773760\n"
        );
        // i64::MIN is exactly representable and in range.
        assert_eq!(
            run("print(int(-9223372036854775808.0))").await,
            "-9223372036854775808\n"
        );
        // non-finite → clean error, never a 0/garbage cast.
        let inf = run("print(recover(() => int(1.0 / 0.0))[1] != nil)").await;
        assert_eq!(inf, "true\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn int_float_names_still_work_as_types_and_instanceof() {
        // The new `int`/`float` builtins must not break the reserved type names.
        // Call position:
        assert_eq!(run("print(int(7.9))").await, "7\n");
        // instanceof:
        assert_eq!(run("print(5 instanceof int)").await, "true\n");
        assert_eq!(run("print(5 instanceof float)").await, "false\n");
        assert_eq!(run("print(5.0 instanceof float)").await, "true\n");
        // annotation (runtime contract):
        assert_eq!(run("let x: int = 5\nprint(x)").await, "5\n");
        assert_eq!(run("let y: float = 2.5\nprint(y)").await, "2.5\n");
    }

    #[test]
    fn check_type_int_float_number_contracts() {
        use crate::ast::Type;
        // NUM §5: `int` accepts only Int, `float` only Float, `number` both. This is
        // the runtime contract enforced on class fields / params / returns. Regression
        // for the bug where `int`/`float` parsed as Type::Named and `class C { x: int }`
        // panicked "expected int, got int".
        assert!(check_type(&Value::int(5), &Type::Int));
        assert!(!check_type(&Value::float(5.0), &Type::Int));
        assert!(check_type(&Value::float(2.5), &Type::Float));
        assert!(!check_type(&Value::int(5), &Type::Float));
        assert!(check_type(&Value::int(5), &Type::Number));
        assert!(check_type(&Value::float(2.5), &Type::Number));
        assert!(!check_type(&Value::str("x"), &Type::Int));
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
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
            debug_name: None,
            name_span: None,
        });
        let closure = Value::closure(crate::vm::value_ext::Closure::new(proto));
        assert!(
            check_type(&closure, &Type::Fn),
            "a VM Closure must satisfy a `: fn` contract"
        );
        // The tree-walker callables still satisfy `: fn`.
        assert!(check_type(&Value::builtin("len"), &Type::Fn));
        // A non-callable still fails the `: fn` contract (behavior preserved).
        assert!(!check_type(&Value::float(7.0), &Type::Fn));
        assert!(!check_type(&Value::str("x"), &Type::Fn));
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
        // NUM §4: an int literal is `"int"`; a float literal is `"float"`.
        assert_eq!(run("print(type(1))").await, "int\n");
        assert_eq!(run("print(type(1.5))").await, "float\n");
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
            "[0.0, 0.3, 0.6, 0.8999999999999999]\n"
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
    async fn class_without_init_rejects_excess_args() {
        // A zero-field class with no init auto-derives a zero-arg constructor
        // (SP2 §5 records): `Empty(1)` is a too-many-args arity error, with the
        // SAME wording as a 0-arg function call.
        let src = "class Empty {}\nEmpty(1)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert_eq!(err.message, "Empty expected 0 argument(s), got 1");
    }

    #[tokio::test]
    async fn class_without_init_auto_derives_positional_constructor() {
        // SP2 §5 records: a field-only class is constructed positionally, in
        // field-declaration order, with field contracts enforced.
        let src = "class Point { x: number\n y: number }\nlet p = Point(1, 2)\nprint(p.x)\nprint(p.y)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output(), "1\n2\n");
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
        assert_eq!(eval_to_value("let x: number? = nil\nx").await, Value::nil());
        assert_eq!(
            eval_to_value("let x: number? = 7\nx").await,
            Value::float(7.0)
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

    // ADT §3.2: named-field variant construction. `ENUM_SHAPE` declares a Circle
    // (single named field), Rect (multi named field), Pair (positional), Point (unit).
    const ENUM_SHAPE: &str =
        "enum Shape { Circle(radius: float), Rect(w: float, h: float), Pair(int, int), Point }\n";

    async fn run_shape(body: &str) -> Result<String, AsError> {
        let src = format!("{ENUM_SHAPE}{body}");
        let stmts = parse(&lex(&src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        interp
            .exec(&stmts, &env)
            .await
            .map(|_| interp.output())
            .map_err(panic_of)
    }

    #[tokio::test]
    async fn variant_named_construction_is_order_independent() {
        // Named args construct a multi-field variant regardless of call order, and
        // two equal-payload constructions are structurally equal.
        let out = run_shape(
            "let a = Shape.Rect(w: 3.0, h: 4.0)\n\
             let b = Shape.Rect(h: 4.0, w: 3.0)\n\
             print(a.value)\nprint(b.value)\nprint(a == b)\nprint(a.w)\nprint(a.h)",
        )
        .await
        .unwrap();
        assert_eq!(out, "{w: 3.0, h: 4.0}\n{w: 3.0, h: 4.0}\ntrue\n3.0\n4.0\n");
    }

    #[tokio::test]
    async fn variant_single_named_field_accepts_positional_and_named() {
        // The single-field convenience: `Circle(2.0)` and `Circle(radius: 2.0)` both
        // construct, and are equal.
        let out = run_shape(
            "let p = Shape.Circle(2.0)\nlet n = Shape.Circle(radius: 2.0)\n\
             print(p.radius)\nprint(n.radius)\nprint(p == n)",
        )
        .await
        .unwrap();
        assert_eq!(out, "2.0\n2.0\ntrue\n");
    }

    #[tokio::test]
    async fn variant_multi_named_positional_is_spec_error() {
        // A positional call of a multi-field named variant is the spec'd error.
        let err = run_shape("print(Shape.Rect(3.0, 4.0))").await.unwrap_err();
        assert_eq!(err.message, "Shape.Rect requires named fields (w:, h:)");
    }

    #[tokio::test]
    async fn variant_named_on_positional_variant_errors() {
        let err = run_shape("print(Shape.Pair(a: 1, b: 2))")
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "Shape.Pair is a positional variant and takes positional arguments, not named fields"
        );
    }

    #[tokio::test]
    async fn variant_named_unknown_field_errors() {
        let err = run_shape("print(Shape.Rect(w: 3.0, z: 4.0))")
            .await
            .unwrap_err();
        assert_eq!(err.message, "Shape.Rect has no field 'z'");
    }

    #[tokio::test]
    async fn variant_named_missing_field_errors() {
        let err = run_shape("print(Shape.Rect(w: 3.0))").await.unwrap_err();
        assert_eq!(err.message, "Shape.Rect is missing field 'h'");
    }

    #[tokio::test]
    async fn variant_named_duplicate_field_errors() {
        let err = run_shape("print(Shape.Rect(w: 3.0, w: 4.0))")
            .await
            .unwrap_err();
        assert_eq!(err.message, "Shape.Rect: duplicate field 'w'");
    }

    #[tokio::test]
    async fn variant_named_field_type_is_validated() {
        let err = run_shape("print(Shape.Rect(w: \"x\", h: 4.0))")
            .await
            .unwrap_err();
        assert_eq!(err.message, "Shape.Rect.w: expected float, got string");
    }

    #[tokio::test]
    async fn variant_named_first_class_constructor() {
        // A first-class constructor (`let mk = Shape.Rect`) accepts named args.
        let out = run_shape(
            "let mk = Shape.Rect\nlet r = mk(h: 2.0, w: 1.0)\nprint(r.value)",
        )
        .await
        .unwrap();
        assert_eq!(out, "{w: 1.0, h: 2.0}\n");
    }

    #[tokio::test]
    async fn named_args_on_non_variant_callee_errors() {
        // Named args are only valid for variant construction.
        let src = "fn f(x) { return x }\nprint(f(x: 1))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(
            err.message
                .contains("named arguments are only valid for enum-variant construction"),
            "got: {}",
            err.message
        );
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
        assert_eq!(eval_to_value("[42, nil]!").await, Value::float(42.0));
        assert_eq!(eval_to_value("Ok(7)!").await, Value::float(7.0));
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
        // NUM §4: int-literal arithmetic yields `Int`.
        let r = eval_to_value("1 + 2 * 3").await;
        match r.kind() {
            ValueKind::Int(n) => assert_eq!(n, 7),
            _ => panic!("expected int, got {:?}", r),
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
        assert_eq!(eval_to_value("1 < 2").await, Value::bool_(true));
        assert_eq!(eval_to_value("2 == 2").await, Value::bool_(true));
        assert_eq!(eval_to_value("1 != 2").await, Value::bool_(true));
        assert_eq!(eval_to_value("\"a\" == \"a\"").await, Value::bool_(true));
    }

    #[tokio::test]
    async fn string_concatenation() {
        // `Str + Str` concatenates.
        assert_eq!(
            eval_to_value("\"a\" + \"b\"").await,
            Value::str("ab")
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
        assert_eq!(eval_to_value("2 ** 10").await, Value::float(1024.0));
    }

    #[tokio::test]
    async fn short_circuit_and_coalesce() {
        assert_eq!(eval_to_value("false && nope").await, Value::bool_(false));
        assert_eq!(eval_to_value("true || nope").await, Value::bool_(true));
        // NUM §4: int literals yield `Int`.
        assert_eq!(eval_to_value("nil ?? 5").await, Value::int(5));
        assert_eq!(eval_to_value("3 ?? nope").await, Value::int(3));
        // NUM §3.3: `0` is falsy, so `!0` is `true`.
        assert_eq!(eval_to_value("!0").await, Value::bool_(true));
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
        assert_eq!(eval_to_value("print").await, Value::builtin("print"));
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
        // NUM §4: `math.abs(-5)` is subtype-preserving — an `int` in, an `int`
        // out (prints `5`, not `5.0`); `math.pow` stays `float`.
        let out = run("import * as math from \"std/math\"\nprint(math.abs(-5))\nprint(math.pow(2, 8))\nprint(math.pi > 3.14)").await;
        assert_eq!(out, "5\n256.0\ntrue\n");
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
        // NUM §3.3: `convert.toBool(0)` is now `false` (0 is falsy).
        assert_eq!(
            run(src).await,
            "42.0\nnil\nnil\ncannot parse 'nope' as a number\n255\n123\nfalse\n"
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
        // NUM §4: `math.abs` is subtype-preserving — int elements stay int.
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
        assert_eq!(out, "12.0\n7.0\n");
    }

    #[tokio::test]
    async fn unknown_std_module_errors() {
        let err = run_err("import { x } from \"std/nope\"").await;
        assert!(err.message.contains("unknown standard library module"));
    }

    #[tokio::test]
    async fn std_module_import_is_cached() {
        // NUM §4: `floor` returns an `int` (`3`), and `abs(int)` stays `int` (`2`).
        let out = run("import * as m1 from \"std/math\"\nimport { abs } from \"std/math\"\nprint(m1.floor(3.7))\nprint(abs(-2))").await;
        assert_eq!(out, "3\n2\n");
    }

    #[tokio::test]
    async fn std_time_now_and_durations() {
        let out = run("import * as time from \"std/time\"\nprint(time.seconds(2))\nprint(time.now() > 1700000000000)").await;
        assert_eq!(out, "2000.0\ntrue\n");
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
        assert_eq!(run(src).await, "2021.0\n6.0\n2021/06/15\n25.0\n864000000.0\n");
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
            "1,234,567\n1.234.567\nİSTANBUL\nISTANBUL\n-1.0\n"
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
    async fn decimal_zero_is_falsy() {
        // NUM §3.3 (BREAKING): a Decimal equal to zero is FALSY (the falsy set is
        // nil, false, Int(0), a zero/NaN Float, a zero Decimal, and "").
        // Use `if (z)` since AScript requires parens around the condition.
        let zero = run(r#"
import * as decimal from "std/decimal"
let z = decimal.from("0")
if (z) { print("truthy") } else { print("falsy") }
"#)
        .await;
        assert_eq!(zero.trim(), "falsy");
        // A non-zero Decimal is truthy.
        let nonzero = run(r#"
import * as decimal from "std/decimal"
let z = decimal.from("0.5")
if (z) { print("truthy") } else { print("falsy") }
"#)
        .await;
        assert_eq!(nonzero.trim(), "truthy");
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
        // NUM §4: `abs(int)` is int (`5`); `max` is unchanged float (`3.0`).
        assert_eq!(run(src).await, "5\n3.0\n");
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

    // ---- SP6 package resolver plumbing (D1) ---------------------------------

    #[test]
    fn split_package_key_unscoped() {
        assert_eq!(split_package_key("http"), ("http".into(), "".into()));
        assert_eq!(
            split_package_key("http/router"),
            ("http".into(), "router".into())
        );
        assert_eq!(
            split_package_key("http/a/b"),
            ("http".into(), "a/b".into())
        );
    }

    #[test]
    fn split_package_key_scoped() {
        assert_eq!(
            split_package_key("@acme/schema"),
            ("@acme/schema".into(), "".into())
        );
        assert_eq!(
            split_package_key("@acme/schema/sub"),
            ("@acme/schema".into(), "sub".into())
        );
        assert_eq!(
            split_package_key("@acme/schema/a/b"),
            ("@acme/schema".into(), "a/b".into())
        );
    }

    #[test]
    fn classify_std_and_relative_unchanged() {
        let interp = Interp::new();
        assert_eq!(interp.classify_specifier("std/math"), SpecifierKind::Std);
        // Relative paths resolve against module_dir (default ".") + default ".as".
        match interp.classify_specifier("./util") {
            SpecifierKind::Relative(p) => assert!(p.to_string_lossy().ends_with("util.as")),
            other => panic!("expected Relative, got {other:?}"),
        }
        match interp.classify_specifier("../sib/mod") {
            SpecifierKind::Relative(p) => assert!(p.to_string_lossy().ends_with("mod.as")),
            other => panic!("expected Relative, got {other:?}"),
        }
    }

    #[test]
    fn classify_bare_unknown_without_resolver() {
        let interp = Interp::new();
        // No resolver installed → every bare specifier is UnknownPackage.
        assert_eq!(
            interp.classify_specifier("http"),
            SpecifierKind::UnknownPackage("http".into())
        );
        assert_eq!(
            interp.classify_specifier("@scope/x/sub"),
            SpecifierKind::UnknownPackage("@scope/x".into())
        );
    }

    #[test]
    fn classify_bare_package_entry_and_subpath() {
        let interp = Interp::new();
        let mut map = PackageMap::new();
        map.insert(
            "lib".into(),
            ResolvedPkg {
                root: PathBuf::from("/store/abc"),
                entry: PathBuf::from("/store/abc/src/main.as"),
            },
        );
        interp.set_package_resolver(map);

        // No subpath → the entry module.
        match interp.classify_specifier("lib") {
            SpecifierKind::Package { key, target } => {
                assert_eq!(key, "lib");
                assert_eq!(target, PathBuf::from("/store/abc/src/main.as"));
            }
            other => panic!("expected Package, got {other:?}"),
        }
        // Subpath → root.join(subpath) + default .as.
        match interp.classify_specifier("lib/util") {
            SpecifierKind::Package { key, target } => {
                assert_eq!(key, "lib");
                assert_eq!(target, PathBuf::from("/store/abc/util.as"));
            }
            other => panic!("expected Package, got {other:?}"),
        }
        // A miss on an unknown key stays UnknownPackage even with a resolver.
        assert_eq!(
            interp.classify_specifier("other"),
            SpecifierKind::UnknownPackage("other".into())
        );
    }

    // ---- NUM §3.2/§3.3: int/float arithmetic, comparison, panics --------------

    /// Evaluate a single expression, returning the Value or the propagated
    /// `Control` (so a Tier-2 panic can be asserted on). Uses the tree-walker
    /// directly — `apply_binop`/`apply_unop` are the shared source of truth, so
    /// the VM path is byte-identical by construction.
    async fn try_eval(src: &str) -> Result<Value, Control> {
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let interp = Interp::new();
        let env = global_env();
        let (last, rest) = stmts.split_last().expect("at least one statement");
        interp.exec(rest, &env).await?;
        match last {
            Stmt::Expr(e) => interp.eval_expr(e, &env).await,
            _ => panic!("last statement must be an expression"),
        }
    }

    async fn eval_num(src: &str) -> Value {
        try_eval(src).await.expect("expected a value, got a panic")
    }

    async fn eval_panic_msg(src: &str) -> String {
        match try_eval(src).await {
            Ok(v) => panic!("expected a panic, got value {v:?}"),
            Err(Control::Panic(e)) => e.message,
            Err(other) => panic!("expected a Panic, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn int_literals_eval_to_int() {
        assert_eq!(eval_num("5").await, Value::int(5));
        assert_eq!(eval_num("0xFF").await, Value::int(255));
        assert_eq!(eval_num("0b1010").await, Value::int(10));
        assert_eq!(eval_num("0o17").await, Value::int(15));
        assert_eq!(eval_num("1_000").await, Value::int(1000));
    }

    #[tokio::test]
    async fn float_literals_eval_to_float() {
        assert_eq!(eval_num("5.0").await, Value::float(5.0));
        assert_eq!(eval_num("1.5").await, Value::float(1.5));
        assert_eq!(eval_num("1e3").await, Value::float(1000.0));
    }

    #[tokio::test]
    async fn int_add_sub_mul_are_int() {
        assert_eq!(eval_num("2 + 3").await, Value::int(5));
        assert_eq!(eval_num("7 - 10").await, Value::int(-3));
        assert_eq!(eval_num("6 * 7").await, Value::int(42));
    }

    #[tokio::test]
    async fn int_div_truncates_toward_zero() {
        assert_eq!(eval_num("7 / 2").await, Value::int(3));
        assert_eq!(eval_num("-7 / 2").await, Value::int(-3));
        assert_eq!(eval_num("1 / 2").await, Value::int(0));
    }

    #[tokio::test]
    async fn int_mod_sign_follows_dividend() {
        assert_eq!(eval_num("7 % 2").await, Value::int(1));
        assert_eq!(eval_num("-7 % 2").await, Value::int(-1));
    }

    #[tokio::test]
    async fn int_pow_int_and_negative_exponent() {
        assert_eq!(eval_num("2 ** 10").await, Value::int(1024));
        assert_eq!(eval_num("0 ** 0").await, Value::int(1));
        assert_eq!(eval_num("(0 - 2) ** 3").await, Value::int(-8));
        assert_eq!(eval_num("2 ** 4").await, Value::int(16));
        // Negative exponent → float.
        assert_eq!(eval_num("2 ** (0 - 1)").await, Value::float(0.5));
    }

    #[tokio::test]
    async fn int_division_and_remainder_by_zero_panic() {
        assert_eq!(eval_panic_msg("1 / 0").await, "integer division by zero");
        assert_eq!(eval_panic_msg("1 % 0").await, "integer remainder by zero");
    }

    #[tokio::test]
    async fn int_overflow_panics() {
        // 2**62 + 2**62 overflows i64 on '+'.
        assert_eq!(
            eval_panic_msg("4611686018427387904 + 4611686018427387904").await,
            "integer overflow in '+'"
        );
        // i64::MAX * 2 overflows on '*'.
        assert_eq!(
            eval_panic_msg("9223372036854775807 * 2").await,
            "integer overflow in '*'"
        );
        // 2**63 overflows on '**'.
        assert_eq!(eval_panic_msg("2 ** 63").await, "integer overflow in '**'");
    }

    #[tokio::test]
    async fn int_min_div_neg_one_overflows() {
        // i64::MIN is -9223372036854775808; -9223372036854775808 / -1 overflows.
        // Build i64::MIN as -(i64::MAX) - 1 to avoid an out-of-range literal.
        let src = "(0 - 9223372036854775807 - 1) / (0 - 1)";
        assert_eq!(eval_panic_msg(src).await, "integer overflow in '/'");
    }

    #[tokio::test]
    async fn unary_neg_of_int_min_overflows() {
        let src = "0 - 9223372036854775807 - 1"; // i64::MIN
        assert_eq!(eval_num(src).await, Value::int(i64::MIN));
        assert_eq!(
            eval_panic_msg("-(0 - 9223372036854775807 - 1)").await,
            "integer overflow in '-'"
        );
    }

    #[tokio::test]
    async fn mixed_int_float_promotes_to_float() {
        assert_eq!(eval_num("1 + 1.0").await, Value::float(2.0));
        assert_eq!(eval_num("1.0 + 1").await, Value::float(2.0));
        assert_eq!(eval_num("7.0 / 2").await, Value::float(3.5));
        assert_eq!(eval_num("2 * 1.5").await, Value::float(3.0));
    }

    #[tokio::test]
    async fn exact_cross_subtype_equality() {
        assert_eq!(eval_num("1 == 1.0").await, Value::bool_(true));
        assert_eq!(eval_num("1 != 1.0").await, Value::bool_(false));
        // Near 2^53, an int not exactly representable as f64 is NOT equal to the
        // rounded float: 9007199254740993 (2^53+1) vs 9007199254740992.0 (2^53).
        assert_eq!(
            eval_num("9007199254740993 == 9007199254740992.0").await,
            Value::bool_(false)
        );
    }

    #[tokio::test]
    async fn exact_cross_subtype_ordering() {
        assert_eq!(eval_num("2 < 2.5").await, Value::bool_(true));
        assert_eq!(eval_num("2.5 > 2").await, Value::bool_(true));
        assert_eq!(eval_num("3 <= 3.0").await, Value::bool_(true));
        assert_eq!(eval_num("3 >= 3.0").await, Value::bool_(true));
        // Exact boundary: 2^53+1 > 2^53.0 (the int is strictly greater).
        assert_eq!(
            eval_num("9007199254740993 > 9007199254740992.0").await,
            Value::bool_(true)
        );
        // NaN comparisons are all false.
        assert_eq!(eval_num("1 < (0.0 / 0.0)").await, Value::bool_(false));
        assert_eq!(eval_num("1 > (0.0 / 0.0)").await, Value::bool_(false));
    }

    #[tokio::test]
    async fn int_int_comparison_is_int_typed() {
        assert_eq!(eval_num("1 < 2").await, Value::bool_(true));
        assert_eq!(eval_num("2 == 2").await, Value::bool_(true));
        assert_eq!(eval_num("3 >= 4").await, Value::bool_(false));
    }

    // ---- NUM §3.2 bitwise / shift / wrapping operators --------------------

    #[tokio::test]
    async fn bitwise_and_or_xor() {
        assert_eq!(eval_num("0xFF & 0b1010").await, Value::int(10));
        assert_eq!(eval_num("12 & 10").await, Value::int(8));
        assert_eq!(eval_num("12 | 10").await, Value::int(14));
        assert_eq!(eval_num("12 ^ 10").await, Value::int(6));
        // `|` in value position is bitwise-OR (not an or-pattern).
        assert_eq!(eval_num("1 | 2").await, Value::int(3));
    }

    #[tokio::test]
    async fn bitwise_not() {
        assert_eq!(eval_num("~0").await, Value::int(-1));
        assert_eq!(eval_num("~5").await, Value::int(-6));
    }

    #[tokio::test]
    async fn shifts_and_arithmetic_sign_extension() {
        assert_eq!(eval_num("1 << 3").await, Value::int(8));
        assert_eq!(eval_num("(1 << 16) | 256").await, Value::int(65792));
        assert_eq!(eval_num("1 >> 0").await, Value::int(1));
        // `>>` is arithmetic (sign-extending): -8 >> 1 == -4.
        assert_eq!(eval_num("(0 - 8) >> 1").await, Value::int(-4));
        // -1 << 1 == -2 (bit-loss into the sign bit does NOT trap).
        assert_eq!(eval_num("(0 - 1) << 1").await, Value::int(-2));
    }

    #[tokio::test]
    async fn shift_boundaries() {
        // 1 << 63 == i64::MIN (top bit set), a DEFINED result — not an overflow.
        assert_eq!(eval_num("1 << 63").await, Value::int(i64::MIN));
        // 1 << 64 → amount >= 64 → panic.
        assert_eq!(eval_panic_msg("1 << 64").await, "shift amount out of range: 64");
        // A negative shift amount panics.
        assert_eq!(
            eval_panic_msg("1 << (0 - 1)").await,
            "shift amount out of range: -1"
        );
        assert_eq!(eval_panic_msg("1 >> 64").await, "shift amount out of range: 64");
    }

    #[tokio::test]
    async fn wrapping_never_panics() {
        assert_eq!(eval_num("5 +% 3").await, Value::int(8));
        assert_eq!(eval_num("5 -% 8").await, Value::int(-3));
        assert_eq!(eval_num("6 *% 7").await, Value::int(42));
        // i64::MAX +% 1 wraps to i64::MIN (vs the checked `+` which panics).
        assert_eq!(
            eval_num("9223372036854775807 +% 1").await,
            Value::int(i64::MIN)
        );
        // i64::MAX * 2 wraps with `*%` (the checked `*` overflows).
        assert_eq!(
            eval_num("9223372036854775807 *% 2").await,
            Value::int(-2)
        );
    }

    #[tokio::test]
    async fn checked_vs_wrapping_overflow() {
        // The checked `+`/`*` panic where the wrapping `+%`/`*%` do not.
        assert_eq!(
            eval_panic_msg("9223372036854775807 + 1").await,
            "integer overflow in '+'"
        );
        assert_eq!(
            eval_num("9223372036854775807 +% 1").await,
            Value::int(i64::MIN)
        );
    }

    #[tokio::test]
    async fn bitwise_on_float_is_type_error() {
        assert_eq!(
            eval_panic_msg("1 & 2.0").await,
            "bitwise op requires int operands, got float"
        );
        assert_eq!(
            eval_panic_msg("1.0 | 2").await,
            "bitwise op requires int operands, got float"
        );
        assert_eq!(
            eval_panic_msg("1 << 2.0").await,
            "bitwise op requires int operands, got float"
        );
        assert_eq!(
            eval_panic_msg("~1.0").await,
            "bitwise op requires int operands, got float"
        );
        // Wrapping is int-only too.
        assert_eq!(
            eval_panic_msg("1 +% 2.0").await,
            "wrapping op requires int operands, got float"
        );
    }

    #[tokio::test]
    async fn go_precedence_bitwise_vs_comparison_and_arithmetic() {
        // `a & b == c` parses as `(a & b) == c` (Go's binding). 6 & 2 == 2 → 2 == 2 → true.
        assert_eq!(eval_num("6 & 2 == 2").await, Value::bool_(true));
        // `a | b == c` parses as `(a | b) == c`. 1 | 2 == 3 → 3 == 3 → true.
        assert_eq!(eval_num("1 | 2 == 3").await, Value::bool_(true));
        // `+ -` bind TIGHTER than `|`: `1 | 2 + 1` is `1 | (2+1)` = 1|3 = 3.
        assert_eq!(eval_num("1 | 2 + 1").await, Value::int(3));
        // `<<`/`&` bind at the multiplicative tier (tighter than `+ -`):
        // `1 + 1 << 2` is `1 + (1<<2)` = 1 + 4 = 5.
        assert_eq!(eval_num("1 + 1 << 2").await, Value::int(5));
    }

    // ---- IFACE Task 2: conforms predicate + lazy flatten + cycle guard + cache ----
    use crate::value::{InterfaceDef, MethodReq};
    use indexmap::IndexMap;

    /// A param with the given name; `defaulted` adds a default expr (so it's optional),
    /// `rest` marks it variadic.
    fn iface_param(name: &str, defaulted: bool, rest: bool) -> crate::ast::Param {
        crate::ast::Param {
            name: name.to_string(),
            ty: None,
            name_span: Span::new(0, 0),
            rest,
            default: if defaulted {
                Some(crate::ast::Expr {
                    kind: crate::ast::ExprKind::Nil,
                    span: Span::new(0, 0),
                })
            } else {
                None
            },
        }
    }

    /// A no-body instance method with the given params.
    fn iface_method(params: Vec<crate::ast::Param>) -> std::rc::Rc<crate::value::Method> {
        std::rc::Rc::new(crate::value::Method {
            params,
            ret: None,
            body: Vec::new(),
            is_async: false,
            is_generator: false,
            is_worker: false,
        })
    }

    /// Build a class with the given methods and optional superclass.
    fn iface_class(
        env: &Environment,
        name: &str,
        methods: Vec<(&str, std::rc::Rc<crate::value::Method>)>,
        superclass: Option<std::rc::Rc<crate::value::Class>>,
    ) -> std::rc::Rc<crate::value::Class> {
        let mut method_map = IndexMap::new();
        for (n, m) in methods {
            method_map.insert(n.to_string(), m);
        }
        let class = std::rc::Rc::new(crate::value::Class {
            name: name.to_string(),
            superclass,
            fields: IndexMap::new(),
            methods: method_map,
            static_methods: IndexMap::new(),
            def_env: env.clone(),
            is_worker: false,
        });
        // Bind the class as a module-global so it stays ALIVE for the whole test (in a
        // real run classes are load-time-immortal — §5.3 — so the verdict cache's
        // pointer keys never alias a freed-then-reallocated class; we mirror that here
        // to keep the test deterministic).
        env.define(name, Value::class(class.clone()), false).ok();
        class
    }

    /// A bare instance of a class (no fields).
    fn iface_instance(class: std::rc::Rc<crate::value::Class>) -> Value {
        Value::instance(gcmodule::Cc::new(std::cell::RefCell::new(
            crate::value::Instance::from_dict(class, IndexMap::new()),
        )))
    }

    /// An interface binding `name` with `own` requirements (name → arity, no rest) and
    /// `extends` names, defined in `env`, and ALSO bound in `env` so extends resolve.
    fn iface_def(
        env: &Environment,
        name: &str,
        own: Vec<(&str, usize)>,
        extends: Vec<&str>,
    ) -> std::rc::Rc<InterfaceDef> {
        let mut own_methods = IndexMap::new();
        for (n, arity) in own {
            own_methods.insert(
                n.to_string(),
                MethodReq {
                    arity,
                    has_rest: false,
                },
            );
        }
        let def = std::rc::Rc::new(InterfaceDef {
            name: name.to_string(),
            own_methods,
            extends: extends.into_iter().map(|s| s.to_string()).collect(),
            def_env: env.clone(),
            flat: std::cell::RefCell::new(None),
        });
        env.define(name, Value::interface(def.clone()), false)
            .unwrap();
        def
    }

    #[test]
    fn conforms_basic_presence_and_arity() {
        let interp = Interp::new();
        let env = global_env().child();
        // interface Reader { fn read(b) -> int }   (arity 1)
        let reader = iface_def(&env, "Reader", vec![("read", 1)], vec![]);
        // class File { fn read(b) {} }  → conforms
        let file = iface_class(&env, "File", vec![("read", iface_method(vec![iface_param("b", false, false)]))], None);
        assert!(interp.conforms(&iface_instance(file), &reader).unwrap());
        // class NoRead { fn write(b) {} } → does NOT conform (missing read)
        let noread = iface_class(&env, "NoRead", vec![("write", iface_method(vec![iface_param("b", false, false)]))], None);
        assert!(!interp.conforms(&iface_instance(noread), &reader).unwrap());
        // Non-instance LHS → false (never an error).
        assert!(!interp.conforms(&Value::int(5), &reader).unwrap());
        assert!(!interp.conforms(&Value::nil(), &reader).unwrap());
        assert!(!interp.conforms(&Value::object(IndexMap::new()), &reader).unwrap());
    }

    #[test]
    fn iface_verdict_cache_generation_guard_drops_stale_pointer_entries() {
        // Regression (IFACE review): the verdict cache keys on raw `Rc::as_ptr` values.
        // A class/interface declared inside a fn/loop is NOT load-time-immortal — it can
        // be freed and the allocator can hand the SAME address to a later, DIFFERENT
        // descriptor, which would otherwise read a stale verdict. The generation guard
        // (bumped on every runtime class/interface DEFINITION) must make a pre-bump entry
        // a cache MISS so it is recomputed, never trusted. Deterministic (no reliance on
        // the allocator actually reusing an address): we assert the gen-stamp mechanics.
        let interp = Interp::new();
        let env = global_env().child();
        let reader = iface_def(&env, "Reader", vec![("read", 1)], vec![]);
        let file = iface_class(
            &env,
            "File",
            vec![("read", iface_method(vec![iface_param("b", false, false)]))],
            None,
        );
        let inst = iface_instance(file.clone());
        // First check caches the verdict stamped with the current generation.
        assert!(interp.conforms(&inst, &reader).unwrap());
        let key = (
            std::rc::Rc::as_ptr(&file) as usize,
            std::rc::Rc::as_ptr(&reader) as usize,
        );
        let gen0 = interp.iface_cache_gen.get();
        assert_eq!(
            interp.iface_verdict_cache.borrow().get(&key),
            Some(&(gen0, true)),
            "verdict must be cached stamped with the current generation"
        );
        // A runtime define (what a nested `class`/`interface` triggers) bumps the gen,
        // making the entry recorded under gen0 stale.
        interp.bump_iface_cache_gen();
        let gen1 = interp.iface_cache_gen.get();
        assert_ne!(gen0, gen1, "a definition must bump the generation");
        // Reading now must MISS on the stale-gen entry, recompute, and re-stamp gen1.
        // The re-stamp is the observable proof the stale entry was NOT trusted: a broken
        // guard (gen-blind hit) would leave the entry at gen0.
        assert!(interp.conforms(&inst, &reader).unwrap());
        assert_eq!(
            interp.iface_verdict_cache.borrow().get(&key),
            Some(&(gen1, true)),
            "a stale-generation entry must be recomputed and re-stamped, not trusted"
        );
    }

    #[test]
    fn conforms_inherited_method_satisfies() {
        let interp = Interp::new();
        let env = global_env().child();
        let reader = iface_def(&env, "Reader", vec![("read", 1)], vec![]);
        let base = iface_class(&env, "Base", vec![("read", iface_method(vec![iface_param("b", false, false)]))], None);
        let sub = iface_class(&env, "Sub", vec![], Some(base));
        assert!(interp.conforms(&iface_instance(sub), &reader).unwrap());
    }

    #[test]
    fn conforms_arity_table() {
        let interp = Interp::new();
        let env = global_env().child();
        // requirement read(b) arity 1 ; read(b,o) arity 2
        let req1 = iface_def(&env, "R1", vec![("read", 1)], vec![]);
        let req2 = iface_def(&env, "R2", vec![("read", 2)], vec![]);
        // Build one instance per class and keep them ALIVE for the whole test. In a real
        // run a runtime define bumps `iface_cache_gen` so a reused pointer can't read a
        // stale verdict (see `iface_verdict_cache_generation_guard_*`); but these helpers
        // construct descriptors directly WITHOUT going through `exec`/`Op` (no gen bump),
        // so here we must not free a class between checks or its address could be reused
        // within a single generation and collide.
        // fn read(b, opts=nil) → min 1 max 2 : satisfies BOTH arity 1 and arity 2
        let i_default = iface_instance(iface_class(&env, "D", vec![("read", iface_method(vec![iface_param("b", false, false), iface_param("opts", true, false)]))], None));
        assert!(interp.conforms(&i_default, &req1).unwrap());
        assert!(interp.conforms(&i_default, &req2).unwrap());
        // fn read(b) → min 1 max 1 : satisfies arity 1, NOT arity 2
        let i_one = iface_instance(iface_class(&env, "O", vec![("read", iface_method(vec![iface_param("b", false, false)]))], None));
        assert!(interp.conforms(&i_one, &req1).unwrap());
        assert!(!interp.conforms(&i_one, &req2).unwrap());
        // fn read(...xs) → min 0 max ∞ : satisfies any arity
        let i_rest = iface_instance(iface_class(&env, "V", vec![("read", iface_method(vec![iface_param("xs", false, true)]))], None));
        assert!(interp.conforms(&i_rest, &req1).unwrap());
        assert!(interp.conforms(&i_rest, &req2).unwrap());
    }

    #[test]
    fn conforms_req_has_rest_needs_variadic_method() {
        let interp = Interp::new();
        let env = global_env().child();
        // A requirement that itself declares a rest param.
        let mut own = IndexMap::new();
        own.insert("read".to_string(), MethodReq { arity: 0, has_rest: true });
        let req = std::rc::Rc::new(InterfaceDef {
            name: "VReader".to_string(),
            own_methods: own,
            extends: vec![],
            def_env: env.clone(),
            flat: std::cell::RefCell::new(None),
        });
        // a non-variadic method does NOT satisfy a variadic requirement
        let fixed = iface_class(&env, "Fixed", vec![("read", iface_method(vec![iface_param("b", false, false)]))], None);
        assert!(!interp.conforms(&iface_instance(fixed), &req).unwrap());
        // a variadic method does
        let var = iface_class(&env, "Var", vec![("read", iface_method(vec![iface_param("xs", false, true)]))], None);
        assert!(interp.conforms(&iface_instance(var), &req).unwrap());
    }

    #[test]
    fn conforms_composition_and_forward_ref() {
        let interp = Interp::new();
        let env = global_env().child();
        // ReadWriter extends Reader, Writer — declared BEFORE Reader/Writer (forward ref)
        let rw = iface_def(&env, "ReadWriter", vec![], vec!["Reader", "Writer"]);
        let _reader = iface_def(&env, "Reader", vec![("read", 1)], vec![]);
        let _writer = iface_def(&env, "Writer", vec![("write", 1)], vec![]);
        // a class with both methods conforms; missing one does not
        let both = iface_class(&env, "Both", vec![
            ("read", iface_method(vec![iface_param("b", false, false)])),
            ("write", iface_method(vec![iface_param("b", false, false)])),
        ], None);
        assert!(interp.conforms(&iface_instance(both), &rw).unwrap());
        let only_read = iface_class(&env, "OnlyRead", vec![("read", iface_method(vec![iface_param("b", false, false)]))], None);
        assert!(!interp.conforms(&iface_instance(only_read), &rw).unwrap());
    }

    #[test]
    fn conforms_transitive_extends_of_extends() {
        let interp = Interp::new();
        let env = global_env().child();
        // C extends B ; B extends A ; A { fn a() }
        let _a = iface_def(&env, "A", vec![("a", 0)], vec![]);
        let _b = iface_def(&env, "B", vec![("b", 0)], vec!["A"]);
        let c = iface_def(&env, "C", vec![("c", 0)], vec!["B"]);
        let full = iface_class(&env, "Full", vec![
            ("a", iface_method(vec![])),
            ("b", iface_method(vec![])),
            ("c", iface_method(vec![])),
        ], None);
        assert!(interp.conforms(&iface_instance(full), &c).unwrap());
        let missing_a = iface_class(&env, "MissingA", vec![
            ("b", iface_method(vec![])),
            ("c", iface_method(vec![])),
        ], None);
        assert!(!interp.conforms(&iface_instance(missing_a), &c).unwrap());
    }

    #[test]
    fn conforms_cyclic_extends_is_recoverable_panic() {
        let interp = Interp::new();
        let env = global_env().child();
        // A extends B ; B extends A
        let a = iface_def(&env, "A", vec![], vec!["B"]);
        let _b = iface_def(&env, "B", vec![], vec!["A"]);
        let inst = iface_instance(iface_class(&env, "X", vec![], None));
        let err = interp.conforms(&inst, &a).unwrap_err();
        let msg = panic_of(err).message;
        assert!(msg.contains("cyclic interface extends"), "got: {msg}");
    }

    #[test]
    fn conforms_bad_extends_is_recoverable_panic() {
        let interp = Interp::new();
        let env = global_env().child();
        // extends a name that resolves to a class, not an interface
        // (iface_class already binds "NotIface" as a module-global Value::class).
        let _cls = iface_class(&env, "NotIface", vec![], None);
        let a = iface_def(&env, "A", vec![], vec!["NotIface"]);
        let inst = iface_instance(iface_class(&env, "X", vec![], None));
        let err = interp.conforms(&inst, &a).unwrap_err();
        assert!(panic_of(err).message.contains("not an interface"));
        // extends an unknown name
        let env2 = global_env().child();
        let b = iface_def(&env2, "B", vec![], vec!["Nope"]);
        let inst2 = iface_instance(iface_class(&env2, "Y", vec![], None));
        let err2 = interp.conforms(&inst2, &b).unwrap_err();
        assert!(panic_of(err2).message.contains("unknown name"));
    }

    // ---- IFACE Task 5: tree-walker exec + instanceof dispatch + contract path ----

    #[tokio::test]
    async fn iface_decl_binds_printable_interface_global() {
        let out = run("interface R { fn read(b): int }\nprint(R)").await;
        assert_eq!(out, "<interface R>\n");
    }

    #[tokio::test]
    async fn iface_instanceof_structural() {
        let src = "
interface R { fn read(b): int }
class File { fn read(b) { return 0 } }
class NoRead { fn write(b) { return 0 } }
print(File() instanceof R)
print(NoRead() instanceof R)
print(5 instanceof R)
";
        assert_eq!(run(src).await, "true\nfalse\nfalse\n");
    }

    #[tokio::test]
    async fn iface_instanceof_bad_rhs_message() {
        // `x instanceof <a number value>` → the new "class or interface" message.
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let stmts = parse(&lex("let n = 5\nlet x = 3\nx instanceof n").unwrap()).unwrap();
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        let res = local
            .run_until(async { interp.exec(&stmts, &env).await })
            .await;
        let msg = panic_of(res.unwrap_err()).message;
        assert!(
            msg.contains("instanceof requires a class or interface on the right-hand side"),
            "got: {msg}"
        );
    }

    #[tokio::test]
    async fn iface_class_instanceof_still_nominal() {
        // The class `instanceof` path is unchanged (subclass walk).
        let src = "
class A {}
class B extends A {}
print(B() instanceof A)
print(A() instanceof B)
";
        assert_eq!(run(src).await, "true\nfalse\n");
    }

    #[tokio::test]
    async fn iface_param_contract_accepts_conforming_rejects_non() {
        // A conforming arg is accepted; a non-conforming one panics with a contract.
        let ok = run("
interface R { fn read(b): int }
class File { fn read(b) { return 7 } }
fn slurp(r: R) { return r.read(0) }
print(slurp(File()))
").await;
        assert_eq!(ok, "7\n");

        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let bad_src = "
interface R { fn read(b): int }
class NoRead { fn write(b) { return 0 } }
fn slurp(r: R) { return 0 }
slurp(NoRead())
";
        let stmts = parse(&lex(bad_src).unwrap()).unwrap();
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        let res = local.run_until(async { interp.exec(&stmts, &env).await }).await;
        assert!(matches!(res, Err(Control::Panic(_))), "non-conforming arg should panic");
    }

    #[tokio::test]
    async fn iface_named_resolving_to_class_still_nominal_contract() {
        // A `Named` annotation that resolves to a CLASS keeps nominal-by-identity.
        let ok = run("
class C { fn m() { return 1 } }
fn f(c: C) { return c.m() }
print(f(C()))
").await;
        assert_eq!(ok, "1\n");
    }

    #[tokio::test]
    async fn iface_unresolved_name_preserves_prior_behavior() {
        // An annotation naming nothing falls through to the env-FREE `check_type`
        // (byte-identical to pre-IFACE): a non-instance value is still rejected — we
        // must NOT change this. (The env-aware path only ADDS interface/class
        // resolution; the unresolved leaf is unchanged.)
        let interp = std::rc::Rc::new(Interp::new());
        interp.install_self();
        let stmts = parse(&lex("fn f(x: Unknown) { return x }\nf(42)").unwrap()).unwrap();
        let env = global_env().child();
        let local = tokio::task::LocalSet::new();
        let res = local.run_until(async { interp.exec(&stmts, &env).await }).await;
        assert!(matches!(res, Err(Control::Panic(_))), "unknown-name contract unchanged");
    }

    #[tokio::test]
    async fn iface_nested_array_contract_resolves_elementwise() {
        let ok = run("
interface R { fn read(b): int }
class File { fn read(b) { return 0 } }
fn many(rs: array<R>) { return len(rs) }
print(many([File(), File()]))
").await;
        assert_eq!(ok, "2\n");
    }

    #[test]
    fn conforms_verdict_cache_warm_equals_cold() {
        let interp = Interp::new();
        let env = global_env().child();
        let reader = iface_def(&env, "Reader", vec![("read", 1)], vec![]);
        let file = iface_class(&env, "File", vec![("read", iface_method(vec![iface_param("b", false, false)]))], None);
        let inst = iface_instance(file);
        // cold (first) call
        let cold = interp.conforms(&inst, &reader).unwrap();
        // warm (cached) calls give the SAME answer, repeatedly
        for _ in 0..5 {
            assert_eq!(interp.conforms(&inst, &reader).unwrap(), cold);
        }
        assert!(cold);
    }

    // ---- SRV Task 4/5: read dispatch + mutation panic on a frozen `Shared` ----

    fn frozen_cfg() -> Value {
        use crate::value::{SharedKey, SharedNode};
        use std::sync::Arc;
        // { region: "us", limits: [10, 100], m: map{"a": 1} }
        let limits = Arc::new(SharedNode::Array(Arc::from(vec![
            Arc::new(SharedNode::Int(10)),
            Arc::new(SharedNode::Int(100)),
        ])));
        let m = Arc::new(SharedNode::Map(Arc::new(vec![(
            SharedKey::Str("a".into()),
            Arc::new(SharedNode::Int(1)),
        )])));
        Value::shared(Arc::new(SharedNode::Object(Arc::new(vec![
            ("region".into(), Arc::new(SharedNode::Str("us".into()))),
            ("limits".into(), limits),
            ("m".into(), m),
        ]))))
    }

    fn node_of(v: &Value) -> std::sync::Arc<crate::value::SharedNode> {
        match v.kind() {
            ValueKind::Shared(n) => n.clone(),
            _ => panic!("not shared"),
        }
    }

    #[test]
    fn shared_index_and_member_reads() {
        let cfg = frozen_cfg();
        let n = node_of(&cfg);
        let sp = Span::new(0, 0);
        // member: scalar
        assert_eq!(
            shared_read_member(&n, "region", sp).unwrap(),
            Value::str("us")
        );
        // member: descend → Shared view (stays zero-copy → type "array")
        let limits = shared_read_member(&n, "limits", sp).unwrap();
        assert!(matches!(limits.kind(), ValueKind::Shared(_)));
        assert_eq!(type_name(&limits), "array");
        // member missing → nil
        assert_eq!(shared_read_member(&n, "nope", sp).unwrap(), Value::nil());
        // index: array by int
        let lnode = node_of(&limits);
        assert_eq!(
            shared_index_get(&lnode, &Value::int(0), sp, sp).unwrap(),
            Value::int(10)
        );
        // index: OOB → error (matches live array)
        assert!(shared_index_get(&lnode, &Value::int(9), sp, sp).is_err());
        // index: object by string key
        assert_eq!(
            shared_index_get(&n, &Value::str("region"), sp, sp).unwrap(),
            Value::str("us")
        );
        // index: map by key
        let mnode = node_of(&shared_read_member(&n, "m", sp).unwrap());
        assert_eq!(
            shared_index_get(&mnode, &Value::str("a"), sp, sp).unwrap(),
            Value::int(1)
        );
    }

    #[test]
    fn shared_read_only_methods() {
        let cfg = frozen_cfg();
        let n = node_of(&cfg);
        let sp = Span::new(0, 0);
        assert_eq!(
            call_shared(&n, "has", &[Value::str("region")], sp).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            call_shared(&n, "has", &[Value::str("nope")], sp).unwrap(),
            Value::bool_(false)
        );
        assert_eq!(
            call_shared(&n, "get", &[Value::str("region")], sp).unwrap(),
            Value::str("us")
        );
        assert_eq!(
            call_shared(
                &n,
                "get",
                &[Value::str("x"), Value::int(7)],
                sp
            )
            .unwrap(),
            Value::int(7)
        );
        assert_eq!(call_shared(&n, "len", &[], sp).unwrap(), Value::int(3));
        // keys
        match call_shared(&n, "keys", &[], sp).unwrap().kind() {
            ValueKind::Array(a) => assert_eq!(a.borrow().len(), 3),
            _ => panic!(),
        }
    }

    #[test]
    fn shared_mutation_methods_panic_with_shipped_wording() {
        let cfg = frozen_cfg();
        let n = node_of(&cfg);
        let sp = Span::new(0, 0);
        // A mutating method → the SHIPPED `cannot mutate a frozen {kind}` message.
        let err = call_shared(&n, "push", &[Value::int(1)], sp).unwrap_err();
        match err {
            Control::Panic(e) => assert_eq!(e.message, "cannot mutate a frozen object"),
            _ => panic!("expected panic"),
        }
        // On a frozen ARRAY, the same method names the array kind.
        let arr = node_of(&shared_read_member(&n, "limits", sp).unwrap());
        let err = call_shared(&arr, "push", &[Value::int(1)], sp).unwrap_err();
        assert!(
            matches!(err, Control::Panic(ref e) if e.message == "cannot mutate a frozen array")
        );
    }

    #[test]
    fn shared_index_assign_panics_via_frozen_kind() {
        // index_set already calls frozen_kind → a Shared write is rejected with the
        // shipped wording, NO new code (Task 5).
        let cfg = frozen_cfg();
        let sp = Span::new(0, 0);
        let err = index_set(
            &cfg,
            &Value::str("region"),
            Value::str("eu"),
            sp,
            sp,
        )
        .unwrap_err();
        assert_eq!(err.message, "cannot mutate a frozen object");
        // check_not_frozen likewise rejects it.
        let err2 = check_not_frozen(&cfg, sp).unwrap_err();
        assert!(matches!(err2, Control::Panic(ref e) if e.message == "cannot mutate a frozen object"));
    }

    #[test]
    fn shared_frozen_instance_method_has_distinct_diagnostic() {
        use crate::value::SharedNode;
        use std::sync::Arc;
        let inst = Arc::new(SharedNode::Instance {
            class_name: "User".into(),
            fields: Arc::new(vec![("id".into(), Arc::new(SharedNode::Int(1)))]),
        });
        let sp = Span::new(0, 0);
        // A read-only structural method still works on the instance's fields.
        assert_eq!(
            call_shared(&inst, "has", &[Value::str("id")], sp).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            shared_read_member(&inst, "id", sp).unwrap(),
            Value::int(1)
        );
        // A user-method call → the DISTINCT diagnostic (NOT the mutation panic).
        let err = call_shared(&inst, "promote", &[], sp).unwrap_err();
        match err {
            Control::Panic(e) => {
                assert!(
                    e.message.contains("not available on a frozen instance"),
                    "{}",
                    e.message
                );
                assert_ne!(e.message, "cannot mutate a frozen instance");
            }
            _ => panic!(),
        }
    }

    /// DX D2 Task 5 — the `TestSummary ↔ Value::object_cell` airlock conversion is
    /// lossless: passed/failed counts and every `(name, message)` failure survive the
    /// round-trip in order, AND through the worker structured-clone encode/decode
    /// (which crosses ONLY existing sendable `Value` kinds — no new kind).
    #[test]
    fn test_summary_value_roundtrip_is_lossless() {
        let summary = TestSummary {
            passed: 3,
            failed: 2,
            filtered: 1,
            failures: vec![
                ("alpha".to_string(), "expected 1 got 2".to_string()),
                ("beta".to_string(), "boom".to_string()),
            ],
        };
        // Direct Value round-trip.
        let v = summary.to_value();
        let back = TestSummary::from_value(&v).expect("decodes");
        assert_eq!(back.passed, 3);
        assert_eq!(back.failed, 2);
        assert_eq!(back.filtered, 1);
        assert_eq!(
            back.failures,
            vec![
                ("alpha".to_string(), "expected 1 got 2".to_string()),
                ("beta".to_string(), "boom".to_string()),
            ]
        );

        // Through the worker airlock (encode → decode): the shape is sendable.
        crate::worker::serialize::check_sendable(&v).expect("summary object is sendable");
        let (bytes, shared) = crate::worker::serialize::encode(&v).expect("encodes");
        let interp = Interp::new();
        let decoded =
            crate::worker::serialize::decode_with_shared(&bytes, &shared, &interp).expect("decodes");
        let back2 = TestSummary::from_value(&decoded).expect("decodes from airlock object");
        assert_eq!(back2.passed, 3);
        assert_eq!(back2.failed, 2);
        assert_eq!(back2.failures, summary.failures);
    }

    /// A malformed isolate result (wrong shape) yields `None`, never a panic — the
    /// defensive guard that keeps a corrupt worker reply a clean error.
    #[test]
    fn test_summary_from_value_rejects_malformed() {
        assert!(TestSummary::from_value(&Value::int(5)).is_none());
        let mut m = indexmap::IndexMap::new();
        m.insert("passed".to_string(), Value::int(1));
        // missing `failed` and `failures`
        assert!(TestSummary::from_value(&Value::object(m)).is_none());
    }

    // -----------------------------------------------------------------------
    // DEFER §3 TDD tests — tree-walker oracle (Tasks 2.1/2.2)
    // -----------------------------------------------------------------------

    // §3.1: args evaluated at statement time (not at function exit)
    #[tokio::test]
    async fn defer_args_evaluated_at_statement() {
        let out = run(r#"
fn f() {
    let x = 1
    defer print(x)
    x = 2
    print("body")
}
f()
"#).await;
        assert_eq!(out.trim(), "body\n1");
    }

    // §3.2: LIFO drain order
    #[tokio::test]
    async fn defer_lifo_order() {
        let out = run(r#"
fn f() {
    defer print(1)
    defer print(2)
    defer print(3)
    print("body")
}
f()
"#).await;
        assert_eq!(out.trim(), "body\n3\n2\n1");
    }

    // §2.3: function-scoped, not block-scoped
    #[tokio::test]
    async fn defer_function_scoped_not_block() {
        let out = run(r#"
fn f() {
    if (true) {
        defer print("deferred")
    }
    print("body")
}
f()
"#).await;
        assert_eq!(out.trim(), "body\ndeferred");
    }

    // §2.3: defer in loop accumulates entries, all run LIFO at function exit
    #[tokio::test]
    async fn defer_in_loop_accumulates() {
        let out = run(r#"
fn f() {
    for (i in 1..=3) {
        defer print(i)
    }
    print("body")
}
f()
"#).await;
        assert_eq!(out.trim(), "body\n3\n2\n1");
    }

    // §3.1: closure captures by cell — sees mutation (by-cell capture semantics)
    #[tokio::test]
    async fn defer_closure_sees_mutation() {
        let out = run(r#"
fn f() {
    let x = 1
    defer (() => print(x))()
    x = 2
}
f()
"#).await;
        assert_eq!(out.trim(), "2");
    }

    // §3.1: opt-chain nil receiver — no entry pushed, side effects NOT evaluated
    #[tokio::test]
    async fn defer_optchain_nil_no_side_effect() {
        let out = run(r#"
fn side() { print("side") return 1 }
fn f() {
    let a = nil
    defer a?.m(side())
    print("body")
}
f()
"#).await;
        assert_eq!(out.trim(), "body");
    }

    // §3.1: member receiver captured at statement time, not at exit
    #[tokio::test]
    async fn defer_member_recv_evaluated_at_statement() {
        let out = run(r#"
class A { fn show() { print("A") } }
class B { fn show() { print("B") } }
fn f() {
    let o = A()
    defer o.show()
    o = B()
}
f()
"#).await;
        assert_eq!(out.trim(), "A");
    }

    // §3.1: spread args materialized at statement time (later mutation invisible)
    #[tokio::test]
    async fn defer_spread_materialized_at_statement() {
        // §3.1: spread args are evaluated at the defer statement site, not at drain time.
        // Reassigning xs after the defer stmt does NOT affect the captured args.
        let out = run(r#"
fn collect(...xs) { print(xs) }
fn f() {
    let xs = [1, 2]
    defer collect(...xs)
    xs = [1, 2, 9]
}
f()
"#).await;
        assert_eq!(out.trim(), "[1, 2]");
    }

    // §2.3: top-level defer runs at program end (before the process exits)
    #[tokio::test]
    async fn defer_top_level_runs_at_program_end() {
        let out = run(r#"
defer print("end")
print("start")
"#).await;
        assert_eq!(out.trim(), "start\nend");
    }

    // §3.3: defers drain on normal return
    #[tokio::test]
    async fn defer_runs_on_return() {
        let out = run(r#"
fn f() {
    defer print("deferred")
    print("before return")
    return 42
    print("unreachable")
}
f()
"#).await;
        assert_eq!(out.trim(), "before return\ndeferred");
    }

    // §3.3: defers drain on ? propagation (the [nil, err] pair is the pending outcome)
    #[tokio::test]
    async fn defer_runs_on_propagate() {
        let out = run(r#"
fn inner() { return [nil, "err"] }
fn f() {
    defer print("deferred")
    inner()?
    print("unreachable")
}
let [_, e] = f()
print(e)
"#).await;
        assert_eq!(out.trim(), "deferred\nerr");
    }

    // §3.3: defers drain on panic unwind, innermost-first
    #[tokio::test]
    async fn defer_runs_on_panic_unwind() {
        let out = run(r#"
fn inner() {
    defer print("inner defer")
    panic("boom")
}
fn outer() {
    defer print("outer defer")
    inner()
}
recover(() => outer())
print("done")
"#).await;
        assert_eq!(out.trim(), "inner defer\nouter defer\ndone");
    }

    // §3.6 r1: defer panic replaces normal return value — caller sees the panic
    #[tokio::test]
    async fn defer_panic_replaces_return() {
        let err = run_err(r#"
fn f() {
    defer assert(false, "cleanup failed")
    return 42
}
f()
"#).await;
        assert!(err.message.contains("cleanup failed"), "got: {}", err.message);
    }

    // §3.6 r2: defer panic supersedes ? propagation
    #[tokio::test]
    async fn defer_panic_supersedes_propagate() {
        let err = run_err(r#"
fn inner() { return [nil, "orig err"] }
fn f() {
    defer assert(false, "cleanup failed")
    inner()?
}
f()
"#).await;
        assert!(err.message.contains("cleanup failed"), "got: {}", err.message);
        assert!(!err.message.contains("orig err"), "should not contain orig err, got: {}", err.message);
    }

    // §3.6 r3: defer panic during an in-flight panic appends the EXACT suppressed note
    #[tokio::test]
    async fn defer_panic_during_panic_appends_note() {
        let err = run_err(r#"
fn f() {
    defer assert(false, "cleanup failed")
    assert(false, "boom")
}
f()
"#).await;
        assert_eq!(
            err.message,
            "boom (suppressed panic in deferred call: cleanup failed)",
            "got: {}",
            err.message
        );
    }

    // §3.6 r4: a panicking defer doesn't skip later (older) defers
    #[tokio::test]
    async fn defer_middle_panic_all_run() {
        // §3.6 r4: a panicking defer does NOT skip later (older) defers.
        // Defers run LIFO: print(3) first, then panicking defer, then print(1).
        let out = run(r#"
fn f() {
    defer print(1)
    defer assert(false, "mid panic")
    defer print(3)
    assert(false, "boom")
}
let [_, e] = recover(() => f())
print(e.message)
"#).await;
        // All three defers ran: 3 then 1 printed; "mid panic" appended as suppressed note.
        assert!(out.contains("3"), "should see 3 in output, got: {}", out);
        assert!(out.contains("1"), "should see 1 in output, got: {}", out);
        assert!(out.contains("boom"), "should see 'boom' in output, got: {}", out);
    }

    // §3.7: deferred print runs BEFORE the failing return-type contract panic
    #[tokio::test]
    async fn defer_runs_before_return_contract_panic() {
        let out = run(r#"
fn f(): string {
    defer print("deferred ran")
    return 42
}
recover(() => f())
print("done")
"#).await;
        assert!(out.contains("deferred ran"), "defer should run before contract, got: {}", out);
        assert!(out.contains("done"), "done should be printed, got: {}", out);
    }

    // §3.2: result of a deferred call is discarded (Tier-1 [nil,err] pair ignored)
    #[tokio::test]
    async fn defer_result_discarded() {
        let out = run(r#"
fn might_fail() { return [nil, "e"] }
fn f() {
    defer might_fail()
    print("body")
}
f()
"#).await;
        assert_eq!(out.trim(), "body");
    }

    // §3.4: bare (non-awaited) defer of an async fn raises the exact Tier-2 message
    #[tokio::test]
    async fn defer_bare_future_panics() {
        let err = run_err(r#"
async fn cleanup() { }
fn f() {
    defer cleanup()
    print("body")
}
f()
"#).await;
        assert!(
            err.message.contains("deferred call returned a future"),
            "got: {}",
            err.message
        );
        assert!(err.message.contains("defer await"), "got: {}", err.message);
    }

    // §3.4: `defer await f()` drives the future to completion
    #[tokio::test]
    async fn defer_await_happy_path() {
        let out = run(r#"
async fn cleanup() { print("async cleanup") }
async fn f() {
    defer await cleanup()
    print("body")
}
await f()
"#).await;
        assert_eq!(out.trim(), "body\nasync cleanup");
    }

    // recover() sees the panic only AFTER every frame's defers have run
    #[tokio::test]
    async fn defer_recover_sees_panic_after_defers() {
        let out = run(r#"
fn f() {
    defer print("cleanup")
    assert(false, "original")
}
let [_, e] = recover(() => f())
print(e.message)
"#).await;
        assert_eq!(out.trim(), "cleanup\noriginal");
    }

    // -----------------------------------------------------------------------
    // DEFER §5.1 — concurrency soundness test
    //
    // Regression guard for a FIXED bug. The original design stored defer lists on
    // a single `Interp.defers` `RefCell<Vec<…>>` stack shared by ALL concurrent
    // `spawn_local` tasks, resolving "the current frame" with `.last()`. Under M17
    // multiple `async fn` activations are concurrently live on the SAME `Interp`,
    // so when A parks at an `.await`, B runs and pushes ITS frame, then A resumes,
    // `.last()` returns B's frame — A's defer landed on B's frame (cross-frame
    // contamination). The fix (spec §5.1) attaches the defer list to the ENV SCOPE,
    // resolved via the env chain (`env.defer_scope()`), so each activation always
    // resolves ITS OWN list regardless of interleaving. This test stays in the
    // suite forever; it FAILED on the Interp-stack design and PASSES on env-scope.
    //
    // This test forces that exact interleaving:
    //   - async fn A awaits a signal, THEN registers `defer print("A")`
    //   - async fn B is spawned while A is suspended; it registers
    //     `defer print("B")`, then signals A before exiting
    //   - Expected (CORRECT): A's defer prints "A" when A exits; B's defer
    //     prints "B" when B exits (no cross-contamination, correct LIFO in each)
    //   - Bug symptom: A's defer is pushed onto B's frame and either runs with B
    //     or is lost entirely; the output order / attribution is wrong
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn defer_async_activation_no_cross_contamination() {
        // Two async fns coordinate via a shared channel. The interleaving is:
        //   1. outer starts async fn A (gets a future handle, A starts running)
        //   2. A awaits the channel (parks)
        //   3. B runs, registers `defer print("B-defer")`, signals A, returns
        //   4. A resumes, registers `defer print("A-defer")`, returns
        //
        // With correct per-activation defer lists:
        //   - B drains: prints "B-defer" (B's own defer)
        //   - A drains: prints "A-defer" (A's own defer)
        //
        // With the Interp-stack bug:
        //   - When A resumes (step 4), the stack top is B's frame (B pushed last)
        //     => A's defer lands on B's frame
        //   - B then has TWO defers: ["B-defer", "A-defer"] (LIFO order: A-defer, B-defer)
        //   - A has ZERO defers (its frame was empty when it drained)
        //   - Output would be: "A-defer\nB-defer" (from B's drain) then nothing for A
        //     instead of "B-defer\nA-defer" (each from their own drain)
        let out = run(r#"
import * as sync from "std/sync"

async fn task_b(ch) {
    defer print("B-defer")
    sync.send(ch, "signal")
}

async fn task_a(ch) {
    defer print("A-defer")
    let _ = await sync.recv(ch)
    // We are now resumed AFTER B has already started but may not have finished.
    // The defer registration here should land on A's OWN frame, not B's.
    print("A-body-resumed")
}

let ch = sync.channel(1)
let fa = task_a(ch)
let fb = task_b(ch)
await fa
await fb
print("done")
"#).await;
        // The correct output: both task bodies run, each defer fires for its own frame.
        // "A-body-resumed" printed when A resumes.
        // "B-defer" when B's frame exits (B's cleanup).
        // "A-defer" when A's frame exits (A's cleanup).
        // "done" from the outer program.
        //
        // The ordering of A-defer vs B-defer depends on scheduling, but the KEY
        // invariant is that BOTH defers ran and that "A-defer" came from A's exit
        // and "B-defer" came from B's exit (no duplicates, no missing).
        assert!(out.contains("A-defer"), "A's defer must run: got {out:?}");
        assert!(out.contains("B-defer"), "B's defer must run: got {out:?}");
        assert!(out.contains("A-body-resumed"), "A must resume: got {out:?}");
        assert!(out.contains("done"), "program must complete: got {out:?}");
        // Check that "A-defer" appears exactly once and "B-defer" exactly once
        // (no cross-contamination doubling or loss).
        let a_count = out.matches("A-defer").count();
        let b_count = out.matches("B-defer").count();
        assert_eq!(a_count, 1, "A-defer should appear exactly once, got {a_count}: {out:?}");
        assert_eq!(b_count, 1, "B-defer should appear exactly once, got {b_count}: {out:?}");
    }

    // Simpler version without channels: two concurrently awaited async fns, each
    // with its own defer. task.gather drives both concurrently.
    #[tokio::test]
    async fn defer_async_gather_no_cross_contamination() {
        let out = run(r#"
import * as task from "std/task"

async fn worker_a() {
    defer print("A-defer")
    print("A-body")
}

async fn worker_b() {
    defer print("B-defer")
    print("B-body")
}

await task.gather([worker_a(), worker_b()])
print("done")
"#).await;
        assert!(out.contains("A-defer"), "A-defer missing: {out:?}");
        assert!(out.contains("B-defer"), "B-defer missing: {out:?}");
        let a_count = out.matches("A-defer").count();
        let b_count = out.matches("B-defer").count();
        assert_eq!(a_count, 1, "A-defer doubled or lost: {out:?}");
        assert_eq!(b_count, 1, "B-defer doubled or lost: {out:?}");
        assert!(out.contains("done"), "{out:?}");
    }

    // ADVERSARIAL interleaving: Task B is CONCURRENTLY LIVE (its run_body frame is
    // on the Interp.defers stack) when Task A registers its defer.
    //
    // Scenario:
    //   1. Task A starts, enters run_body → pushes frameA.
    //   2. Task A awaits ch_a2b (parks).
    //   3. Task B starts, enters run_body → pushes frameB. Stack: [prog, frameA, frameB]
    //   4. Task B registers `defer print("B-defer")` → frameB (correct).
    //   5. Task B sends on ch_a2b (unblocks A) then awaits ch_b2a (B still live!).
    //      Stack is STILL [prog, frameA, frameB] at this point.
    //   6. Task A resumes, registers `defer print("A-defer")`.
    //      With the Interp-stack approach, `last()` returns frameB — A's defer
    //      goes to B's frame (BUG). With per-env lists (spec §5.1), A's defer
    //      correctly goes to A's scope.
    //   7. Task A body finishes. A's frame drains.
    //   8. Task A sends on ch_b2a (unblocks B). B resumes, body finishes. B's frame drains.
    //
    // If the bug exists:
    //   - A's frame has 0 defers (drain is a no-op)
    //   - B's frame has 2 defers: "B-defer" and "A-defer" (LIFO: A-defer, B-defer)
    //   - Output: "A-defer" then "B-defer" (both from B's drain)
    //   - "A-defer" appears once, "B-defer" appears once — count checks alone can't detect it!
    //
    // To DETECT the bug, we need to verify the ORDERING:
    //   - CORRECT: A's body completes first (A doesn't wait for B after unblocking),
    //     so "A-defer" prints BEFORE "B-body-done" (B's final print before B sends on ch_b2a).
    //     Then B drains printing "B-defer". Then B prints "B-done-after-A-unblocked".
    //   - BUG: A has no defers, B gets both, so the ordering differs.
    //
    // Actually: let's use a DIRECT Rust-level probe. We check the defer list Rc
    // pointer identity at the moment of `defer` registration — but that's hard from script.
    //
    // Better: check whether A's defer prints BETWEEN A's two body prints (A-start, A-end),
    // meaning it ran when A's frame exited, not when B's frame exited.
    //
    // Output ordering invariant we can assert:
    //   - "A-defer" must appear BETWEEN "A-body-done" and "B-body-done" if and only if
    //     it ran from A's drain — i.e., A's drain runs before B completes.
    //   - If the bug, "A-defer" runs from B's drain (after B's body-done).
    #[tokio::test]
    async fn defer_adversarial_interleaving_ordering() {
        let out = run(r#"
import * as sync from "std/sync"

// ch_a2b: A sends to unblock B (after B has parked waiting on it — confusingly named)
// Actually: A blocks on ch_ab until B sends; B blocks on ch_ba until A sends.
// ch_ab: A awaits this (A blocked first), B sends to unblock A
// ch_ba: B awaits this after unblocking A, A sends to unblock B

async fn task_a(ch_ab, ch_ba) {
    print("A-start")
    let _ = await sync.recv(ch_ab)  // Park A; B will unblock us
    // AT THIS POINT: if B's frame is still on the stack, our defer goes to B's frame.
    defer print("A-defer")
    print("A-body-done")
    // Unblock B (let B complete)
    sync.send(ch_ba, "go")
}

async fn task_b(ch_ab, ch_ba) {
    print("B-start")
    defer print("B-defer")
    sync.send(ch_ab, "go")          // Unblock A
    let _ = await sync.recv(ch_ba)  // Park B; A will unblock us after registering its defer
    print("B-body-done")
}

let ch_ab = sync.channel(1)
let ch_ba = sync.channel(1)
let fa = task_a(ch_ab, ch_ba)
let fb = task_b(ch_ab, ch_ba)
await fa
await fb
print("done")
"#).await;
        // Sanity: all parts ran
        assert!(out.contains("A-start"), "A-start: {out:?}");
        assert!(out.contains("B-start"), "B-start: {out:?}");
        assert!(out.contains("A-body-done"), "A-body-done: {out:?}");
        assert!(out.contains("B-body-done"), "B-body-done: {out:?}");
        assert!(out.contains("A-defer"), "A-defer missing: {out:?}");
        assert!(out.contains("B-defer"), "B-defer missing: {out:?}");
        assert!(out.contains("done"), "done: {out:?}");
        // Exactly one of each
        assert_eq!(out.matches("A-defer").count(), 1, "A-defer count: {out:?}");
        assert_eq!(out.matches("B-defer").count(), 1, "B-defer count: {out:?}");

        // ORDERING INVARIANT (the definitive soundness check):
        //
        // "A-defer" must appear AFTER "A-body-done" (it's deferred, runs at A's exit)
        // and BEFORE "B-body-done" (because A unblocks B only AFTER A's defer runs,
        // and B prints "B-body-done" before B itself exits).
        //
        // Timeline of correct execution:
        //   A: A-start → (park) → (wake) → A-body-done → [A exits → A-defer prints]
        //   B: B-start → B-defer registered → send → (park) → (wake by A after A-defer) →
        //      B-body-done → [B exits → B-defer prints]
        //
        // So correct ordering: A-body-done, A-defer, B-body-done, B-defer
        //
        // Bug ordering (A's defer lands on B's frame):
        //   A: A-start → (park) → (wake) → A-body-done → [A exits, no defers!]
        //   B: B-start → B-defer registered → send → (park) → (wake by A) →
        //      B-body-done → [B exits → B drains: A-defer prints, B-defer prints]
        //
        // So bug ordering: A-body-done, B-body-done, A-defer, B-defer
        //
        // We assert: "A-defer" appears before "B-body-done"
        let a_defer_pos = out.find("A-defer").unwrap();
        let b_body_done_pos = out.find("B-body-done").unwrap();
        assert!(
            a_defer_pos < b_body_done_pos,
            "SOUNDNESS BUG: A-defer should appear before B-body-done \
             (defer went to wrong frame). Output:\n{out}"
        );
    }

    // -----------------------------------------------------------------------
    // DEFER Task 2.3 — async / generator / cancellation matrix (§3.4 §4.1 §4.2 §4.3)
    //
    // These tests prove the hard async edges of the defer spec on the tree-walker.
    // No VM / CST changes — tree-walker oracle only.
    // -----------------------------------------------------------------------

    // §3.4 §4.1 — defer await happy path: async cleanup fn completes before the
    // caller's frame resolves.  The print order proves await drove the future.
    #[tokio::test]
    async fn defer_await_happy_path_async_cleanup() {
        let out = run(r#"
import * as sync from "std/sync"
// A side-effect channel lets us prove ordering without relying on print order alone.
let ch = sync.channel(1)

async fn teardown(ch) {
    sync.send(ch, "teardown-ran")
}

async fn work(ch) {
    defer await teardown(ch)
    print("body")
}

await work(ch)
let result = await sync.recv(ch)
print(result)
"#).await;
        // "body" from the work body; "teardown-ran" sent by teardown and then
        // received AFTER work() resolved — proves teardown completed inside work.
        assert_eq!(out.trim(), "body\nteardown-ran");
    }

    // §3.4 §4.1 — strict LIFO across mixed await and sync defers.
    // Three defers: sync-A (oldest), await-B (middle), sync-C (newest).
    // Drain order must be: C, then await B completes, then A.
    #[tokio::test]
    async fn defer_await_lifo_mixed_sync_and_await() {
        let out = run(r#"
async fn async_cleanup() { print("B-await") }

async fn f() {
    defer print("A-sync")         // oldest: runs last
    defer await async_cleanup()   // middle: await completes before A
    defer print("C-sync")         // newest: runs first
    print("body")
}

await f()
"#).await;
        assert_eq!(out.trim(), "body\nC-sync\nB-await\nA-sync");
    }

    // §3.4 §4.1 §3.3 — defer await runs during ?-propagation unwind.
    // The [nil, err] pair is delivered only AFTER the awaited defer completes.
    #[tokio::test]
    async fn defer_await_during_propagate_unwind() {
        let out = run(r#"
async fn async_close() { print("async-close") }

async fn risky() {
    defer await async_close()
    return Err("oops")
}

fn caller() {
    let v = (await risky())?
    return Ok(v)
}

let r = caller()
print(r[1].message)
"#).await;
        // async-close runs during the ?-unwind; the Err pair is delivered after.
        assert_eq!(out.trim(), "async-close\noops");
    }

    // §3.4 §3.6 §4.1 — recover sees the panic only after the awaited defer completed.
    #[tokio::test]
    async fn defer_await_during_panic_to_recover() {
        let out = run(r#"
async fn async_cleanup() { print("cleanup-ran") }

async fn boom() {
    defer await async_cleanup()
    assert(false, "original-panic")
}

let [_, e] = recover(() => await boom())
print(e.message)
"#).await;
        assert_eq!(out.trim(), "cleanup-ran\noriginal-panic");
    }

    // §3.4 — bare defer of a future-returning call is an exact-message Tier-2 panic.
    #[tokio::test]
    async fn bare_future_defer_panics_with_exact_message() {
        let err = run_err(r#"
async fn async_cleanup() { print("cleanup") }

fn f() {
    defer async_cleanup()   // bare defer of an async fn — must panic at drain time
}

f()
"#).await;
        assert_eq!(
            err.message,
            "deferred call returned a future that would be cancelled on drop \
             — use 'defer await f()' or do async cleanup before exit",
            "exact §3.4 message mismatch: {}", err.message
        );
    }

    // §3.4 — defer await on a sync call is the identity rule: no error, result discarded.
    #[tokio::test]
    async fn defer_await_on_sync_call_is_identity() {
        let out = run(r#"
fn sync_close() { print("sync-close") }

fn f() {
    defer await sync_close()   // await on a non-future is identity — must not error
    print("body")
}

f()
"#).await;
        assert_eq!(out.trim(), "body\nsync-close");
    }

    // §4.1 — a defer registered BEFORE an await runs at body exit however many
    // awaits later (the activation persists across awaits by construction).
    #[tokio::test]
    async fn async_fn_defers_persist_across_multiple_awaits() {
        let out = run(r#"
async fn step(n) { return n }

async fn multi_await() {
    defer print("deferred-cleanup")
    print("before-awaits")
    let a = await step(1)
    let b = await step(2)
    let c = await step(3)
    print("after-awaits")
    return a + b + c
}

let result = await multi_await()
print(result)
"#).await;
        // deferred-cleanup must appear after the body completes (after "after-awaits")
        // but before the caller's next statement.
        assert!(out.contains("before-awaits"), "body start: {out:?}");
        assert!(out.contains("after-awaits"), "body end: {out:?}");
        assert!(out.contains("deferred-cleanup"), "defer ran: {out:?}");
        assert!(out.contains("6"), "return value: {out:?}");
        let cleanup_pos = out.find("deferred-cleanup").unwrap();
        let after_pos = out.find("after-awaits").unwrap();
        let result_pos = out.find('6').unwrap();
        assert!(
            after_pos < cleanup_pos,
            "defer must run AFTER body completes: {out:?}"
        );
        assert!(
            cleanup_pos < result_pos,
            "defer must run BEFORE caller resumes: {out:?}"
        );
    }

    // §4.3 — generator body completion (normal return after all yields) runs defers.
    #[tokio::test]
    async fn generator_completion_runs_defers() {
        let out = run(r#"
fn* gen() {
    defer print("gen-defer")
    yield 1
    yield 2
    // body completes here — defers must run before this resume reports done
}

let it = gen()
print(it.next())
print(it.next())
let done = it.next()  // drives body to completion, defer runs here
print("after-done")
"#).await;
        // gen-defer must appear before "after-done" (i.e., during the final resume
        // that drives the body to completion).
        assert!(out.contains("gen-defer"), "defer must run on completion: {out:?}");
        let defer_pos = out.find("gen-defer").unwrap();
        let after_done_pos = out.find("after-done").unwrap();
        assert!(
            defer_pos < after_done_pos,
            "gen-defer must appear before after-done: {out:?}"
        );
        assert!(out.contains("1\n2\n"), "yields must work: {out:?}");
    }

    // §4.3 — generator panic unwind also runs defers.
    #[tokio::test]
    async fn generator_panic_runs_defers() {
        let out = run(r#"
fn* panicky() {
    defer print("gen-panic-defer")
    yield 1
    assert(false, "gen-panic")
}

let it = panicky()
print(it.next())   // yields 1 fine
let [_, err] = recover(() => it.next())  // drives to panic; defer must run inside
print(err.message)
print("recovered")
"#).await;
        assert!(out.contains("gen-panic-defer"), "defer ran: {out:?}");
        assert!(out.contains("gen-panic\n"), "panic message in output: {out:?}");
        assert!(out.contains("recovered"), "recover worked: {out:?}");
        let defer_pos = out.find("gen-panic-defer").unwrap();
        // "gen-panic\n" is the standalone panic message line (not the "gen-panic-defer" line).
        let panic_msg_pos = out.find("gen-panic\n").unwrap();
        // The defer runs before the panic crosses the recover boundary.
        assert!(
            defer_pos < panic_msg_pos,
            "defer runs before panic surfaces: {out:?}"
        );
    }

    // §4.3 — gen.close() mid-suspend does NOT run defers (documented, soundness).
    // Two variants: explicit close() and last-handle drop (let the generator go
    // out of scope without driving to completion).
    #[tokio::test]
    async fn generator_close_does_not_run_defers() {
        // Variant 1: explicit close() mid-suspend.
        let out = run(r#"
fn* gen() {
    defer print("MUST-NOT-APPEAR-close")
    yield 1
    yield 2
}

let it = gen()
print(it.next())   // yields 1 (body parked at yield 2)
it.close()         // drops body mid-suspend — defer must NOT run
print("after-close")
"#).await;
        assert!(
            !out.contains("MUST-NOT-APPEAR-close"),
            "§4.3: close() must NOT run defers; got: {out:?}"
        );
        assert!(out.contains("after-close"), "program continued: {out:?}");

        // Variant 2: last-handle drop (generator abandoned after one yield).
        // The generator's handle goes out of scope at end of the helper fn;
        // the body is dropped mid-suspend (parked at the second yield).
        let out2 = run(r#"
fn* droppable() {
    defer print("MUST-NOT-APPEAR-drop")
    yield 1
    yield 2
}

fn abandon() {
    let it = droppable()
    print(it.next())   // yields 1; body now parked at second yield
    // `it` goes out of scope here — body dropped mid-suspend
}

abandon()
print("after-drop")
"#).await;
        assert!(
            !out2.contains("MUST-NOT-APPEAR-drop"),
            "§4.3: last-handle drop must NOT run defers; got: {out2:?}"
        );
        assert!(out2.contains("after-drop"), "program continued: {out2:?}");
    }

    // §4.2 — task cancellation (cancel-on-drop) does NOT run defers.
    //
    // Variant 1 — un-held async call, handle dropped via going out of scope.
    //
    // NON-VACUOUS DESIGN: a bare `work()` whose future is dropped at the end of its
    // expression statement is VACUOUS — the spawned task is never polled before the
    // handle drops, so the body never even registers the defer (the absent marker is
    // then trivially absent because the body never ran). To make this prove that
    // cancellation suppresses a REGISTERED defer, we (1) hold the future in a local
    // inside a helper fn, (2) drive the body — via a barrier channel — far enough to
    // PROVABLY register its defer and print a positive control marker, then park on a
    // never-signalled channel, and (3) let the helper fn return so the local handle
    // drops while the body is parked → cancel-on-drop. The positive control
    // ("work-reached-park") proves the body ran up to the park; the absent defer
    // marker proves cancellation skipped the registered defer.
    #[tokio::test]
    async fn task_cancellation_does_not_run_defers() {
        // Variant 1: held-then-dropped — body provably parks before the handle drops.
        let out = run(r#"
import * as sync from "std/sync"

// barrier: work signals it AFTER registering its defer + reaching the park.
// never:   work parks on this forever (nobody sends) so it is cancelled-on-drop.
let barrier = sync.channel(1)
let never   = sync.channel(0)

async fn work(barrier, never) {
    defer print("MUST-NOT-APPEAR-cancel")   // REGISTERED before the park
    print("work-reached-park")               // positive control: body ran this far
    sync.send(barrier, 1)                    // tell the driver we are parked
    let _ = await sync.recv(never)            // park forever
    print("work-body-completed")             // never reached
}

async fn drive(barrier, never) {
    let f = work(barrier, never)             // eagerly scheduled; held in `f`
    let _ = await sync.recv(barrier)          // park driver until work is at its park
    // `f` drops when `drive` returns here → work cancelled while parked.
}

await drive(barrier, never)
print("main")
"#).await;
        assert!(
            out.contains("work-reached-park"),
            "§4.2 non-vacuity: body must have run up to the park (registering the defer); got: {out:?}"
        );
        assert!(
            !out.contains("MUST-NOT-APPEAR-cancel"),
            "§4.2: cancelled task must NOT run its REGISTERED defer; got: {out:?}"
        );
        assert!(
            !out.contains("work-body-completed"),
            "body must not have completed (it was cancelled at the park): {out:?}"
        );
        assert!(out.contains("main"), "main ran: {out:?}");

        // Variant 2: race loser — task is cancelled when the winner resolves.
        let out2 = run(r#"
import * as task from "std/task"
import * as sync from "std/sync"

// `blocker` parks forever on a channel nobody signals, then has a defer.
// `winner` signals ch_sync so we know blocker had a chance to park, then returns.
let ch_park = sync.channel(0)   // capacity 0 → recv parks until someone sends
let ch_sync = sync.channel(1)

async fn blocker(ch_park, ch_sync) {
    defer print("MUST-NOT-APPEAR-race")
    sync.send(ch_sync, 1)           // tell winner we're parked
    let _ = await sync.recv(ch_park)   // park forever (nobody sends)
    print("blocker-body-done")
}

async fn winner(ch_sync) {
    let _ = await sync.recv(ch_sync)  // wait until blocker is parked
    return "won"
}

let result = await task.race([blocker(ch_park, ch_sync), winner(ch_sync)])
print(result)
"#).await;
        assert!(
            !out2.contains("MUST-NOT-APPEAR-race"),
            "§4.2: race loser must NOT run defers; got: {out2:?}"
        );
        assert!(out2.contains("won"), "race winner: {out2:?}");
    }

    // §4.2 — cancellation while a deferred `await` is suspended: the body future is
    // dropped mid-drain → the older defers DO NOT run.
    //
    // Setup:
    //   - `slow` task: body completes immediately (no body awaits), registers two defers:
    //       oldest = `print("older-defer-MUST-NOT-RUN")` (would print if drain finishes)
    //       newest = `defer await never_recv()` (parks on ch_drain, never signaled)
    //     Drain starts LIFO: newest runs first → parks on ch_drain. Slow is now
    //     suspended mid-drain.
    //   - `fast` task: parks on ch_sync until `slow`'s body is done and drain has started,
    //     then returns, winning the race.
    //   - `task.race` cancels `slow` while it is suspended in the deferred await.
    //   - Older defer ("older-defer-MUST-NOT-RUN") must NOT appear.
    #[tokio::test]
    async fn defer_await_cancelled_mid_drain() {
        let out = run(r#"
import * as task from "std/task"
import * as sync from "std/sync"

// ch_drain: slow's defer await parks here; nobody sends → it blocks forever.
// ch_sync:  slow sends here after registering its defers (body done); fast waits
//           for it before returning to win the race.
let ch_drain = sync.channel(0)
let ch_sync  = sync.channel(1)

async fn never_recv(ch) {
    let _ = await sync.recv(ch)   // parks; nobody sends on ch_drain
}

async fn slow(ch_drain, ch_sync) {
    defer print("older-defer-MUST-NOT-RUN")   // oldest: would run after defer-await
    defer await never_recv(ch_drain)           // newest: runs first, parks forever
    // body: signal fast that our body is done and drain is about to start
    sync.send(ch_sync, 1)
}

async fn fast(ch_sync) {
    let _ = await sync.recv(ch_sync)  // wait for slow's body to complete
    return "fast-won"
}

let result = await task.race([slow(ch_drain, ch_sync), fast(ch_sync)])
print(result)
"#).await;
        assert!(
            !out.contains("older-defer-MUST-NOT-RUN"),
            "§4.2: older defer must NOT run when mid-drain cancel occurs; got: {out:?}"
        );
        assert!(out.contains("fast-won"), "fast must win: {out:?}");
    }

    // §4.2 — task.timeout cancellation route: a defer in a timed-out async fn does
    // NOT run. The third cancel route §4.2 names (race-loser + un-held are above).
    //
    // NON-FLAKY DESIGN: the body parks on a channel nobody ever signals, so the
    // timeout ALWAYS fires regardless of the deadline (no race against real work).
    // The positive control ("work-reached-park") proves the body registered its
    // defer and reached the park before the timeout cancelled it; the absent defer
    // marker proves cancellation skipped the registered defer.
    #[tokio::test]
    async fn defer_not_run_on_timeout_cancellation() {
        let out = run(r#"
import * as task from "std/task"
import * as sync from "std/sync"

// never: the body parks here forever; nobody sends, so timeout always fires.
let never = sync.channel(0)

async fn work(never) {
    defer print("MUST-NOT-APPEAR-timeout")   // REGISTERED before the park
    print("work-reached-park")                // positive control: body ran this far
    let _ = await sync.recv(never)             // park forever → timeout cancels here
    print("work-body-completed")              // never reached
}

let r = await task.timeout(20, work(never))
print(r[0])           // nil (timed out)
print(r[1].message)   // the timeout error
"#).await;
        assert!(
            out.contains("work-reached-park"),
            "§4.2 non-vacuity: body must have run up to the park (registering the defer); got: {out:?}"
        );
        assert!(
            !out.contains("MUST-NOT-APPEAR-timeout"),
            "§4.2: timed-out task must NOT run its REGISTERED defer; got: {out:?}"
        );
        assert!(
            !out.contains("work-body-completed"),
            "body must not have completed (cancelled by timeout at the park): {out:?}"
        );
        assert!(out.contains("timed out"), "timeout error surfaced: {out:?}");
    }

    // §4.3 — `async fn*` generator: completion (return after all yields) runs defers.
    // §4.3 covers BOTH `fn*` and `async fn*`; this exercises the async variant whose
    // body BOTH awaits internally AND yields.
    #[tokio::test]
    async fn async_generator_completion_runs_defers() {
        let out = run(r#"
async fn pick(n) { return n }

async fn* gen() {
    defer print("async-gen-defer")
    let a = await pick(1)
    yield a
    let b = await pick(2)
    yield b
    // body completes here — defer must run before the final resume reports done
}

let it = gen()
print(await it.next())
print(await it.next())
let done = await it.next()   // drives body to completion; defer runs here
print("after-done")
"#).await;
        assert!(
            out.contains("async-gen-defer"),
            "§4.3: async fn* completion must run defers; got: {out:?}"
        );
        let defer_pos = out.find("async-gen-defer").unwrap();
        let after_done_pos = out.find("after-done").unwrap();
        assert!(
            defer_pos < after_done_pos,
            "async-gen-defer must appear before after-done: {out:?}"
        );
        assert!(out.contains("1\n2\n"), "yields must work: {out:?}");
    }

    // §4.3 — `async fn*` generator: close()/last-drop mid-suspend does NOT run defers.
    #[tokio::test]
    async fn async_generator_close_does_not_run_defers() {
        // Variant 1: explicit close() while parked at a yield.
        let out = run(r#"
async fn pick(n) { return n }

async fn* gen() {
    defer print("MUST-NOT-APPEAR-async-close")
    let a = await pick(1)
    print("async-gen-reached-yield")   // positive control: body reached the yield
    yield a
    yield 2
}

let it = gen()
print(await it.next())   // drives body to first yield (body now parked)
it.close()               // drops body mid-suspend → defer must NOT run
print("after-close")
"#).await;
        assert!(
            out.contains("async-gen-reached-yield"),
            "§4.3 non-vacuity: body must have reached the yield (registering the defer); got: {out:?}"
        );
        assert!(
            !out.contains("MUST-NOT-APPEAR-async-close"),
            "§4.3: async fn* close() must NOT run defers; got: {out:?}"
        );
        assert!(out.contains("after-close"), "program continued: {out:?}");

        // Variant 2: last-handle drop (handle goes out of scope mid-suspend).
        let out2 = run(r#"
async fn pick(n) { return n }

async fn* droppable() {
    defer print("MUST-NOT-APPEAR-async-drop")
    let a = await pick(1)
    print("async-gen-drop-reached-yield")   // positive control
    yield a
    yield 2
}

async fn abandon() {
    let it = droppable()
    print(await it.next())   // drives body to first yield; body now parked
    // `it` goes out of scope at end of `abandon` → body dropped mid-suspend
}

await abandon()
print("after-drop")
"#).await;
        assert!(
            out2.contains("async-gen-drop-reached-yield"),
            "§4.3 non-vacuity: body must have reached the yield (registering the defer); got: {out2:?}"
        );
        assert!(
            !out2.contains("MUST-NOT-APPEAR-async-drop"),
            "§4.3: async fn* last-handle drop must NOT run defers; got: {out2:?}"
        );
        assert!(out2.contains("after-drop"), "program continued: {out2:?}");
    }

    // ── ELIDE §4.4: check_call_args / check_call_args_in_place elide_contracts mode ──

    /// Helper: build a typed positional Param with no default and no rest.
    fn typed_param(name: &str, ty: crate::ast::Type) -> crate::ast::Param {
        crate::ast::Param {
            name: name.to_string(),
            ty: Some(ty),
            name_span: Span::new(0, 0),
            rest: false,
            default: None,
        }
    }

    /// ELIDE §4.4 (a): a wrong-typed arg passes through UNCHECKED when
    /// elide_contracts=true; the result contains the original value unchanged.
    #[test]
    fn elide_contracts_skips_type_check() {
        use crate::ast::Type;
        // param `x: int` but we pass a string — normally a Tier-2 panic.
        let params = vec![typed_param("x", Type::Int)];
        let args = vec![Value::str("wrong")];
        // With elide=false this should fail.
        let err = check_call_args(&params, args.clone(), Span::new(0, 0), "f", None, None, false);
        assert!(err.is_err(), "elide=false must reject a wrong-typed arg");
        // With elide=true the same call must succeed and return the value unchanged.
        let ok = check_call_args(&params, args, Span::new(0, 0), "f", None, None, true)
            .expect("elide=true must pass a wrong-typed arg through unchecked");
        assert_eq!(ok.supplied, 1);
        assert!(
            matches!(ok.values[0].kind(), crate::value::ValueKind::Str(_)),
            "value must be the original string, not coerced"
        );
    }

    /// ELIDE §4.4 (b): arity errors STILL fire even when elide_contracts=true.
    #[test]
    fn elide_contracts_preserves_arity_check() {
        use crate::ast::Type;
        // fn f(x: int) — called with zero args.
        let params = vec![typed_param("x", Type::Int)];
        let err_false = check_call_args(&params, vec![], Span::new(0, 0), "f", None, None, false);
        let err_true  = check_call_args(&params, vec![], Span::new(0, 0), "f", None, None, true);
        assert!(err_false.is_err(), "elide=false: arity must be enforced");
        assert!(err_true.is_err(),  "elide=true: arity must STILL be enforced");
        // Both messages must be byte-identical.
        let msg_false = format!("{:?}", err_false.err().unwrap());
        let msg_true  = format!("{:?}", err_true.err().unwrap());
        assert_eq!(msg_false, msg_true, "arity panic message must be identical regardless of elide_contracts");
    }

    /// ELIDE §4.4 (c): default placeholders are still produced correctly when
    /// elide_contracts=true — the engine-side default evaluation depends on them.
    #[test]
    fn elide_contracts_keeps_defaults() {
        use crate::ast::Type;
        // fn f(a: int, b: int = <default>) — call with 1 arg.
        // Build b with a sentinel default so check_call_args treats it as optional.
        let sentinel_default = crate::ast::Expr {
            kind: crate::ast::ExprKind::Nil,
            span: Span::new(0, 0),
        };
        let params = vec![
            typed_param("a", Type::Int),
            crate::ast::Param {
                name: "b".to_string(),
                ty: Some(Type::Int),
                name_span: Span::new(0, 0),
                rest: false,
                default: Some(sentinel_default),
            },
        ];
        let args = vec![Value::int(42)];
        let bound = check_call_args(&params, args, Span::new(0, 0), "f", None, None, true)
            .expect("elide=true with one arg for a two-param (one defaulted) fn must succeed");
        assert_eq!(bound.supplied, 1, "supplied must be 1");
        assert_eq!(bound.values.len(), 2, "values must have placeholders for all params");
        assert!(
            matches!(bound.values[0].kind(), crate::value::ValueKind::Int(42)),
            "supplied arg must be preserved"
        );
        assert!(
            matches!(bound.values[1].kind(), crate::value::ValueKind::Nil),
            "defaulted placeholder must be Nil"
        );
        assert!(bound.defaults.contains(&1), "defaults range must cover the omitted slot");
    }

    /// ELIDE §4.4 (d): rest collection still works (value collected, no type check).
    #[test]
    fn elide_contracts_keeps_rest_collection() {
        use crate::ast::Type;
        // fn f(a: int, ...rest: array<int>) — call with wrong rest elements.
        let params = vec![
            typed_param("a", Type::Int),
            crate::ast::Param {
                name: "rest".to_string(),
                ty: Some(Type::Array(Box::new(Type::Int))),
                name_span: Span::new(0, 0),
                rest: true,
                default: None,
            },
        ];
        // With elide=false: passing a string as a rest element must fail.
        let args_bad = vec![Value::int(1), Value::str("oops")];
        let err = check_call_args(&params, args_bad.clone(), Span::new(0, 0), "f", None, None, false);
        assert!(err.is_err(), "elide=false must reject a wrong-typed rest element");
        // With elide=true: same call must succeed and collect the rest array.
        let ok = check_call_args(&params, args_bad, Span::new(0, 0), "f", None, None, true)
            .expect("elide=true must collect rest elements unchecked");
        assert_eq!(ok.supplied, 1, "supplied positional = 1");
        // The rest array is the last value.
        let rest_v = &ok.values[ok.values.len() - 1];
        let arr = match rest_v.kind() {
            crate::value::ValueKind::Array(a) => a.borrow().clone(),
            other => panic!("expected Array, got {other:?}"),
        };
        assert_eq!(arr.len(), 1, "rest array must contain 1 element");
        assert!(
            matches!(arr[0].kind(), crate::value::ValueKind::Str(_)),
            "rest element is the original string value"
        );
    }

    /// ELIDE §4.4: check_call_args_in_place with elide_contracts=true skips type
    /// checks but preserves arity enforcement.
    #[test]
    fn elide_contracts_in_place_skips_type_check_preserves_arity() {
        use crate::ast::Type;
        let params = vec![typed_param("x", Type::Int)];
        // Wrong type — must be rejected with elide=false.
        let stack = vec![Value::str("bad")];
        let err = check_call_args_in_place(&params, &stack, Span::new(0, 0), "f", None, None, false);
        assert!(err.is_err(), "in_place elide=false must reject wrong type");
        // Same args, elide=true — must pass.
        let supplied = check_call_args_in_place(&params, &stack, Span::new(0, 0), "f", None, None, true)
            .expect("in_place elide=true must pass wrong-typed arg");
        assert_eq!(supplied, 1);
        // Arity: 0 args for 1 required param must still fail with elide=true.
        let empty: &[Value] = &[];
        let arity_err = check_call_args_in_place(&params, empty, Span::new(0, 0), "f", None, None, true);
        assert!(arity_err.is_err(), "in_place elide=true must still enforce arity");
        // Arity messages match between elide=true and elide=false.
        let msg_false = format!("{:?}", check_call_args_in_place(&params, empty, Span::new(0, 0), "f", None, None, false).err().unwrap());
        let msg_true  = format!("{:?}", arity_err.err().unwrap());
        assert_eq!(msg_false, msg_true, "arity message must be identical regardless of elide_contracts");
    }

    /// CNTR Phase-0 pin — `native_stream_method` registry baseline.
    ///
    /// `std/docker` will add log and event stream handles (`DockerLogStream`,
    /// `DockerEventStream`) to the `native_stream_method` registry in Phase 1.
    /// Pinning the CURRENT registry state before that addition proves the Phase-1
    /// commit is PURELY ADDITIVE (it must not alter the verdicts for existing
    /// `NativeKind` variants).
    ///
    /// Current state (feature = "net"):
    ///   - `SseStream`     → `Some("next")`  (SSE server-sent events)
    ///   - `WsConnection`  → `Some("recv")`  (WebSocket)
    ///
    /// Gated on `feature = "net"` — the same gate that guards the match arms in
    /// `native_stream_method` (`#[cfg(feature = "net")]`).  Under
    /// `--no-default-features` the function returns `None` for all kinds and
    /// there is nothing to pin beyond the None-fallthrough behaviour.
    #[cfg(feature = "net")]
    #[test]
    fn cntr_native_stream_method_registry_baseline() {
        use crate::value::NativeKind;
        // SseStream → "next" (SSE pull-stream protocol).
        assert_eq!(
            native_stream_method(NativeKind::SseStream),
            Some("next"),
            "native_stream_method(SseStream) must be Some(\"next\") before CNTR \
             extends the registry; a change here means the pre-CNTR baseline moved"
        );
        // WsConnection → "recv" (WebSocket receive loop).
        assert_eq!(
            native_stream_method(NativeKind::WsConnection),
            Some("recv"),
            "native_stream_method(WsConnection) must be Some(\"recv\") before CNTR \
             extends the registry; a change here means the pre-CNTR baseline moved"
        );
        // A NativeKind that is deliberately NOT a stream must return None
        // (proves the registry is a positive allowlist, not a catch-all).
        assert_eq!(
            native_stream_method(NativeKind::TcpStream),
            None,
            "native_stream_method(TcpStream) must be None — TcpStream is a transport \
             handle, not a stream-protocol handle with a named pull method"
        );
    }
}
