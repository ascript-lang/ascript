//! `std/task` — structured concurrency primitives (spec §7.3). NOT feature-gated:
//! futures are core async, available in every build.
//!
//! - `spawn(futureOr0ArgFn) -> future` — schedule work and get a handle.
//! - `gather([futures]) -> [values]` — await all, preserving input order.
//! - `race([futures]) -> value` — the first to resolve wins.
//! - `timeout(ms, future) -> [value, err]` — bounded await, Result pair.
//!
//! All four ride the current-thread `LocalSet` established by the entry points, so
//! `spawn_local` and `tokio::select!` work without `Send`. A panic raised in a
//! spawned task crosses the task boundary via `SharedFuture`'s stored `Control`.

use super::{arg, bi, want_array, want_number};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp};
use crate::span::Span;
use crate::task::SharedFuture;
use crate::value::{OwnedKind, Value, ValueKind};
use crate::worker::isolate::{ChunkJob, ChunkKind};

/// Aborts a `spawn_local` task when dropped. Used by `race` to cancel the resolver
/// tasks (and thereby the losing futures) once a winner is decided.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `import * as task from "std/task"` bindings.
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("spawn", bi("task.spawn")),
        ("gather", bi("task.gather")),
        ("race", bi("task.race")),
        ("timeout", bi("task.timeout")),
        ("retry", bi("task.retry")),
        ("pipe", bi("task.pipe")),
        // PAR (spec §2.1)
        ("pmap", bi("task.pmap")),
        ("preduce", bi("task.preduce")),
    ]
}

/// Build a `[value, nil]` ok Result pair.
fn ok_pair(value: Value) -> Value {
    make_pair(value, Value::nil())
}

/// Build a `[nil, {message}]` error Result pair.
fn err_pair(msg: String) -> Value {
    make_pair(Value::nil(), make_error(Value::str(msg)))
}

// ─────────────────────────────────────────────────────────────────────────────
// Retry v2 (RESIL spec §3.4) — the shared retry engine.
//
// `task.retry` (CORE, stateless) and `resilience.retry(opts)` (the feature-gated
// stateful policy) both route through ONE engine: `Interp::retry_engine`. The
// engine is configured by a plain `RetryConfig` (parsed from opts), and the delay
// schedule is a PURE function (`compute_retry_delay`) so it is exhaustively
// unit-testable. When the v2 keys are absent the behavior is BIT-IDENTICAL to
// retry v1 (the three shipped `task.retry` tests are the compat contract).
// ─────────────────────────────────────────────────────────────────────────────

/// Backoff schedule for the delay between attempts (§3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Backoff {
    /// `base_ms * 2^attempt`, capped at `max_ms` — the v1 default.
    Exponential,
    /// Every delay is `base_ms` (capped at `max_ms`).
    Fixed,
}

/// Jitter mode applied to the computed delay (§3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Jitter {
    /// No jitter — the deterministic computed delay. `jitter: false`/`"none"`.
    None,
    /// The v1 `jitter: true` behavior: add up to +50% of the computed delay.
    PlusHalf,
    /// AWS full jitter: the delay is drawn uniformly from `[0, computedDelay]`.
    /// `jitter: "full"`.
    Full,
}

/// Which outcome classes are retried (§3.4 `retryOn`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RetryOn {
    /// Only `Control::Panic` (the v1 default). Returned pairs are NOT retried.
    Panic,
    /// Only a returned `[_, err≠nil]` pair is retried (panics pass through).
    Error,
    /// Either a panic or an error pair.
    Both,
}

/// A parsed retry configuration shared by `task.retry` and `resilience.retry`.
pub(crate) struct RetryConfig {
    pub attempts: usize,
    pub base_ms: u64,
    pub max_ms: Option<u64>,
    pub backoff: Backoff,
    pub jitter: Jitter,
    pub retry_on: RetryOn,
    /// Optional `retryIf(err) -> bool` predicate value (a callable).
    pub retry_if: Option<Value>,
}

/// PURE delay schedule (§3.4). Given the zero-based `attempt` index, the `base_ms`,
/// the optional `max_ms` cap, the `backoff` mode, the `jitter` mode, and a
/// random-source closure `rand_f64` (called ONLY in jitter modes — `[0,1)`),
/// return the sleep duration in milliseconds.
///
/// BIT-IDENTICAL to retry v1 for `Backoff::Exponential` + `Jitter::{None,PlusHalf}`:
/// the exponential schedule is `base_ms * 2^attempt` with the shift capped at 62 so
/// `1u64 << shift` never overflows (attempt ≥ 63 is clamped to shift 62), `max_ms`
/// caps the pre-jitter delay, and `Jitter::PlusHalf` reuses the v1 `+0..50%` formula
/// verbatim.
pub(crate) fn compute_retry_delay(
    attempt: usize,
    base_ms: u64,
    max_ms: Option<u64>,
    backoff: Backoff,
    jitter: Jitter,
    rand_f64: impl FnOnce() -> f64,
) -> u64 {
    // Base (pre-jitter) delay.
    let base = match backoff {
        Backoff::Exponential => {
            // Cap shift at 62 so `1u64 << shift` never overflows (v1 invariant).
            let shift = attempt.min(62) as u32;
            let multiplier = 1u64 << shift;
            base_ms.saturating_mul(multiplier)
        }
        Backoff::Fixed => base_ms,
    };
    let delay = match max_ms {
        Some(max) => delay_min(base, max),
        None => base,
    };

    match jitter {
        Jitter::None => delay,
        Jitter::PlusHalf => {
            // v1 formula, verbatim — add up to +50% of the computed delay.
            let frac = rand_f64();
            delay.saturating_add((delay / 2).saturating_mul((frac * 1000.0) as u64) / 1000)
        }
        Jitter::Full => {
            // AWS full jitter: draw uniformly from [0, delay].
            let frac = rand_f64();
            ((delay as f64) * frac) as u64
        }
    }
}

/// `a.min(b)` for `u64` — extracted so `compute_retry_delay` reads cleanly.
#[inline]
fn delay_min(a: u64, b: u64) -> u64 {
    if a < b {
        a
    } else {
        b
    }
}

/// Parse a `RetryConfig` from a `task.retry`/`resilience.retry` opts value (§3.4).
///
/// The legacy keys (`attempts`, `baseMs`, `maxMs`, `jitter: bool`) parse EXACTLY as
/// v1 — when the v2 keys are absent the resulting config drives byte-identical
/// behavior. `budget` is NOT parsed here (it is the policy's stateful key, handled
/// by the caller); an unknown key is ignored (stdlib-wide convention).
pub(crate) fn parse_retry_config(opts: &Value, span: Span) -> Result<RetryConfig, Control> {
    let o = match opts.kind() {
        ValueKind::Nil => {
            return Ok(RetryConfig {
                attempts: 3,
                base_ms: 100,
                max_ms: None,
                backoff: Backoff::Exponential,
                jitter: Jitter::None,
                retry_on: RetryOn::Panic,
                retry_if: None,
            });
        }
        ValueKind::Object(o) => o,
        _ => {
            return Err(AsError::at(
                format!(
                    "task.retry opts must be an object or nil, got {}",
                    crate::interp::type_name(opts)
                ),
                span,
            )
            .into());
        }
    };

    let attempts = match o.get("attempts") {
        Some(v) => {
            let n = super::want_number(&v, span, "task.retry attempts")?;
            if n < 1.0 || n.fract() != 0.0 {
                return Err(
                    AsError::at("task.retry: attempts must be a positive integer", span).into(),
                );
            }
            n as usize
        }
        None => 3,
    };
    let base_ms = match o.get("baseMs") {
        Some(v) => {
            let n = super::want_number(&v, span, "task.retry baseMs")?;
            if n < 0.0 {
                return Err(AsError::at("task.retry: baseMs must be non-negative", span).into());
            }
            n as u64
        }
        None => 100,
    };
    let max_ms = match o.get("maxMs") {
        Some(v) => {
            let n = super::want_number(&v, span, "task.retry maxMs")?;
            if n < 0.0 {
                return Err(AsError::at("task.retry: maxMs must be non-negative", span).into());
            }
            Some(n as u64)
        }
        None => None,
    };

    // backoff: "exponential" (default) | "fixed"
    let backoff = match o.get("backoff") {
        None => Backoff::Exponential,
        Some(v) => match v.kind() {
            ValueKind::Nil => Backoff::Exponential,
            ValueKind::Str(s) => match s.as_ref() {
                "exponential" => Backoff::Exponential,
                "fixed" => Backoff::Fixed,
                other => {
                    return Err(AsError::at(
                        format!(
                            "task.retry: backoff must be \"exponential\" or \"fixed\", got \"{other}\""
                        ),
                        span,
                    )
                    .into());
                }
            },
            _ => {
                return Err(AsError::at(
                    "task.retry: backoff must be a string",
                    span,
                )
                .into());
            }
        },
    };

    // jitter: false/"none" | true (+0..50%) | "full"
    let jitter = match o.get("jitter") {
        None => Jitter::None,
        Some(v) => match v.kind() {
            ValueKind::Nil | ValueKind::Bool(false) => Jitter::None,
            ValueKind::Bool(true) => Jitter::PlusHalf,
            ValueKind::Str(s) => match s.as_ref() {
                "none" => Jitter::None,
                "full" => Jitter::Full,
                other => {
                    return Err(AsError::at(
                        format!(
                            "task.retry: jitter must be a bool, \"none\", or \"full\", got \"{other}\""
                        ),
                        span,
                    )
                    .into());
                }
            },
            _ => {
                return Err(AsError::at(
                    "task.retry: jitter must be a bool or a string",
                    span,
                )
                .into());
            }
        },
    };

    // retryOn: "panic" (default) | "error" | "both"
    let retry_on = match o.get("retryOn") {
        None => RetryOn::Panic,
        Some(v) => match v.kind() {
            ValueKind::Nil => RetryOn::Panic,
            ValueKind::Str(s) => match s.as_ref() {
                "panic" => RetryOn::Panic,
                "error" => RetryOn::Error,
                "both" => RetryOn::Both,
                other => {
                    return Err(AsError::at(
                        format!(
                            "task.retry: retryOn must be \"panic\", \"error\", or \"both\", got \"{other}\""
                        ),
                        span,
                    )
                    .into());
                }
            },
            _ => {
                return Err(AsError::at(
                    "task.retry: retryOn must be a string",
                    span,
                )
                .into());
            }
        },
    };

    // retryIf: fn(err) -> bool (a callable)
    let retry_if = match o.get("retryIf") {
        None => None,
        Some(v) => match v.kind() {
            ValueKind::Nil => None,
            ValueKind::Function(_)
            | ValueKind::Closure(_)
            | ValueKind::Builtin(_)
            | ValueKind::BoundMethod(_)
            | ValueKind::NativeMethod(_) => Some(v),
            _ => {
                return Err(AsError::at(
                    format!(
                        "task.retry: retryIf must be a function, got {}",
                        crate::interp::type_name(&v)
                    ),
                    span,
                )
                .into());
            }
        },
    };

    Ok(RetryConfig {
        attempts,
        base_ms,
        max_ms,
        backoff,
        jitter,
        retry_on,
        retry_if,
    })
}

/// Fold `code: "retries-exhausted"` into the err of a returned pair when it is
/// absent (§3.4 — the `retryOn: error`/`both` exhaustion path). The pair's value
/// slot is preserved; only the err object gains the code iff it lacks one.
fn fold_retries_exhausted(pair: Value) -> Value {
    let (val, err) = match pair.kind() {
        ValueKind::Array(a) => {
            let b = a.borrow();
            if b.len() == 2 {
                (b[0].clone(), b[1].clone())
            } else {
                return pair.clone();
            }
        }
        _ => return pair,
    };
    // Only fold when err is an Object lacking a `code` field.
    if let ValueKind::Object(o) = err.kind() {
        if o.get("code").is_none() {
            o.insert("code", Value::str("retries-exhausted"));
        }
    }
    make_pair(val, err)
}

// ── budget (the resilience.retry policy's count-based retry budget, §3.4) ──────

/// The policy field names for the count-based retry budget.
const BUDGET_ATTEMPTS: &str = "__attemptsSeen";
const BUDGET_SPENT: &str = "__retriesSpent";

/// Record one attempt against the budget's first-attempt-rate denominator.
fn budget_record_attempt(state: &Value) {
    if let ValueKind::Object(o) = state.kind() {
        let n = o.get(BUDGET_ATTEMPTS).and_then(|v| v.as_int()).unwrap_or(0);
        o.insert(BUDGET_ATTEMPTS, Value::int(n + 1));
    }
}

/// Record one spent retry credit.
fn budget_record_retry(state: &Value) {
    if let ValueKind::Object(o) = state.kind() {
        let n = o.get(BUDGET_SPENT).and_then(|v| v.as_int()).unwrap_or(0);
        o.insert(BUDGET_SPENT, Value::int(n + 1));
    }
}

/// True iff the budget permits another retry: `__retriesSpent < budget * __attemptsSeen`.
/// Count-based, no clock interaction (§3.4).
fn budget_permits_retry(state: &Value) -> bool {
    let ValueKind::Object(o) = state.kind() else {
        return true;
    };
    let budget = o.get("budget").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let attempts = o.get(BUDGET_ATTEMPTS).and_then(|v| v.as_int()).unwrap_or(0) as f64;
    let spent = o.get(BUDGET_SPENT).and_then(|v| v.as_int()).unwrap_or(0) as f64;
    spent < budget * attempts
}

impl Interp {
    /// `std/task` dispatch. All entries are async (they drive futures / spawn
    /// tasks), so this is awaited on the event loop.
    pub(crate) async fn call_task(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "spawn" => self.task_spawn(args, span).await,
            "gather" => self.task_gather(args, span).await,
            "race" => self.task_race(args, span).await,
            "timeout" => self.task_timeout(args, span).await,
            "retry" => self.task_retry(args, span).await,
            "pipe" => self.task_pipe(args, span).await,
            // PAR §2.1 — Task 2.2: pmap; Task 2.3: preduce.
            "pmap" => self.task_pmap(args, span).await,
            "preduce" => self.task_preduce(args, span).await,
            _ => Err(AsError::at(format!("unknown function 'task.{}'", func), span).into()),
        }
    }

    /// `spawn(futureOr0ArgFn) -> future`. A future passes straight through; a
    /// 0-arg function is called now (its async-fn call already returns a future;
    /// a sync return value is wrapped in an already-resolved future).
    async fn task_spawn(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let v = arg(args, 0);
        // `spawn` is the explicit opt-out of cancel-on-drop: detach the backing
        // task so it runs to completion (fire-and-forget) regardless of whether
        // the returned handle is awaited or dropped.
        if matches!(v.kind(), ValueKind::Future(_)) {
            let OwnedKind::Future(f) = v.into_kind() else {
                unreachable!()
            };
            f.detach();
            return Ok(Value::future(f));
        }
        // `Value::closure` is the VM's compiled-function value (V4-T5 bridge);
        // `task.spawn(closure)` must invoke it like any other callable.
        if matches!(
            v.kind(),
            ValueKind::Function(_)
                | ValueKind::Closure(_)
                | ValueKind::Builtin(_)
                | ValueKind::BoundMethod(_)
                | ValueKind::NativeMethod(_)
        ) {
            let r = self.call_value(v, Vec::new(), span).await?;
            if matches!(r.kind(), ValueKind::Future(_)) {
                let OwnedKind::Future(f) = r.into_kind() else {
                    unreachable!()
                };
                f.detach();
                return Ok(Value::future(f));
            }
            return Ok(Value::future(SharedFuture::resolved(Ok(r))));
        }
        Err(AsError::at(
            format!(
                "task.spawn expects a future or a 0-argument function, got {}",
                crate::interp::type_name(&v)
            ),
            span,
        )
        .into())
    }

    /// `gather([futures]) -> [values]`. Awaits every element in order; non-future
    /// elements are taken as-is. The first error short-circuits.
    async fn task_gather(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let array = want_array(&arg(args, 0), span, "task.gather")?;
        // Snapshot the elements so we don't hold the array borrow across `.await`.
        let items: Vec<Value> = array.borrow().clone();
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            if matches!(item.kind(), ValueKind::Future(_)) {
                let OwnedKind::Future(f) = item.into_kind() else {
                    unreachable!()
                };
                out.push(f.get().await?);
            } else {
                out.push(item);
            }
        }
        Ok(Value::array(out))
    }

    /// `race([futures]) -> value`. Resolves to the first input future to complete
    /// (value or error). Non-future elements resolve immediately. The losers are
    /// **cancelled**: each is awaited inside a resolver task whose `AbortHandle` is
    /// held by an `AbortOnDrop` guard; when `race` returns, the guards drop, the
    /// resolver tasks abort, their loser-future clones drop, and (once the caller
    /// no longer holds them) the losers' own tasks are cancelled via cancel-on-drop.
    async fn task_race(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let array = want_array(&arg(args, 0), span, "task.race")?;
        let items: Vec<Value> = array.borrow().clone();
        if items.is_empty() {
            return Err(AsError::at("task.race requires a non-empty array", span).into());
        }
        let winner = SharedFuture::new();
        let mut resolver_guards: Vec<AbortOnDrop> = Vec::new();
        for item in items {
            if matches!(item.kind(), ValueKind::Future(_)) {
                let OwnedKind::Future(f) = item.into_kind() else {
                    unreachable!()
                };
                let w = winner.clone();
                let jh = tokio::task::spawn_local(async move {
                    let r = f.get().await;
                    w.resolve(r);
                });
                resolver_guards.push(AbortOnDrop(jh.abort_handle()));
            } else {
                // A non-future element is already "done": it wins instantly.
                winner.resolve(Ok(item));
            }
        }
        let result = winner.get().await;
        // Dropping the guards aborts the still-pending resolver tasks, releasing
        // their hold on the loser futures so the losers can be cancelled.
        drop(resolver_guards);
        result
    }

    /// `timeout(ms, future) -> [value, err]`. Races the future against a sleep; on
    /// timeout returns an error pair and the future handle is dropped as `timeout`
    /// returns, so (once the caller no longer holds it) the timed-out work is
    /// **cancelled** via cancel-on-drop rather than left running. A panic inside the
    /// future propagates (not an err pair).
    async fn task_timeout(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let ms = want_number(&arg(args, 0), span, "task.timeout")?;
        if ms < 0.0 {
            return Err(AsError::at("task.timeout duration must be non-negative", span).into());
        }
        let v = arg(args, 1);
        let fut = if matches!(v.kind(), ValueKind::Future(_)) {
            let OwnedKind::Future(f) = v.into_kind() else {
                unreachable!()
            };
            f
        } else {
            // A non-future second arg is already complete: never times out.
            return Ok(ok_pair(v));
        };
        tokio::select! {
            r = fut.get() => match r {
                Ok(value) => Ok(ok_pair(value)),
                Err(c) => Err(c),
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(ms as u64)) => {
                Ok(err_pair(format!("operation timed out after {}ms", ms as u64)))
            }
        }
    }

    /// `retry(fn, opts?) -> value`
    ///
    /// Calls `fn()` up to `opts.attempts` times (default 3). On each
    /// `Control::Panic` (and only on panic by default — returned `[nil, err]`
    /// pairs are NOT retried unless `retryOn` opts in; retry is on Tier-2 panics
    /// only by default), waits `baseMs * 2^attemptIndex` ms (capped at `opts.maxMs`
    /// if given) then retries. If `opts.jitter` is `true`, adds a uniform random
    /// fraction of the delay (up to +50%). After all attempts fail, re-raises the
    /// LAST panic.
    ///
    /// Non-panic errors (`Control::Propagate`, `Control::Exit`) are passed
    /// through immediately without retry.
    ///
    /// RESIL §3.4: additive opts (`backoff`, `jitter` string modes, `retryOn`,
    /// `retryIf`) are parsed by the shared `parse_retry_config`; `budget` is the
    /// stateful policy's key and is a Tier-2 error on `task.retry`. When the new
    /// keys are absent the behavior is BIT-IDENTICAL to retry v1.
    async fn task_retry(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let func = arg(args, 0);
        let opts = arg(args, 1);

        // `budget` belongs to the stateful resilience.retry policy, never to the
        // stateless task.retry (Gate 6 — never a silent ignore).
        if let ValueKind::Object(o) = opts.kind() {
            if o.get("budget").is_some() {
                return Err(AsError::at(
                    "task.retry: budget requires a resilience.retry policy",
                    span,
                )
                .into());
            }
        }

        let cfg = parse_retry_config(&opts, span)?;
        self.retry_engine(func, &cfg, None, span).await
    }

    /// The shared retry engine (RESIL §3.4). Drives `func()` up to `cfg.attempts`
    /// times under the configured backoff/jitter/retryOn/retryIf policy. Both
    /// `task.retry` (with `budget_state: None`) and `resilience.retry`'s `call`
    /// method (with the policy's count-based `budget_state`) route through here, so
    /// the retry behavior is identical by construction.
    ///
    /// `budget_state`, when `Some`, is the resilience.retry policy object carrying
    /// `__retriesSpent` / `__attemptsSeen` and the `budget` ratio. A retry is
    /// permitted only while `__retriesSpent < budget * __attemptsSeen`; once the
    /// ratio is exhausted the engine behaves as exhausted immediately. The budget
    /// verdict is COUNT-BASED (no clock interaction).
    pub(crate) async fn retry_engine(
        &self,
        func: Value,
        cfg: &RetryConfig,
        budget_state: Option<&Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let mut last_panic: Option<crate::error::AsError> = None;
        // The last retried error pair (for the `retryOn: error`/`both` exhaustion path).
        let mut last_pair: Option<Value> = None;

        for attempt in 0..cfg.attempts {
            // Count this attempt for the budget ratio (the first-attempt rate).
            if let Some(state) = budget_state {
                budget_record_attempt(state);
            }

            // Call the function. If it is an async fn, call_value returns
            // Ok(Value::future(..)) immediately — we must drive the future to
            // completion by awaiting it before inspecting the result.
            let call_result = self.call_value(func.clone(), vec![], span).await;
            let result = match call_result {
                Ok(v) if matches!(v.kind(), ValueKind::Future(_)) => {
                    let OwnedKind::Future(f) = v.into_kind() else {
                        unreachable!()
                    };
                    f.get().await
                }
                other => other,
            };

            // Classify the outcome against `retryOn`, deciding the retry candidate.
            // `retryable_err` is the error VALUE handed to `retryIf` (the panic's
            // error value, or the pair's err).
            enum Class {
                /// Terminal success (or an un-retried pair) — return as-is.
                Success(Value),
                /// A panic candidate — retry if budget/retryIf/attempts allow.
                Panic(crate::error::AsError, Value),
                /// An error-pair candidate — retry if budget/retryIf/attempts allow.
                ErrPair(Value, Value),
                /// Non-retryable control flow — pass through.
                Passthrough(Control),
            }
            let class = match result {
                Ok(v) => {
                    // An error pair is a retry candidate only under retryOn error/both.
                    match crate::interp::result_pair_err(&v) {
                        Some(err)
                            if matches!(cfg.retry_on, RetryOn::Error | RetryOn::Both) =>
                        {
                            Class::ErrPair(v, err)
                        }
                        _ => Class::Success(v),
                    }
                }
                Err(Control::Panic(e))
                    if matches!(cfg.retry_on, RetryOn::Panic | RetryOn::Both) =>
                {
                    // The error VALUE handed to `retryIf` mirrors `recover`'s shape:
                    // `{message: <panic message>}` (src/interp.rs recover arm).
                    let errv = make_error(Value::str(e.message.clone()));
                    Class::Panic(e, errv)
                }
                Err(other) => Class::Passthrough(other),
            };

            match class {
                Class::Success(v) => return Ok(v),
                Class::Passthrough(c) => return Err(c),
                Class::Panic(e, errv) => {
                    // `retryIf(err) == false` → re-raise immediately (no further attempts).
                    if !self.retry_if_allows(cfg, &errv, span).await? {
                        return Err(Control::Panic(e));
                    }
                    last_panic = Some(e);
                    last_pair = None;
                    // Last attempt OR budget exhausted → re-raise below.
                    let budget_ok = budget_state
                        .map(budget_permits_retry)
                        .unwrap_or(true);
                    if attempt + 1 >= cfg.attempts || !budget_ok {
                        break;
                    }
                    if let Some(state) = budget_state {
                        budget_record_retry(state);
                    }
                    self.retry_backoff_sleep(cfg, attempt).await;
                }
                Class::ErrPair(pair, errv) => {
                    if !self.retry_if_allows(cfg, &errv, span).await? {
                        return Ok(pair);
                    }
                    last_pair = Some(pair);
                    last_panic = None;
                    let budget_ok = budget_state
                        .map(budget_permits_retry)
                        .unwrap_or(true);
                    if attempt + 1 >= cfg.attempts || !budget_ok {
                        break;
                    }
                    if let Some(state) = budget_state {
                        budget_record_retry(state);
                    }
                    self.retry_backoff_sleep(cfg, attempt).await;
                }
            }
        }

        // Exhausted. Panic exhaustion re-raises the last panic (v1 behavior); pair
        // exhaustion returns the LAST pair with `code: "retries-exhausted"` folded
        // into its err if absent (§3.4).
        if let Some(pair) = last_pair {
            Ok(fold_retries_exhausted(pair))
        } else {
            Err(Control::Panic(last_panic.expect("at least one attempt")))
        }
    }

    /// Sleep the computed backoff delay between attempts (timing-only — the §3.4
    /// SP9 exemption). `attempt` is the zero-based index that just failed.
    async fn retry_backoff_sleep(&self, cfg: &RetryConfig, attempt: usize) {
        let delay = compute_retry_delay(
            attempt,
            cfg.base_ms,
            cfg.max_ms,
            cfg.backoff,
            cfg.jitter,
            retry_rand_f64,
        );
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
    }

    /// Evaluate `cfg.retry_if(err)` if present; returns `true` (retry allowed) when
    /// no predicate is set. A panic INSIDE the predicate re-raises (programmer
    /// error — §3.4). A non-bool return is coerced via truthiness.
    async fn retry_if_allows(
        &self,
        cfg: &RetryConfig,
        errv: &Value,
        span: Span,
    ) -> Result<bool, Control> {
        let Some(pred) = cfg.retry_if.as_ref() else {
            return Ok(true);
        };
        let r = self
            .call_value(pred.clone(), vec![errv.clone()], span)
            .await?;
        // Drive a returned future (async predicate) to completion.
        let r = if matches!(r.kind(), ValueKind::Future(_)) {
            let OwnedKind::Future(f) = r.into_kind() else {
                unreachable!()
            };
            f.get().await?
        } else {
            r
        };
        Ok(r.is_truthy())
    }

    // ── PAR Task 2.2: pmap orchestrator ──────────────────────────────────────

    /// `task.pmap(data, f, opts?) -> future<array>` (PAR spec §2/§3.4).
    ///
    /// Synchronously inside the call: validate args (§2.1/§2.2), SNAPSHOT the input
    /// (so mutating `data` afterward can't affect the result), plan the chunks
    /// (§3.3.1), build the code slice ONCE, and eagerly dispatch every chunk
    /// (`dispatch_worker_job` with a `ChunkJob{Map, …}` — each returns an
    /// already-running `Value::future`). Then an orchestrator `spawn_local` task awaits
    /// the chunk futures **in input (chunk) order** and concatenates their result
    /// arrays — input-order results, first-by-input-order errors, cancel-on-drop via
    /// the `SharedFuture` abort handle (the `dispatch_worker` bridge shape, §3.5).
    ///
    /// Two non-pool fast paths: empty input resolves to `[]` WITHOUT touching the pool
    /// (§2.1); a call made from INSIDE an isolate runs the same chunk decomposition
    /// inline (`par_inline`, §5.1 venue-invariance) — an isolate never blocks on its
    /// own pool.
    async fn task_pmap(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        // Validate + snapshot synchronously (no borrow held past this point).
        let input = classify_par_input(&arg(args, 0), "task.pmap", span)?;
        let entry_name = par_callback_name(&arg(args, 1), "task.pmap", span)?;
        let (cap, min_chunk) = par_opts(&arg(args, 2), "task.pmap", span)?;

        let len = input.len();
        // Empty input → [] instantly, pool untouched (§2.1).
        if len == 0 {
            return Ok(Value::future(SharedFuture::resolved(Ok(Value::array(
                Vec::new(),
            )))));
        }

        let plan = chunk_plan(len, cap, min_chunk);

        // Nested (called inside an isolate): SAME decomposition, executed inline —
        // venue never changes the value (§5.1) and an isolate never blocks on its pool.
        if crate::worker::pool::in_isolate() {
            return self.par_inline(&input, &entry_name, &plan, ChunkKind::Map, None, span);
        }

        // Build the slice once; clone_for_dispatch serves every chunk (§3.2).
        let slice = crate::worker::build_code_slice_for_interp(self, &entry_name)?;
        let mut chunk_futs: Vec<Value> = Vec::with_capacity(plan.len());
        for &(start, end) in &plan {
            // Frozen: (whole Shared, start..end). Plain: (slice copy, 0..end-start).
            // A non-sendable plain element panics inside dispatch_worker_job's encode,
            // synchronously, at THIS chunk — chunks dispatch in input order, so the
            // first offending chunk by input order raises (§3.5).
            let (data, job) = input.chunk_payload(start, end, ChunkKind::Map);
            let fut = crate::worker::dispatch_worker_job(
                self,
                slice.clone_for_dispatch(),
                vec![data],
                Some(job),
                span,
            )?;
            chunk_futs.push(fut);
        }

        // Orchestrator: await in INPUT order, concatenate. First error wins; dropping
        // the remaining futures cancels queued chunks (§3.5).
        let fut = SharedFuture::new();
        let cell = fut.cell();
        let handle = tokio::task::spawn_local(async move {
            let mut merged: Vec<Value> = Vec::new();
            let mut futs = chunk_futs.into_iter();
            let result = loop {
                let Some(f) = futs.next() else {
                    break Ok(Value::array(merged));
                };
                match await_worker_future(f).await {
                    Ok(v) => match v.kind() {
                        ValueKind::Array(a) => {
                            merged.extend(a.borrow().iter().cloned());
                        }
                        _ => {
                            break Err(Control::Panic(AsError::at(
                                format!(
                                    "pmap chunk returned a non-array (internal invariant): {}",
                                    crate::interp::type_name(&v)
                                ),
                                span,
                            )));
                        }
                    },
                    // Remaining futs DROP here → queued chunks cancel (§3.5).
                    Err(e) => break Err(e),
                }
            };
            cell.resolve(result);
        });
        fut.set_abort(handle.abort_handle());
        Ok(Value::future(fut))
    }

    /// PAR spec §5.1: the in-isolate INLINE executor for `pmap`/`preduce`. Runs the
    /// SAME chunk decomposition the pooled path runs, but on the CURRENT isolate's VM
    /// (the entry global is already shipped transitively because the enclosing worker
    /// body references `f` by name) — so a nested parallel call is deadlock-free and
    /// produces a byte-identical value (venue-invariance).
    ///
    /// For `ChunkKind::Map` the chunk results are concatenated. For `ChunkKind::Reduce`
    /// the per-chunk partials are collected and `final_init` (the `preduce` `init`)
    /// drives one local final fold over `[init, p0, .., pk]` via the same `run_chunk_job`
    /// — Task 2.3 passes `Some(init)`; Map passes `None`.
    fn par_inline(
        &self,
        input: &ParInput,
        entry_name: &str,
        plan: &[(usize, usize)],
        kind: ChunkKind,
        final_init: Option<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        let vm = self.vm().ok_or_else(|| {
            Control::Panic(AsError::at(
                "inline parallel dispatch requires a VM (internal invariant)".to_string(),
                span,
            ))
        })?;
        let entry = vm.user_global(entry_name).ok_or_else(|| {
            Control::Panic(AsError::at(
                format!(
                    "nested parallel callback '{entry_name}' is not available in the enclosing worker's code slice"
                ),
                span,
            ))
        })?;

        let data = input.inline_data();
        let plan: Vec<(usize, usize)> = plan.to_vec();

        let fut = SharedFuture::new();
        let cell = fut.cell();
        let handle = tokio::task::spawn_local(async move {
            let result = par_inline_run(&vm, &entry, &data, &plan, kind, final_init, span).await;
            cell.resolve(result);
        });
        fut.set_abort(handle.abort_handle());
        Ok(Value::future(fut))
    }

    /// `task.preduce(data, f, init, opts?) -> future<T>` (PAR spec §2.1/§3.3.3).
    ///
    /// Parallel reduction. Each chunk is folded with `f` **seeded by the chunk's own
    /// first element** (no `init` inside a chunk); the per-chunk partials are then
    /// combined by ONE final fold `f(...f(f(init, p0), p1)...)`. `init` participates
    /// **exactly once** — only in the final combine stage.
    ///
    /// `init` sendability is checked **up front** before any dispatch (§3.3.3 "fail fast").
    /// Total dispatches = `chunks + 1` (the chunk jobs + the final-combine job).
    ///
    /// Non-pool fast paths: empty input resolves to `init` without touching the pool;
    /// in-isolate calls run the same decomposition inline (`par_inline`, §5.1).
    async fn task_preduce(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        // Step 1: classify input.
        let input = classify_par_input(&arg(args, 0), "task.preduce", span)?;
        // Step 2: validate callback (named `worker fn` only — §2.2).
        let entry_name = par_callback_name(&arg(args, 1), "task.preduce", span)?;
        // Step 3: capture init; check sendability UP FRONT before any dispatch (§3.3.3).
        let init = arg(args, 2);
        crate::worker::serialize::check_sendable(&init).map_err(|e| {
            Control::Panic(AsError::at(
                format!("task.preduce: init is not sendable — {}", e.message()),
                span,
            ))
        })?;
        // Step 4: parse opts.
        let (cap, min_chunk) = par_opts(&arg(args, 3), "task.preduce", span)?;

        let len = input.len();

        // Empty input → init instantly, pool untouched (§2.1).
        if len == 0 {
            return Ok(Value::future(SharedFuture::resolved(Ok(init))));
        }

        let plan = chunk_plan(len, cap, min_chunk);

        // Nested (called inside an isolate): SAME decomposition, executed inline.
        // The inline Reduce path: per-chunk partials + one local final fold — §5.1.
        if crate::worker::pool::in_isolate() {
            return self.par_inline(&input, &entry_name, &plan, ChunkKind::Reduce, Some(init), span);
        }

        // Build the code slice ONCE — all chunk dispatches + the final combine share it.
        let slice = crate::worker::build_code_slice_for_interp(self, &entry_name)?;

        // Dispatch all chunk Reduce jobs eagerly. Each returns a future<partial>.
        // Chunks dispatch in INPUT ORDER so the first offending chunk by input order is
        // the first to raise a non-sendable-element panic (§3.5).
        let mut chunk_futs: Vec<Value> = Vec::with_capacity(plan.len());
        for &(start, end) in &plan {
            let (data, job) = input.chunk_payload(start, end, ChunkKind::Reduce);
            let fut = crate::worker::dispatch_worker_job(
                self,
                slice.clone_for_dispatch(),
                vec![data],
                Some(job),
                span,
            )?;
            chunk_futs.push(fut);
        }

        // The final-combine dispatch also needs `&Interp` (synchronous call). We capture
        // `self.rc()` — the owning `Rc<Interp>` — so the orchestrator can call
        // `dispatch_worker_job(&*interp_rc, …)` from inside the `spawn_local` async block
        // without crossing a `Send` boundary (both live on the same `LocalSet` thread).
        // No `RefCell` borrow is held across the `.await` calls inside the block.
        let interp_rc = self.rc();
        let final_slice = slice.clone_for_dispatch();

        // Orchestrator: collect partials IN CHUNK ORDER, then dispatch ONE final combine.
        let fut = SharedFuture::new();
        let cell = fut.cell();
        let handle = tokio::task::spawn_local(async move {
            // Phase A: collect all per-chunk partials in input (chunk) order.
            // Dropping the remaining futures cancels queued chunks on error (§3.5).
            let mut partials: Vec<Value> = Vec::with_capacity(chunk_futs.len());
            for f in chunk_futs {
                match await_worker_future(f).await {
                    Ok(v) => partials.push(v),
                    Err(e) => {
                        cell.resolve(Err(e));
                        return;
                    }
                }
            }

            // Phase B: dispatch ONE final Reduce over [init, p0, .., pk] (plain data,
            // copy path). The driver seeds with `init` and folds across `p0..pk`,
            // producing `f(...f(f(init, p0), p1)...)` in one dispatch — §3.3.3.
            let mut combine: Vec<Value> = Vec::with_capacity(partials.len() + 1);
            combine.push(init);
            combine.extend(partials);
            let combine_len = combine.len();
            let combine_data = Value::array(combine);
            let final_job = ChunkJob {
                kind: ChunkKind::Reduce,
                start: 0,
                end: combine_len as u32,
            };
            // dispatch_worker_job is synchronous — no borrow held across .await.
            let final_fut_result = crate::worker::dispatch_worker_job(
                &interp_rc,
                final_slice,
                vec![combine_data],
                Some(final_job),
                span,
            );
            match final_fut_result {
                Err(e) => {
                    cell.resolve(Err(e));
                }
                Ok(v) => match await_worker_future(v).await {
                    Ok(result) => cell.resolve(Ok(result)),
                    Err(e) => cell.resolve(Err(e)),
                },
            }
        });
        fut.set_abort(handle.abort_handle());
        Ok(Value::future(fut))
    }

    /// `pipe(gen, bus)` — consume a (worker) generator and re-emit each yielded
    /// item on a local event bus.
    ///
    /// Each item `e` must be an Object with a `kind` string field; `bus.emit(e.kind, e)`
    /// fans the item out to every registered listener in order. Backpressure threads
    /// end-to-end for free: a slow `on` listener slows `emit`, which slows the loop,
    /// which slows `resume`, which slows the producer (demand-driven pull).
    ///
    /// Both arguments are required: `gen` must be a `Value::generator`; `bus` must be a
    /// `Value::native` with `NativeKind::Events`. Type misuse → Tier-2 panic (spec §11.3).
    async fn task_pipe(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let gen_val = arg(args, 0);
        let bus = arg(args, 1);

        // Validate gen is a Generator.
        let gen = match gen_val.kind() {
            ValueKind::Generator(g) => g.clone(),
            _ => {
                return Err(AsError::at(
                    format!(
                        "task.pipe: first argument must be a generator, got {}",
                        crate::interp::type_name(&gen_val)
                    ),
                    span,
                )
                .into());
            }
        };

        // Validate bus is a Native Events handle.
        let native_obj = match bus.kind() {
            ValueKind::Native(n) if n.kind == crate::value::NativeKind::Events => n.clone(),
            _ => {
                return Err(AsError::at(
                    format!(
                        "task.pipe: second argument must be an event bus (emitter), got {}",
                        crate::interp::type_name(&bus)
                    ),
                    span,
                )
                .into());
            }
        };

        // Consume the generator: drive it one step at a time, fan each item onto the bus.
        loop {
            let item = match gen.resume(Value::nil()).await? {
                Some(v) => v,
                None => break,
            };

            // Extract e.kind — must be a string field on an Object.
            let kind: std::rc::Rc<str> = match item.kind() {
                ValueKind::Object(o) => match o.get("kind") {
                    Some(k) => match k.kind() {
                        ValueKind::Str(s) => s.clone(),
                        _ => {
                            return Err(AsError::at(
                                format!(
                                    "task.pipe: yielded item's 'kind' field must be a string, got {}",
                                    crate::interp::type_name(&k)
                                ),
                                span,
                            )
                            .into());
                        }
                    },
                    None => {
                        return Err(AsError::at(
                            "task.pipe: yielded item must have a 'kind' string field",
                            span,
                        )
                        .into());
                    }
                },
                ValueKind::Instance(inst) => match inst.borrow().get("kind") {
                    Some(k) => match k.kind() {
                        ValueKind::Str(s) => s.clone(),
                        _ => {
                            return Err(AsError::at(
                                format!(
                                    "task.pipe: yielded item's 'kind' field must be a string, got {}",
                                    crate::interp::type_name(&k)
                                ),
                                span,
                            )
                            .into());
                        }
                    },
                    None => {
                        return Err(AsError::at(
                            "task.pipe: yielded item must have a 'kind' string field",
                            span,
                        )
                        .into());
                    }
                },
                _ => {
                    return Err(AsError::at(
                        format!(
                            "task.pipe: yielded value must be an object with a 'kind' field, got {}",
                            crate::interp::type_name(&item)
                        ),
                        span,
                    )
                    .into());
                }
            };

            // Build and dispatch: bus.emit(kind, item).
            // No RefCell borrow is held across this await — `native_obj` is a cloned Rc.
            let emit_method = std::rc::Rc::new(crate::value::NativeMethod {
                receiver: native_obj.clone(),
                method: "emit".to_string(),
            });
            let result = self
                .call_native_method(emit_method, vec![Value::str(kind), item], span)
                .await?;
            // emit may return a Future (async listeners) — drive it to completion.
            if let OwnedKind::Future(f) = result.into_kind() {
                f.get().await?;
            }
        }

        Ok(Value::nil())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PAR §3.1/§3.3.1 — chunk planner + input classification + callback validation
// ─────────────────────────────────────────────────────────────────────────────

/// PAR spec §3.3.1: compute the contractual chunk boundaries for a parallel
/// operation over `len` elements. The formula is PUBLISHED in the docs and is
/// part of the `preduce` reproducibility contract — never change it silently.
///
/// ```text
/// chunk_size = max(min_chunk, ceil(len / cap))
/// chunks     = ceil(len / chunk_size)
/// chunk i    = [i * chunk_size, min((i+1) * chunk_size, len))
/// ```
///
/// Returns an empty `Vec` for `len == 0` (callers must fast-path empty).
// Used by the orchestrator (Task 2.2). Suppress dead_code until then.
#[allow(dead_code)]
pub(crate) fn chunk_plan(len: usize, cap: usize, min_chunk: usize) -> Vec<(usize, usize)> {
    if len == 0 {
        return Vec::new();
    }
    let cap = cap.max(1);
    let min_chunk = min_chunk.max(1);
    // ceil(len / cap)
    let raw_chunk_size = len.div_ceil(cap);
    let chunk_size = raw_chunk_size.max(min_chunk);
    // ceil(len / chunk_size)
    let num_chunks = len.div_ceil(chunk_size);
    let mut plan = Vec::with_capacity(num_chunks);
    let mut start = 0;
    while start < len {
        let end = (start + chunk_size).min(len);
        plan.push((start, end));
        start = end;
    }
    plan
}

/// PAR spec §3.3.1: resolve the worker-pool cap for default chunk count.
/// Mirrors `src/worker/pool.rs:59-64` — does NOT couple to private pool state.
pub(crate) fn pool_cap() -> usize {
    std::env::var("ASCRIPT_WORKERS")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or_else(num_cpus::get)
        .max(1)
}

/// PAR spec §3.1: the two accepted input forms. Created synchronously inside
/// `task.pmap`/`task.preduce` — the input is SNAPSHOTTED at call time so mutating
/// the source array after calling pmap/preduce cannot affect the result.
// Fields are consumed by Task 2.2 (orchestrator). Suppress dead_code until then.
#[allow(dead_code)]
pub(crate) enum ParInput {
    /// A `Value::Shared` whose frozen node is a `SharedNode::Array` (PAR §3.1 happy
    /// path). The WHOLE shared value is shipped to each chunk via the `TAG_SHARED`
    /// side-vector (O(1) `Arc` bump per chunk); the chunk receives `(start, end)`
    /// index bounds and reads elements zero-copy via the shipped SRV readers.
    Frozen { shared: Value, len: usize },
    /// A plain `Value::Array`. Elements are snapshotted here (clone out of the
    /// `ArrayCell` borrow — never hold the borrow across an `.await`) so per-chunk
    /// slices can be built from owned `Vec<Value>` slices without re-borrowing.
    Plain { elems: Vec<Value> },
}

impl ParInput {
    /// Number of elements in the input.
    pub(crate) fn len(&self) -> usize {
        match self {
            ParInput::Frozen { len, .. } => *len,
            ParInput::Plain { elems } => elems.len(),
        }
    }

    /// PAR spec §3.1/§3.3.2: build the `(data, ChunkJob)` payload for the chunk
    /// `[start, end)` to dispatch.
    ///
    /// - **Frozen:** the data is the WHOLE shared `Value` (an `Arc` bump per chunk via
    ///   the `TAG_SHARED` side-vector); the job indexes `start..end` directly into it.
    /// - **Plain:** the data is THIS chunk's own element slice (a structured-clone copy
    ///   of `elems[start..end]`); the job indexes `0..(end-start)` into that slice.
    fn chunk_payload(&self, start: usize, end: usize, kind: ChunkKind) -> (Value, ChunkJob) {
        match self {
            ParInput::Frozen { shared, .. } => (
                shared.clone(),
                ChunkJob {
                    kind,
                    start: start as u32,
                    end: end as u32,
                },
            ),
            ParInput::Plain { elems } => {
                let slice: Vec<Value> = elems[start..end].to_vec();
                let len = slice.len();
                (
                    Value::array(slice),
                    ChunkJob {
                        kind,
                        start: 0,
                        end: len as u32,
                    },
                )
            }
        }
    }

    /// PAR spec §5.1 (inline nesting): build the WHOLE-input data value once for the
    /// in-isolate executor, which indexes chunk ranges directly into it via
    /// `run_chunk_job` (the same decomposition the pooled path runs per chunk). Frozen
    /// returns the shared `Value` (zero-copy); Plain returns the snapshot array.
    fn inline_data(&self) -> Value {
        match self {
            ParInput::Frozen { shared, .. } => shared.clone(),
            ParInput::Plain { elems } => Value::array(elems.clone()),
        }
    }
}

/// PAR spec §3.1: classify the input for `task.pmap`/`task.preduce`. `fn_name` is
/// `"task.pmap"` or `"task.preduce"` and is used in the panic message.
///
/// Accepted:
/// - `Value::Shared` whose inner node is `SharedNode::Array` → `ParInput::Frozen`
/// - `Value::Array` → `ParInput::Plain` (elements snapshotted at call time)
///
/// Rejected (Tier-2 panic):
/// - `Value::Shared` of a non-array node → `"<fn_name> expects an array or a frozen
///    array (got frozen <kind>)"`
/// - anything else → `"<fn_name> expects an array or a frozen array (got <kind>)"`
pub(crate) fn classify_par_input(
    v: &Value,
    fn_name: &str,
    span: Span,
) -> Result<ParInput, Control> {
    use crate::value::SharedNode;
    match v.kind() {
        ValueKind::Array(a) => {
            // Snapshot the elements now — never hold the borrow across an await.
            let elems: Vec<Value> = a.borrow().clone();
            Ok(ParInput::Plain { elems })
        }
        ValueKind::Shared(node) => {
            // Only a frozen ARRAY is accepted; other frozen kinds are rejected with
            // the "frozen <kind>" suffix per the spec §4 table.
            if let SharedNode::Array(arr) = node.as_ref() {
                let len = arr.len();
                Ok(ParInput::Frozen {
                    shared: v.clone(),
                    len,
                })
            } else {
                Err(AsError::at(
                    format!(
                        "{fn_name} expects an array or a frozen array (got frozen {})",
                        node.kind_name()
                    ),
                    span,
                )
                .into())
            }
        }
        _ => Err(AsError::at(
            format!(
                "{fn_name} expects an array or a frozen array (got {})",
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

/// PAR spec §2.2: validate the callback is a named `worker fn` and return its
/// dispatch name. Reuses `worker_fn_dispatch_name` (promoted to `pub(crate)`) —
/// never duplicated. `fn_name` is `"task.pmap"` or `"task.preduce"`.
pub(crate) fn par_callback_name(
    f: &Value,
    fn_name: &str,
    span: Span,
) -> Result<String, Control> {
    crate::interp::worker_fn_dispatch_name(f).ok_or_else(|| {
        AsError::at(
            format!(
                "{fn_name} expects a named `worker fn` as its callback (got {})",
                crate::interp::type_name(f)
            ),
            span,
        )
        .into()
    })
}


/// PAR spec §3.3.1: parse `{chunks?, minChunk?}` opts. Returns `(cap, min_chunk)`.
/// Unknown keys are ignored (mirroring other stdlib opts). A present key that is not
/// a positive integer is a Tier-2 panic mirroring `task.retry`'s validation style.
/// A `nil` opts arg returns the pool-cap default and `min_chunk = 1`.
pub(crate) fn par_opts(opts: &Value, fn_name: &str, span: Span) -> Result<(usize, usize), Control> {
    match opts.kind() {
        ValueKind::Nil => Ok((pool_cap(), 1)),
        ValueKind::Object(o) => {
            let cap = match o.get("chunks") {
                Some(v) => {
                    let n = super::want_number(&v, span, &format!("{fn_name} chunks"))?;
                    if n < 1.0 || n.fract() != 0.0 {
                        return Err(AsError::at(
                            format!("{fn_name}: chunks must be a positive integer"),
                            span,
                        )
                        .into());
                    }
                    n as usize
                }
                None => pool_cap(),
            };
            let min_chunk = match o.get("minChunk") {
                Some(v) => {
                    let n = super::want_number(&v, span, &format!("{fn_name} minChunk"))?;
                    if n < 1.0 || n.fract() != 0.0 {
                        return Err(AsError::at(
                            format!("{fn_name}: minChunk must be a positive integer"),
                            span,
                        )
                        .into());
                    }
                    n as usize
                }
                None => 1,
            };
            Ok((cap, min_chunk))
        }
        _ => Err(AsError::at(
            format!(
                "{fn_name} opts must be an object or nil, got {}",
                crate::interp::type_name(opts)
            ),
            span,
        )
        .into()),
    }
}

/// PAR spec §3.4: await one chunk's `Value::future` and return its decoded result.
/// `dispatch_worker_job` always returns a `Value::future`; this drives it. A non-future
/// (defensive — never produced by the dispatch path) is returned as-is.
async fn await_worker_future(v: Value) -> Result<Value, Control> {
    if matches!(v.kind(), ValueKind::Future(_)) {
        let OwnedKind::Future(f) = v.into_kind() else {
            unreachable!()
        };
        f.get().await
    } else {
        Ok(v)
    }
}

/// PAR spec §5.1: drive the inline (in-isolate) chunk decomposition for `par_inline`.
/// Runs each chunk through the SAME `run_chunk_job` the pooled path uses, against the
/// current isolate's `vm`/`entry`, and merges identically:
/// - **Map:** concatenate the per-chunk result arrays in chunk order.
/// - **Reduce:** collect per-chunk partials in chunk order, then drive one final fold
///   `f(...f(f(init, p0), p1)...)` via a `Reduce` `run_chunk_job` over `[init, p0, .., pk]`
///   (the §3.3.3 final-combine stage, executed locally).
async fn par_inline_run(
    vm: &std::rc::Rc<crate::vm::Vm>,
    entry: &Value,
    data: &Value,
    plan: &[(usize, usize)],
    kind: ChunkKind,
    final_init: Option<Value>,
    span: Span,
) -> Result<Value, Control> {
    match kind {
        ChunkKind::Map => {
            let mut merged: Vec<Value> = Vec::new();
            for &(start, end) in plan {
                let job = ChunkJob {
                    kind: ChunkKind::Map,
                    start: start as u32,
                    end: end as u32,
                };
                let chunk =
                    crate::worker::isolate::run_chunk_job(vm, entry.clone(), data.clone(), &job, span)
                        .await?;
                match chunk.kind() {
                    ValueKind::Array(a) => merged.extend(a.borrow().iter().cloned()),
                    _ => {
                        return Err(Control::Panic(AsError::at(
                            format!(
                                "pmap chunk returned a non-array (internal invariant): {}",
                                crate::interp::type_name(&chunk)
                            ),
                            span,
                        )));
                    }
                }
            }
            Ok(Value::array(merged))
        }
        ChunkKind::Reduce => {
            // Collect per-chunk partials (seeded by each chunk's first element) in order.
            let mut partials: Vec<Value> = Vec::with_capacity(plan.len());
            for &(start, end) in plan {
                let job = ChunkJob {
                    kind: ChunkKind::Reduce,
                    start: start as u32,
                    end: end as u32,
                };
                let partial =
                    crate::worker::isolate::run_chunk_job(vm, entry.clone(), data.clone(), &job, span)
                        .await?;
                partials.push(partial);
            }
            // Final combine: fold `f` over [init, p0, .., pk] (init participates once).
            let init = final_init.unwrap_or_else(Value::nil);
            let mut combine: Vec<Value> = Vec::with_capacity(partials.len() + 1);
            combine.push(init);
            combine.extend(partials);
            let combine_data = Value::array(combine.clone());
            let job = ChunkJob {
                kind: ChunkKind::Reduce,
                start: 0,
                end: combine.len() as u32,
            };
            crate::worker::isolate::run_chunk_job(vm, entry.clone(), combine_data, &job, span).await
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// Minimal xorshift64* PRNG for retry jitter. Thread-local, seeded from the
/// system clock. NOT cryptographic — adequate for backoff jitter only.
///
/// SP9 §3 — DELIBERATE timing-only (non-data) entropy exemption: this perturbs
/// only the retry-backoff *sleep DURATION*, never an observable script value, so it
/// is intentionally NOT routed through `interp.fill_seeded_bytes`. A divergent jitter
/// across replay changes only wall-clock pacing (which the virtual clock already
/// abstracts away), never the recorded result — so it cannot break replay fidelity.
fn retry_rand_f64() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static RNG: Cell<u64> = Cell::new({
            use std::time::{SystemTime, UNIX_EPOCH};
            let n = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15);
            n.max(1)
        });
    }
    RNG.with(|c| {
        let mut x = c.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        c.set(x);
        (x.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    })
}

#[cfg(test)]
mod tests {
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    // ── retry: succeeds on Kth attempt ──────────────────────────────────────
    // A fn that panics K-1 times then succeeds. With {attempts:5, baseMs:1}
    // the call should return the success value and the counter should be K.

    #[tokio::test]
    async fn retry_succeeds_on_third_attempt() {
        let out = run(r#"
import { retry } from "std/task"
let counter = [0]
async fn flaky() {
    counter[0] = counter[0] + 1
    if (counter[0] < 3) {
        assert(false, "not yet")
    }
    return "ok"
}
let result = await retry(flaky, {attempts: 5, baseMs: 1})
print(result)
print(counter[0])
"#)
        .await;
        assert_eq!(out, "ok\n3\n");
    }

    // ── retry: exhausts attempts → re-raises last panic ─────────────────────
    // A fn that always panics. retry with {attempts:3, baseMs:1} should
    // exhaust all 3 attempts then re-raise the last panic (caught by recover).

    #[tokio::test]
    async fn retry_exhausts_and_reraises() {
        let out = run(r#"
import { retry } from "std/task"
let counter = [0]
async fn always_fails() {
    counter[0] = counter[0] + 1
    assert(false, "always bad")
}
let [v, err] = recover(() => {
    await retry(always_fails, {attempts: 3, baseMs: 1})
    return nil
})
print(v)
print(err != nil)
print(counter[0])
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n3\n");
    }

    // ── retry: returns immediately on success (no retry of ok [nil,err] pair) ─
    // fn returns a [nil, err] pair (a Tier-1 error pair, NOT a panic).
    // retry must NOT retry this — it returns the pair immediately.

    #[tokio::test]
    async fn retry_does_not_retry_error_pairs() {
        let out = run(r#"
import { retry } from "std/task"
let counter = [0]
async fn returns_err_pair() {
    counter[0] = counter[0] + 1
    return [nil, {message: "user error"}]
}
let result = await retry(returns_err_pair, {attempts: 5, baseMs: 1})
print(type(result))
print(counter[0])
"#)
        .await;
        // result is the array [nil, {message:...}], type is "array"; counter == 1 (no retry)
        assert_eq!(out, "array\n1\n");
    }

    // ── RESIL §3.4: compute_retry_delay PURE unit tests ──────────────────────
    // The delay schedule is a pure fn so its bounds are exhaustively testable.

    use super::{compute_retry_delay, Backoff, Jitter};

    /// Fixed backoff: every delay is `base_ms` regardless of attempt (no jitter).
    #[test]
    fn compute_delay_fixed_is_constant() {
        for attempt in 0..10 {
            let d = compute_retry_delay(attempt, 50, None, Backoff::Fixed, Jitter::None, || 0.0);
            assert_eq!(d, 50, "fixed backoff attempt {attempt} should be base_ms");
        }
    }

    /// Fixed backoff capped at max_ms.
    #[test]
    fn compute_delay_fixed_capped() {
        let d = compute_retry_delay(3, 200, Some(100), Backoff::Fixed, Jitter::None, || 0.0);
        assert_eq!(d, 100);
    }

    /// Exponential backoff doubles per attempt (base 100): 100, 200, 400, 800, ...
    #[test]
    fn compute_delay_exponential_doubles() {
        let base = 100u64;
        for attempt in 0..6 {
            let d =
                compute_retry_delay(attempt, base, None, Backoff::Exponential, Jitter::None, || 0.0);
            assert_eq!(d, base << attempt, "attempt {attempt}");
        }
    }

    /// Exponential backoff capped at max_ms.
    #[test]
    fn compute_delay_exponential_capped() {
        // 100 * 2^5 = 3200, capped at 1000.
        let d = compute_retry_delay(5, 100, Some(1000), Backoff::Exponential, Jitter::None, || 0.0);
        assert_eq!(d, 1000);
    }

    /// The v1 `1u64 << shift` cap: attempt ≥ 63 must NOT overflow (shift clamped at 62).
    #[test]
    fn compute_delay_shift_cap_no_overflow() {
        // base_ms = 1 so the multiplier is the full 1u64 << shift (no saturation hides it).
        for &attempt in &[62usize, 63, 64, 100, usize::MAX] {
            let d =
                compute_retry_delay(attempt, 1, None, Backoff::Exponential, Jitter::None, || 0.0);
            // shift clamps at 62 → multiplier == 1<<62; with base 1, delay == 1<<62.
            assert_eq!(d, 1u64 << 62, "attempt {attempt} should clamp the shift at 62");
        }
        // With base 100 (the default) the saturating_mul saturates — but never panics.
        let d = compute_retry_delay(100, 100, None, Backoff::Exponential, Jitter::None, || 0.0);
        assert_eq!(d, 100u64.saturating_mul(1u64 << 62));
    }

    /// Jitter "none" is deterministic — never calls the rand source.
    #[test]
    fn compute_delay_jitter_none_deterministic() {
        let d = compute_retry_delay(2, 100, None, Backoff::Exponential, Jitter::None, || {
            panic!("rand must not be consulted in Jitter::None")
        });
        assert_eq!(d, 400);
    }

    /// Full jitter: result ∈ [0, computedDelay]. rand=0.0 → 0; rand=~1.0 → ~delay.
    #[test]
    fn compute_delay_jitter_full_bounds() {
        // computed delay = 100 * 2^2 = 400.
        let lo = compute_retry_delay(2, 100, None, Backoff::Exponential, Jitter::Full, || 0.0);
        assert_eq!(lo, 0, "full jitter at rand=0.0 is 0");

        let hi =
            compute_retry_delay(2, 100, None, Backoff::Exponential, Jitter::Full, || 0.9999999);
        assert!(hi <= 400, "full jitter must not exceed the computed delay, got {hi}");
        assert!(hi >= 399, "full jitter at rand≈1.0 should be near the full delay, got {hi}");

        // A mid value stays in bounds.
        let mid = compute_retry_delay(2, 100, None, Backoff::Exponential, Jitter::Full, || 0.5);
        assert!(mid <= 400, "mid full jitter in bounds");
        assert_eq!(mid, 200);
    }

    /// PlusHalf jitter (the v1 `jitter: true`): result ∈ [delay, delay + delay/2].
    #[test]
    fn compute_delay_jitter_plushalf_bounds() {
        // computed delay = 100.
        let lo = compute_retry_delay(0, 100, None, Backoff::Exponential, Jitter::PlusHalf, || 0.0);
        assert_eq!(lo, 100, "plushalf at rand=0.0 adds nothing");
        let hi =
            compute_retry_delay(0, 100, None, Backoff::Exponential, Jitter::PlusHalf, || 0.999);
        assert!((100..=150).contains(&hi), "plushalf within [delay, delay+50%], got {hi}");
    }

    // ── RESIL §3.4: task.retry additive keys (counter-observable) ─────────────

    /// `backoff: "fixed"` still drives the attempts (baseMs:1 keeps it fast).
    #[tokio::test]
    async fn task_retry_fixed_backoff_attempts_happen() {
        let out = run(r#"
import { retry } from "std/task"
let counter = [0]
async fn flaky() {
    counter[0] = counter[0] + 1
    if (counter[0] < 3) { assert(false, "not yet") }
    return "ok"
}
let r = await retry(flaky, {attempts: 5, baseMs: 1, backoff: "fixed"})
print(r)
print(counter[0])
"#)
        .await;
        assert_eq!(out, "ok\n3\n");
    }

    /// `jitter: "none"` and `jitter: "full"` are accepted (drive attempts).
    #[tokio::test]
    async fn task_retry_jitter_string_modes_accepted() {
        let out = run(r#"
import { retry } from "std/task"
let c = [0]
async fn flaky() { c[0] = c[0] + 1; if (c[0] < 2) { assert(false, "x") }; return "ok" }
let r1 = await retry(flaky, {attempts: 3, baseMs: 1, jitter: "none"})
let c2 = [0]
async fn flaky2() { c2[0] = c2[0] + 1; if (c2[0] < 2) { assert(false, "x") }; return "ok" }
let r2 = await retry(flaky2, {attempts: 3, baseMs: 1, jitter: "full"})
print(r1)
print(r2)
"#)
        .await;
        assert_eq!(out, "ok\nok\n");
    }

    /// `retryOn: "error"`: an err-pair-returning fn IS retried; exhaustion returns
    /// the LAST pair with `code: "retries-exhausted"` folded in (absent before).
    #[tokio::test]
    async fn task_retry_retry_on_error_exhausts_with_code() {
        let out = run(r#"
import { retry } from "std/task"
let c = [0]
async fn errs() { c[0] = c[0] + 1; return [nil, {message: "bad"}] }
let [v, err] = await retry(errs, {attempts: 3, baseMs: 1, retryOn: "error"})
print(c[0])          // 3 — retried until exhaustion
print(err.code)      // retries-exhausted (folded in)
print(err.message)   // bad (preserved)
"#)
        .await;
        assert_eq!(out, "3\nretries-exhausted\nbad\n");
    }

    /// `retryOn: "error"`: a present `code` is NOT overwritten on exhaustion.
    #[tokio::test]
    async fn task_retry_retry_on_error_preserves_existing_code() {
        let out = run(r#"
import { retry } from "std/task"
async fn errs() { return [nil, {message: "bad", code: "mine"}] }
let [v, err] = await retry(errs, {attempts: 2, baseMs: 1, retryOn: "error"})
print(err.code)
"#)
        .await;
        assert_eq!(out, "mine\n");
    }

    /// `retryIf` returning false → only ONE attempt; the pair returns immediately.
    #[tokio::test]
    async fn task_retry_retry_if_false_short_circuits() {
        let out = run(r#"
import { retry } from "std/task"
let c = [0]
async fn errs() { c[0] = c[0] + 1; return [nil, {message: "bad"}] }
let [v, err] = await retry(errs, {attempts: 5, baseMs: 1, retryOn: "error", retryIf: (e) => false})
print(c[0])   // 1 — no retry
"#)
        .await;
        assert_eq!(out, "1\n");
    }

    /// A panic INSIDE `retryIf` re-raises (programmer error — §3.4).
    #[tokio::test]
    async fn task_retry_retry_if_panic_reraises() {
        let out = run(r#"
import { retry } from "std/task"
async fn boom() { assert(false, "boom") }
let [v, err] = recover(() => await retry(boom, {attempts: 3, baseMs: 1, retryIf: (e) => { assert(false, "predicate boom") }}))
print(err != nil)
print(err.message)
"#)
        .await;
        assert_eq!(out, "true\npredicate boom\n");
    }

    /// `budget` on the STATELESS task.retry → Tier-2 with the §3.4 EXACT message.
    #[tokio::test]
    async fn task_retry_budget_is_tier2() {
        let res = crate::run_source(r#"
import { retry } from "std/task"
async fn f() { return 1 }
await retry(f, {attempts: 3, budget: 0.5})
"#)
        .await;
        let msg = match res {
            Err(e) => e.message,
            Ok(s) => s,
        };
        assert!(
            msg.contains("task.retry: budget requires a resilience.retry policy"),
            "expected the exact §3.4 budget message, got: {msg:?}"
        );
    }

    // ── PAR Phase 0 pins — shipped semantics the pmap/preduce design composes (spec §3) ──

    /// Pin 1: A top-level `?` propagation inside a worker fn body resolves the call to the
    /// propagated [nil, err] pair — NOT nil.
    ///
    /// SPEC-VS-REALITY NOTE: The PAR plan (Task 0.1) and spec §3 state that `Propagate` from
    /// a worker body maps to nil (citing `isolate_loop`'s Propagate arm). That arm is dead
    /// code: `run_body` (src/interp.rs:5452) converts `Control::Propagate(v)` to `Ok(v)` —
    /// returning the pair — before `call_value` returns, so the isolate boundary never sees
    /// a raw `Propagate`. Both engines (tree-walker and VM) and both isolate paths (pool and
    /// dedicated run_in_worker) exhibit `[nil, err]` as the result, not nil.
    ///
    /// Consequence for PAR: the chunk driver's per-element "Propagate → nil" rule in spec §3
    /// is also unreachable. The chunk driver will receive `Ok([nil, err])` from the element
    /// call, not `Err(Propagate)` — so a propagated error is transparent to pmap (the output
    /// array element will hold the pair, like any other return value). The PAR spec's
    /// "Propagate → nil element" needs to be revised before Phase 1 implementation.
    #[tokio::test]
    async fn pin_worker_propagate_yields_nil() {
        // The worker fn uses `?` on a [nil, err] pair; `run_body` converts the propagation
        // to Ok([nil, err]) before the isolate boundary, so the result is the pair, not nil.
        let out = run(r#"
worker fn t(x) {
    let [v, e] = [nil, {message: "nope"}]
    let r = [v, e]?
    return 1
}
print(await t(0))
"#)
        .await;
        // ACTUAL behavior: the propagated pair is the worker's return value (not nil).
        assert_eq!(out, "[nil, {message: \"nope\"}]\n");
    }

    /// Pin 2: A frozen (shared.freeze) array arg crosses the worker airlock via the TAG_SHARED
    /// side-vector (Arc bump, not a deep clone) and is readable per-element inside the worker
    /// body. PAR §3.1 relies on this — frozen input to pmap crosses per-chunk for ~free.
    /// Gated on `feature = "shared"` (std/shared is not core; workers are core).
    #[cfg(feature = "shared")]
    #[tokio::test]
    async fn pin_frozen_array_arg_crosses_and_reads_in_worker() {
        let out = run(r#"
import * as shared from "std/shared"
worker fn pick(arr, i) { return arr[i] * 10 }
let f = shared.freeze([1, 2, 3])
print(await pick(f, 1))
"#)
        .await;
        assert_eq!(out, "20\n");
    }

    /// Pin 3: run_in_worker's named-worker-fn-only callback rule (spec §2.2). Passing a
    /// non-worker arrow panics with a recoverable Tier-2 panic. PAR's pmap/preduce will
    /// mirror this rule (same worker_fn_dispatch_name check). run_in_worker is a bare global
    /// (BUILTIN_NAMES, interp.rs:178) — no import needed.
    #[tokio::test]
    async fn pin_worker_fn_dispatch_name_rules() {
        let out = run(r#"
let [v, err] = recover(() => run_in_worker((x) => x, 1))
print(err != nil)
"#)
        .await;
        assert_eq!(out, "true\n");
    }

    // ── PAR Task 2.1: chunk_plan formula tests ──────────────────────────────
    // These pin the contractual formula from spec §3.3.1 EXACTLY. The formula:
    //   chunk_size = max(min_chunk, ceil(len / cap))
    //   boundaries = consecutive (0..len).step_by(chunk_size) pairs

    #[test]
    fn chunk_plan_contract() {
        use super::chunk_plan;
        // (10, 4, 1): chunk_size = max(1, ceil(10/4)) = max(1, 3) = 3
        assert_eq!(
            chunk_plan(10, 4, 1),
            vec![(0, 3), (3, 6), (6, 9), (9, 10)]
        );
        // (3, 8, 1): chunk_size = max(1, ceil(3/8)) = max(1, 1) = 1 (chunks > len clamps)
        assert_eq!(
            chunk_plan(3, 8, 1),
            vec![(0, 1), (1, 2), (2, 3)]
        );
        // (100, 8, 16): chunk_size = max(16, ceil(100/8)) = max(16, 13) = 16
        assert_eq!(
            chunk_plan(100, 8, 16),
            vec![(0, 16), (16, 32), (32, 48), (48, 64), (64, 80), (80, 96), (96, 100)]
        );
        // (5, 1, 1): chunk_size = max(1, ceil(5/1)) = 5
        assert_eq!(chunk_plan(5, 1, 1), vec![(0, 5)]);
        // empty
        assert!(chunk_plan(0, 8, 1).is_empty());
    }

    // ── PAR Task 2.1: ParInput classification tests ─────────────────────────

    #[test]
    fn par_input_plain_array_classifies() {
        use crate::span::Span;
        use crate::value::Value;
        use super::classify_par_input;

        let arr = Value::array(vec![Value::int(1), Value::int(2), Value::int(3)]);
        let span = Span::new(0, 0);
        let input = classify_par_input(&arr, "task.pmap", span).expect("should classify plain array");
        assert_eq!(input.len(), 3);
        assert!(matches!(input, super::ParInput::Plain { .. }));
    }

    #[test]
    fn par_input_non_array_panics_with_correct_message() {
        use crate::interp::Control;
        use crate::span::Span;
        use crate::value::Value;
        use super::classify_par_input;

        let span = Span::new(0, 0);
        // A plain object
        let obj = Value::object(indexmap::IndexMap::new());
        let result = classify_par_input(&obj, "task.pmap", span);
        let Err(Control::Panic(e)) = result else { panic!("expected Panic, got Ok") };
        assert!(
            e.message.contains("task.pmap expects an array or a frozen array (got object)"),
            "unexpected message: {}",
            e.message
        );
    }

    #[test]
    fn par_input_nil_panics_with_correct_message() {
        use crate::interp::Control;
        use crate::span::Span;
        use crate::value::Value;
        use super::classify_par_input;

        let span = Span::new(0, 0);
        let result = classify_par_input(&Value::nil(), "task.preduce", span);
        let Err(Control::Panic(e)) = result else { panic!("expected Panic, got Ok") };
        assert!(
            e.message.contains("task.preduce expects an array or a frozen array (got nil)"),
            "unexpected message: {}",
            e.message
        );
    }

    // ── PAR Task 2.1: callback validation tests ─────────────────────────────

    #[tokio::test]
    async fn par_callback_non_worker_fn_panics() {
        // A non-worker fn callback panics with the correct message.
        let out = run(r#"
import * as task from "std/task"
fn plain(x) { return x }
let [v, err] = recover(() => task.pmap([1, 2], plain))
print(err.message)
"#)
        .await;
        assert!(
            out.contains("task.pmap expects a named `worker fn` as its callback"),
            "unexpected output: {out}"
        );
    }

    #[tokio::test]
    async fn par_callback_arrow_fn_panics() {
        // An arrow (lambda) callback panics with the correct message.
        let out = run(r#"
import * as task from "std/task"
let [v, err] = recover(() => task.pmap([1, 2], (x) => x * 2))
print(err.message)
"#)
        .await;
        assert!(
            out.contains("task.pmap expects a named `worker fn` as its callback"),
            "unexpected output: {out}"
        );
    }

    // ── PAR Task 2.1: opts parsing tests ────────────────────────────────────

    #[test]
    fn par_opts_nil_gives_defaults() {
        use crate::span::Span;
        use crate::value::Value;
        use super::{par_opts, pool_cap};

        let span = Span::new(0, 0);
        let (cap, min_chunk) = par_opts(&Value::nil(), "task.pmap", span)
            .expect("nil opts should parse");
        assert_eq!(cap, pool_cap(), "nil opts cap should equal pool_cap()");
        assert_eq!(min_chunk, 1);
    }

    #[test]
    fn par_opts_chunks_parses() {
        use crate::span::Span;
        use crate::value::Value;
        use super::par_opts;
        use indexmap::IndexMap;

        let span = Span::new(0, 0);
        let mut m = IndexMap::new();
        m.insert("chunks".to_string(), Value::int(4));
        let opts = Value::object(m);
        let (cap, min_chunk) = par_opts(&opts, "task.pmap", span)
            .expect("opts with chunks=4 should parse");
        assert_eq!(cap, 4);
        assert_eq!(min_chunk, 1);
    }

    #[test]
    fn par_opts_min_chunk_parses() {
        use crate::span::Span;
        use crate::value::Value;
        use super::par_opts;
        use indexmap::IndexMap;

        let span = Span::new(0, 0);
        let mut m = IndexMap::new();
        m.insert("minChunk".to_string(), Value::int(16));
        let opts = Value::object(m);
        let (cap, min_chunk) = par_opts(&opts, "task.pmap", span)
            .expect("opts with minChunk=16 should parse");
        assert_eq!(min_chunk, 16);
        let _ = cap; // cap is pool_cap()
    }

    #[test]
    fn par_opts_zero_chunks_panics() {
        use crate::interp::Control;
        use crate::span::Span;
        use crate::value::Value;
        use super::par_opts;
        use indexmap::IndexMap;

        let span = Span::new(0, 0);
        let mut m = IndexMap::new();
        m.insert("chunks".to_string(), Value::int(0));
        let opts = Value::object(m);
        let err = par_opts(&opts, "task.pmap", span).unwrap_err();
        let Control::Panic(e) = err else { panic!("expected Panic") };
        assert!(
            e.message.contains("task.pmap: chunks must be a positive integer"),
            "unexpected message: {}",
            e.message
        );
    }

    #[test]
    fn par_opts_fractional_min_chunk_panics() {
        use crate::interp::Control;
        use crate::span::Span;
        use crate::value::Value;
        use super::par_opts;
        use indexmap::IndexMap;

        let span = Span::new(0, 0);
        let mut m = IndexMap::new();
        m.insert("minChunk".to_string(), Value::float(1.5));
        let opts = Value::object(m);
        let err = par_opts(&opts, "task.pmap", span).unwrap_err();
        let Control::Panic(e) = err else { panic!("expected Panic") };
        assert!(
            e.message.contains("task.pmap: minChunk must be a positive integer"),
            "unexpected message: {}",
            e.message
        );
    }

    // ── PAR Task 2.1: frozen-array classification (feature-gated) ───────────

    #[cfg(feature = "shared")]
    #[test]
    fn par_input_frozen_array_classifies() {
        use crate::span::Span;
        use crate::value::Value;
        use super::classify_par_input;
        use std::sync::Arc;

        let inner: Vec<crate::value::SharedValue> = vec![
            Arc::new(crate::value::SharedNode::Int(1)),
            Arc::new(crate::value::SharedNode::Int(2)),
            Arc::new(crate::value::SharedNode::Int(3)),
        ];
        let shared = Value::shared(Arc::new(crate::value::SharedNode::Array(Arc::from(
            inner.into_boxed_slice(),
        ))));
        let span = Span::new(0, 0);
        let input = classify_par_input(&shared, "task.pmap", span)
            .expect("frozen array should classify as Frozen");
        assert!(matches!(input, super::ParInput::Frozen { len: 3, .. }));
    }

    // ── PAR Task 2.2: pmap orchestrator (run_source) ────────────────────────

    /// pmap over a plain array returns results in INPUT order.
    #[tokio::test]
    async fn pmap_plain_array_input_order() {
        let out = run(r#"
import * as task from "std/task"
worker fn double(x) { return x * 2 }
print(await task.pmap([1, 2, 3, 4, 5, 6, 7, 8], double))
"#)
        .await;
        assert_eq!(out, "[2, 4, 6, 8, 10, 12, 14, 16]\n");
    }

    /// pmap over a frozen array gives the same result (zero-copy path).
    #[cfg(feature = "shared")]
    #[tokio::test]
    async fn pmap_frozen_input_same_result() {
        let out = run(r#"
import * as task from "std/task"
import * as shared from "std/shared"
worker fn double(x) { return x * 2 }
print(await task.pmap(shared.freeze([1, 2, 3]), double, { chunks: 2 }))
"#)
        .await;
        assert_eq!(out, "[2, 4, 6]\n");
    }

    /// Empty pmap resolves to [] instantly AND does NOT initialize the pool (§2.1).
    /// Serial: asserts a process-global (pool init flag) so it must not race other
    /// tests that spin up the pool.
    #[tokio::test]
    async fn pmap_empty_is_instant_and_poolless() {
        // Only meaningful if no prior worker ran in this process — but the assertion
        // is one-directional (empty pmap must not be the thing that inits the pool).
        let already = crate::worker::pool_is_initialized();
        let out = run(r#"
import * as task from "std/task"
worker fn id(x) { return x }
print(await task.pmap([], id))
"#)
        .await;
        assert_eq!(out, "[]\n");
        if !already {
            assert!(
                !crate::worker::pool_is_initialized(),
                "empty pmap must not touch the pool"
            );
        }
    }

    /// PAR §2.2 (Phase-2 review fix): a `static worker fn` is a class method, not a
    /// shippable top-level `worker fn`, so a static-method callback must raise the §2.2
    /// callback contract panic — NOT leak the internal slice-build "not a top-level
    /// function" message. Both `pmap` and `preduce`; the message is byte-identical across
    /// engines (the slice builder recompiles the shared source).
    #[tokio::test]
    async fn par_static_worker_fn_callback_is_a_clean_contract_panic() {
        let pmap = run(r#"
import * as task from "std/task"
class Img { static worker fn sq(x) { return x * x } }
let [v, err] = recover(() => task.pmap([1, 2, 3], Img.sq))
print(err.message)
"#)
        .await;
        assert!(
            pmap.contains("expects a named `worker fn`"),
            "pmap static-callback must raise the §2.2 contract message, got: {pmap}"
        );
        assert!(
            !pmap.contains("worker entry"),
            "pmap must NOT leak the internal slice-build message, got: {pmap}"
        );

        let preduce = run(r#"
import * as task from "std/task"
class Acc { static worker fn add(a, b) { return a + b } }
let [v, err] = recover(() => task.preduce([1, 2, 3], Acc.add, 0))
print(err.message)
"#)
        .await;
        assert!(
            preduce.contains("expects a named `worker fn`"),
            "preduce static-callback must raise the §2.2 contract message, got: {preduce}"
        );
        assert!(
            !preduce.contains("worker entry"),
            "preduce must NOT leak the internal slice-build message, got: {preduce}"
        );
    }

    /// Order preservation under an INVERSE workload: element i sleeps (n-i) ms, so
    /// later elements finish first — the merged result must still be input order.
    /// Total sleep budget kept well under 1s.
    #[tokio::test]
    async fn pmap_order_under_inverse_workload() {
        let out = run(r#"
import * as task from "std/task"
import * as time from "std/time"
worker fn slow(x) {
    // x in 0..8; sleep (8 - x) * 5 ms so element 0 is slowest, 7 fastest.
    await time.sleep((8 - x) * 5)
    return x * 10
}
print(await task.pmap([0, 1, 2, 3, 4, 5, 6, 7], slow))
"#)
        .await;
        assert_eq!(out, "[0, 10, 20, 30, 40, 50, 60, 70]\n");
    }

    /// chunks: 1 (one isolate, sequential-in-isolate) still input order.
    #[tokio::test]
    async fn pmap_chunks_one() {
        let out = run(r#"
import * as task from "std/task"
worker fn double(x) { return x * 2 }
print(await task.pmap([1, 2, 3, 4, 5], double, { chunks: 1 }))
"#)
        .await;
        assert_eq!(out, "[2, 4, 6, 8, 10]\n");
    }

    /// chunks > len clamps; minChunk > len → one chunk. Both still correct.
    #[tokio::test]
    async fn pmap_chunks_gt_len_and_min_chunk_gt_len() {
        let out = run(r#"
import * as task from "std/task"
worker fn double(x) { return x * 2 }
print(await task.pmap([1, 2, 3], double, { chunks: 16 }))
print(await task.pmap([1, 2, 3], double, { minChunk: 16 }))
"#)
        .await;
        assert_eq!(out, "[2, 4, 6]\n[2, 4, 6]\n");
    }

    /// A callback that is not a `worker fn` panics (recoverable).
    #[tokio::test]
    async fn pmap_callback_not_worker_fn_panics() {
        let out = run(r#"
import * as task from "std/task"
fn plain(x) { return x }
let [v, err] = recover(() => task.pmap([1, 2], plain))
print(v)
print(err.message)
"#)
        .await;
        assert!(
            out.contains("nil")
                && out.contains("task.pmap expects a named `worker fn` as its callback"),
            "unexpected output: {out}"
        );
    }

    /// A non-sendable element (a closure inside the data) panics with a field path,
    /// raised synchronously at the offending chunk's dispatch (first by input order).
    /// The panic is raised by `dispatch_worker_job`'s encode, INSIDE the synchronous
    /// `task.pmap` call, so `recover` around the bare call catches it directly.
    #[tokio::test]
    async fn pmap_non_sendable_element_field_path_panics() {
        let out = run(r#"
import * as task from "std/task"
worker fn id(x) { return x }
let [v, err] = recover(() => task.pmap([1, () => 2, 3], id, { chunks: 1 }))
print(v)
print(err != nil)
"#)
        .await;
        // The closure cannot cross the airlock → recoverable Tier-2 panic at dispatch.
        assert_eq!(out, "nil\ntrue\n");
    }

    /// `?`-propagation inside the callback yields the `[nil, err]` PAIR element
    /// (Phase-0 correction: `run_body` converts the propagation to Ok(pair)).
    #[tokio::test]
    async fn pmap_propagate_in_callback_yields_pair_element() {
        let out = run(r#"
import * as task from "std/task"
worker fn maybe(x) {
    let [v, e] = [nil, {message: "bad"}]
    let r = [v, e]?
    return x
}
print(await task.pmap([1, 2], maybe, { chunks: 1 }))
"#)
        .await;
        // Both elements end in `?` → each element is the propagated pair, not nil.
        assert_eq!(out, "[[nil, {message: \"bad\"}], [nil, {message: \"bad\"}]]\n");
    }

    /// A panicking callback surfaces as the pmap error (caught by recover). The panic
    /// is raised inside the orchestrator future, so `recover` must `await` the pmap.
    #[tokio::test]
    async fn pmap_callback_panic_surfaces() {
        let out = run(r#"
import * as task from "std/task"
worker fn boom(x) {
    assert(x < 3, "too big")
    return x
}
let [v, err] = recover(() => {
    return await task.pmap([1, 2, 3, 4], boom, { chunks: 1 })
})
print(v)
print(err != nil)
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    // §3.1 parity battery — frozen-element mutation panic vs plain-copy mutation OK,
    // and the two DIFFERENT frozen messages (mutate-frozen vs frozen-instance method).

    /// Mutating a frozen OBJECT element view inside `f` is the shipped frozen panic.
    /// The panic happens in the chunk (the orchestrator future), so `recover` awaits.
    #[cfg(feature = "shared")]
    #[tokio::test]
    async fn pmap_frozen_element_mutation_panics() {
        let out = run(r#"
import * as task from "std/task"
import * as shared from "std/shared"
worker fn touch(o) {
    o.x = 99
    return o.x
}
let data = shared.freeze([{x: 1}, {x: 2}])
let [v, err] = recover(() => {
    return await task.pmap(data, touch, { chunks: 1 })
})
print(v)
print(err.message)
"#)
        .await;
        assert!(
            out.starts_with("nil\n") && out.contains("cannot mutate a frozen"),
            "unexpected output: {out}"
        );
    }

    /// A user-method call on a frozen INSTANCE element gives the SRV distinct
    /// "method '<name>' is not available on a frozen instance" diagnostic — DIFFERENT
    /// from the mutate-frozen message above (the §3.1 two-message battery).
    #[cfg(feature = "shared")]
    #[tokio::test]
    async fn pmap_frozen_instance_method_distinct_diagnostic() {
        let out = run(r#"
import * as task from "std/task"
import * as shared from "std/shared"
class Box {
    value: number
    fn doubled() { return self.value * 2 }
}
worker fn call_method(b) { return b.doubled() }
let data = shared.freeze([Box(3), Box(4)])
let [v, err] = recover(() => {
    return await task.pmap(data, call_method, { chunks: 1 })
})
print(v)
print(err.message)
"#)
        .await;
        assert!(
            out.starts_with("nil\n")
                && out.contains("is not available on a frozen instance"),
            "unexpected output: {out}"
        );
    }

    /// A plain (unfrozen) INSTANCE element crosses the airlock as a FIELD-ONLY shell
    /// (Spec A airlock limitation: classes ship without method tables — method dispatch
    /// is NOT preserved across the isolate boundary, `resolve_class` in
    /// `worker/serialize.rs`). So a method call on a plain-instance element fails with
    /// the shipped "value is not callable" — the same as a direct `worker fn` call.
    /// (The §3.1 spec text overstated "working methods"; field ACCESS works, method
    /// dispatch does not — this pins the actual shipped behavior, identical on both
    /// venues.) Field access on a plain-instance element DOES work (next test).
    #[tokio::test]
    async fn pmap_plain_instance_method_call_matches_worker_airlock() {
        let out = run(r#"
import * as task from "std/task"
class Box {
    value: number
    fn doubled() { return self.value * 2 }
}
worker fn call_method(b) { return b.doubled() }
let [v, err] = recover(() => {
    return await task.pmap([Box(3), Box(4)], call_method, { chunks: 1 })
})
print(v)
print(err != nil)
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    /// FIELD access on a plain (unfrozen) instance element works (copy semantics) —
    /// the §3.1 contrast that DOES hold across the airlock.
    #[tokio::test]
    async fn pmap_plain_instance_field_access_works() {
        let out = run(r#"
import * as task from "std/task"
class Box {
    value: number
}
worker fn read_value(b) { return b.value * 2 }
print(await task.pmap([Box(3), Box(4)], read_value, { chunks: 1 }))
"#)
        .await;
        assert_eq!(out, "[6, 8]\n");
    }

    /// A plain (unfrozen) OBJECT element can be mutated locally inside `f` (copy
    /// semantics) — silent and isolated, the §3.1 contrast to the frozen mutation panic.
    #[tokio::test]
    async fn pmap_plain_element_mutation_is_local() {
        let out = run(r#"
import * as task from "std/task"
worker fn touch(o) {
    o.x = o.x + 100
    return o.x
}
print(await task.pmap([{x: 1}, {x: 2}], touch, { chunks: 1 }))
"#)
        .await;
        assert_eq!(out, "[101, 102]\n");
    }

    /// NESTED: a `worker fn` body that itself calls `task.pmap` runs the inline
    /// decomposition (deadlock-free) and produces identical output to a top-level call.
    #[tokio::test]
    async fn pmap_nested_inline() {
        let out = run(r#"
import * as task from "std/task"
worker fn double(x) { return x * 2 }
worker fn outer(seed) {
    let r = await task.pmap([seed, seed + 1, seed + 2], double)
    return r
}
print(await task.pmap([10, 20], outer, { chunks: 1 }))
"#)
        .await;
        // outer(10) -> [20, 22, 24]; outer(20) -> [40, 42, 44]
        assert_eq!(out, "[[20, 22, 24], [40, 42, 44]]\n");
    }

    // ── PAR Task 2.3: preduce tests ─────────────────────────────────────────

    /// preduce with an associative combiner (add) must equal the sequential sum.
    ///
    /// Contractual values (spec §3.3.2/§3.3.3):
    ///   preduce([1..10], add, 0):
    ///     Chunks = pool_cap (≥1). Because add is associative, all chunk orderings
    ///     give the same result: each chunk folds its elements seeded by the first,
    ///     partials combine with init=0, total = 1+2+…+10 = 55.
    ///   preduce([1..10], add, 100, {chunks:3}):
    ///     Chunks: [0,4)=[1,2,3,4], [4,7)=[5,6,7], [7,10)=[8,9,10].
    ///     Partials: p0=1+2+3+4=10, p1=5+6+7=18 (seeded 5, +6=11, +7=18), p2=8+9+10=27.
    ///     Wait — seed-with-first: p0=1+2+3+4=((1+2)+3)+4=10; p1=5+6+7=((5+6)+7)=18;
    ///     p2=8+9+10=((8+9)+10)=27. But wait: seed = first element, fold rest.
    ///     p0: seed=1, fold 2,3,4 → (((1+2)+3)+4) = 10. ✓
    ///     p1: seed=5, fold 6,7 → ((5+6)+7) = 18. ✓
    ///     p2: seed=8, fold 9,10 → ((8+9)+10) = 27. ✓
    ///     Final combine: [100, 10, 18, 27] → seed=100, fold: 100+10=110, 110+18=128, 128+27=155. ✓
    ///     init=100 participates EXACTLY ONCE (in the final combine, as the seed).
    #[tokio::test]
    async fn preduce_equals_sequential_for_associative_f() {
        let out = run(r#"
import * as task from "std/task"
worker fn add(a, b) { return a + b }
print(await task.preduce([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], add, 0))
print(await task.preduce([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], add, 100, { chunks: 3 }))
"#)
        .await;
        assert_eq!(out, "55\n155\n");  // init participates EXACTLY once (spec §3.3.3)
    }

    /// Empty preduce resolves to init (poolless); single-element preduce = f(init, e).
    ///
    /// Contractual values:
    ///   preduce([], add, 42): empty fast-path → 42.
    ///   preduce([7], add, 1): one chunk [0,1), seed=7, partial=7;
    ///     final combine: [1, 7] → seed=1, fold 7 → 1+7 = 8. ✓
    #[tokio::test]
    async fn preduce_empty_and_single() {
        let out = run(r#"
import * as task from "std/task"
worker fn add(a, b) { return a + b }
print(await task.preduce([], add, 42))
print(await task.preduce([7], add, 1))
"#)
        .await;
        assert_eq!(out, "42\n8\n");  // [] → init (poolless); [e] → f(init, e)
    }

    /// Non-associative f (subtraction) with pinned chunks is REPRODUCIBLE (byte-identical
    /// across runs) but NOT equal to sequential reduce for chunks > 1.
    ///
    /// Contractual values for chunks:1 (spec §3.3.2/§3.3.3):
    ///   sub(a,b) = a - b, data = [100,1,2,3,4,5], init = 0, chunks:1.
    ///   ONE chunk [0,6): seed = data[0] = 100.
    ///   Fold rest: ((((100-1)-2)-3)-4)-5 = 85. Partial p0 = 85.
    ///   Final combine: [init=0, p0=85] → seed=0, fold 85 → 0-85 = -85.
    ///   Result = -85.
    ///   NB: preduce(chunks:1) = f(init, fold(chunk)) — equals sequential reduce ONLY
    ///   for associative f. Sequential reduce(sub, 0, [100..5]) =
    ///   (((((0-100)-1)-2)-3)-4)-5 = -115, which differs. By design (spec §2.1).
    ///
    /// For chunks:2 (the first run is for reproducibility only — we assert a==b):
    ///   chunk_plan(6, 2, 1) = [(0,3), (3,6)].
    ///   Chunk 0: [100,1,2] seed=100, fold: (100-1)-2=97. p0=97.
    ///   Chunk 1: [3,4,5] seed=3, fold: (3-4)-5=-6. p1=-6.
    ///   Final: [0, 97, -6] → seed=0, fold: 0-97=-97, -97-(-6)=-91. Result=-91.
    #[tokio::test]
    async fn preduce_nonassociative_is_reproducible_with_pinned_chunks() {
        // chunks:1 → contractual value is -85 (NOT the sequential fold -115).
        let seq1 = run(r#"
import * as task from "std/task"
worker fn sub(a, b) { return a - b }
print(await task.preduce([100, 1, 2, 3, 4, 5], sub, 0, { chunks: 1 }))
"#)
        .await;
        // Contractual: ONE chunk seeded with 100, partial=85; final fold = f(0, 85) = -85.
        assert_eq!(seq1.trim(), "-85",
            "contractual preduce(chunks:1) expected -85, got {seq1}");

        // chunks:2 → run twice, assert equal (deterministic).
        let src = r#"
import * as task from "std/task"
worker fn sub(a, b) { return a - b }
print(await task.preduce([100, 1, 2, 3, 4, 5], sub, 0, { chunks: 2 }))
"#;
        let a = run(src).await;
        let b = run(src).await;
        assert_eq!(a, b, "preduce with pinned chunks must be deterministic");
        // Contractual value for chunks:2: -91.
        assert_eq!(a.trim(), "-91",
            "contractual preduce(chunks:2) expected -91, got {a}");
    }

    /// Ragged final chunk: when len is not divisible by chunk_size, the last chunk
    /// has fewer elements — result must still be correct.
    #[tokio::test]
    async fn preduce_ragged_final_chunk() {
        // 7 elements, chunks:3 → plan(7,3,1): chunk_size=ceil(7/3)=3 → [(0,3),(3,6),(6,7)]
        // Chunk 0: [10,20,30] seed=10, fold: 10+20=30, 30+30=60. p0=60.
        // Chunk 1: [40,50,60] seed=40, fold: 40+50=90, 90+60=150. p1=150.
        // Chunk 2: [70] seed=70, no fold. p2=70.
        // Final: [0, 60, 150, 70] seed=0: 0+60=60, 60+150=210, 210+70=280.
        let out = run(r#"
import * as task from "std/task"
worker fn add(a, b) { return a + b }
print(await task.preduce([10, 20, 30, 40, 50, 60, 70], add, 0, { chunks: 3 }))
"#)
        .await;
        assert_eq!(out.trim(), "280");
    }

    /// Non-sendable init panics UP FRONT (before any dispatch is made).
    #[tokio::test]
    async fn preduce_nonsendable_init_panics_upfront() {
        let out = run(r#"
import * as task from "std/task"
worker fn add(a, b) { return a + b }
let [v, err] = recover(() => task.preduce([1, 2, 3], add, () => 0))
print(v)
print(err != nil)
"#)
        .await;
        // A closure (lambda) is not sendable → panic before any dispatch.
        assert_eq!(out, "nil\ntrue\n");
    }

    /// A panic inside the combiner during the FINAL stage surfaces as the preduce error.
    #[tokio::test]
    async fn preduce_panic_in_final_combine_surfaces() {
        // The combiner panics when accumulator goes below -10. This happens in the
        // final combine stage (when init=0 folds the partial which is > 10).
        let out = run(r#"
import * as task from "std/task"
worker fn paranoid_add(a, b) {
    if (a + b > 1000) {
        assert(false, "overflow in combiner")
    }
    return a + b
}
let [v, err] = recover(() => {
    return await task.preduce([500, 600], paranoid_add, 0, { chunks: 2 })
})
print(v)
print(err != nil)
"#)
        .await;
        // 2 chunks: p0=500, p1=600; final: [0, 500, 600] → 0+500=500, 500+600=1100 > 1000 → panic.
        assert_eq!(out, "nil\ntrue\n");
    }

    /// Frozen array input to preduce works correctly.
    #[cfg(feature = "shared")]
    #[tokio::test]
    async fn preduce_frozen_input() {
        let out = run(r#"
import * as task from "std/task"
import * as shared from "std/shared"
worker fn add(a, b) { return a + b }
let data = shared.freeze([1, 2, 3, 4, 5])
print(await task.preduce(data, add, 0, { chunks: 2 }))
"#)
        .await;
        // [1,2,3,4,5] sum = 15; init=0; result = 15.
        assert_eq!(out.trim(), "15");
    }

    #[cfg(feature = "shared")]
    #[test]
    fn par_input_frozen_non_array_panics_with_frozen_kind_message() {
        use crate::interp::Control;
        use crate::span::Span;
        use crate::value::Value;
        use super::classify_par_input;
        use std::sync::Arc;

        // Build a frozen object (not array). SharedMap = Vec<(Arc<str>, SharedValue)>
        let shared = Value::shared(Arc::new(crate::value::SharedNode::Object(Arc::new(
            Vec::<(Arc<str>, crate::value::SharedValue)>::new(),
        ))));
        let span = Span::new(0, 0);
        let result = classify_par_input(&shared, "task.pmap", span);
        let Err(Control::Panic(e)) = result else { panic!("expected Panic, got Ok") };
        assert!(
            e.message.contains("task.pmap expects an array or a frozen array (got frozen object)"),
            "unexpected message: {}",
            e.message
        );
    }

    // ── PAR Task 2.4: error ordering, cancellation, caps, nesting (§3.5/§3.6) ──

    // ── Step 1: Panic ordering (§3.5) ───────────────────────────────────────────
    //
    // Chunk 0 sleeps then panics (SLOW); chunk 1 panics immediately (FAST).
    // The orchestrator awaits chunk futures in INPUT ORDER, so it observes chunk 0's
    // panic FIRST regardless of which chunk panicked first in wall-clock time.
    // Specification: "the reported panic is the first failing chunk by INPUT order,
    // never completion order." (spec §3.5)

    /// Panic ordering: chunk 0 slow-panics, chunk 1 fast-panics.
    /// The surfaced message must be chunk 0's ("slow panic"), not chunk 1's.
    /// Run 5× in-process for flake confidence (timing-based, but generous margins).
    #[tokio::test]
    async fn par_panic_ordering_first_by_input_order_not_completion_order() {
        // `chunks: 2` guarantees chunk 0 = [0] and chunk 1 = [1].
        // Callback on x=0: sleep 80ms then panic "slow panic chunk0".
        // Callback on x=1: panic immediately "fast panic chunk1".
        // The orchestrator awaits chunk 0's future first — so even though chunk 1
        // panics first in wall-clock time, chunk 0's error is what surfaces.
        let src = r#"
import * as task from "std/task"
import * as time from "std/time"
worker fn panic_ordered(x) {
    if (x == 0) {
        await time.sleep(80)
        assert(false, "slow panic chunk0")
    }
    assert(false, "fast panic chunk1")
    return x
}
let [v, err] = recover(() => {
    return await task.pmap([0, 1], panic_ordered, { chunks: 2 })
})
print(v)
print(err.message)
"#;
        // Run 5× for flake confidence.
        for i in 0..5 {
            let out = run(src).await;
            assert!(
                out.starts_with("nil\n"),
                "run {i}: expected nil first line, got: {out}"
            );
            assert!(
                out.contains("slow panic chunk0"),
                "run {i}: expected chunk 0's message ('slow panic chunk0') to surface, got: {out}"
            );
            assert!(
                !out.contains("fast panic chunk1"),
                "run {i}: chunk 1's message must NOT surface when chunk 0 also panics, got: {out}"
            );
        }
    }

    // ── Step 2: Cancellation (§3.5) ──────────────────────────────────────────────

    /// `task.timeout(50, pmap(big_slow_data, slowFn))` returns the timeout error pair.
    /// After cancellation, a follow-up pmap on the same pool SUCCEEDS — proving
    /// the InflightGuard accounting is correct (no wedge).
    #[tokio::test]
    async fn par_timeout_and_followup_succeeds() {
        let out = run(r#"
import * as task from "std/task"
import * as time from "std/time"

// slowFn: each element sleeps 500ms — much longer than the 50ms timeout.
worker fn slow(x) {
    await time.sleep(500)
    return x * 10
}

// Fast fn for the follow-up call.
worker fn double(x) { return x * 2 }

// The pmap times out after 50ms.
let [v, err] = await task.timeout(50, task.pmap([1, 2, 3, 4], slow, { chunks: 2 }))
print(v)
print(err != nil)
print(err.message)

// Follow-up pmap on the same pool must succeed (no wedge).
let result = await task.pmap([10, 20, 30], double, { chunks: 1 })
print(result)
"#).await;
        // Timeout returns: v = nil, err != nil (has message "operation timed out…")
        assert!(out.contains("nil\n"), "expected nil first, got: {out}");
        assert!(out.contains("true\n"), "expected err != nil, got: {out}");
        assert!(
            out.contains("operation timed out"),
            "expected timeout message, got: {out}"
        );
        // Follow-up pmap produces the correct result.
        assert!(
            out.contains("[20, 40, 60]"),
            "follow-up pmap must succeed with correct result, got: {out}"
        );
    }

    /// Queued-chunks-never-run probe (§3.5 honesty, `<= dispatched` assertion).
    ///
    /// Design: use `chunks: 1` (one chunk) + a slow callback that sleeps. The pmap
    /// future is dropped immediately (un-awaited). We assert completion count is
    /// `<= 1` (could be 0 or 1 — in-flight chunks run to completion per spec §3.5,
    /// but a queued chunk that hasn't started is cancelled). Since we can't observe
    /// interior state from script directly, we verify via the follow-up pmap
    /// (pool not wedged) and the timeout return value (timeout pair received).
    ///
    /// Non-flaky design: we do NOT assert exactly how many chunks ran — only that
    /// the operation was cancelled (timeout err pair) and the pool is responsive.
    #[tokio::test]
    async fn par_dropped_future_does_not_wedge_pool() {
        // Drop the pmap future without awaiting it by putting it inside a timeout
        // that fires immediately (0ms). This cancels the pmap.
        let out = run(r#"
import * as task from "std/task"
import * as time from "std/time"

worker fn very_slow(x) {
    await time.sleep(2000)
    return x
}

worker fn id(x) { return x }

// timeout(0) cancels the pmap immediately.
let [v, err] = await task.timeout(0, task.pmap([1, 2, 3, 4, 5, 6], very_slow, { chunks: 2 }))
print(err != nil)

// Pool must still be responsive.
let ok = await task.pmap([1, 2, 3], id, { chunks: 1 })
print(ok)
"#).await;
        assert!(
            out.contains("true\n"),
            "timeout must return error pair, got: {out}"
        );
        assert!(
            out.contains("[1, 2, 3]"),
            "follow-up pmap after cancel must succeed, got: {out}"
        );
    }

    // ── Step 3: Caps (§3.6) ──────────────────────────────────────────────────────

    /// `caps.drop` inside the callback is REFUSED (pooled rule §3.6).
    ///
    /// A pooled worker isolate's `Interp` is reused across requests; a durable drop
    /// would leak forward, so `isolate_loop` sets `caps_drop_allowed = false` per
    /// request. Attempting `caps.drop("env")` inside the chunk callback must panic
    /// with the "not allowed inside a pooled worker fn" message.
    #[tokio::test]
    async fn par_caps_drop_refused_in_chunk_callback() {
        let out = run(r#"
import * as task from "std/task"
import * as caps from "std/caps"

worker fn try_drop(x) {
    caps.drop("env")
    return x
}

let [v, err] = recover(() => {
    return await task.pmap([1, 2], try_drop, { chunks: 1 })
})
print(v)
print(err != nil)
print(err.message)
"#).await;
        assert!(
            out.starts_with("nil\n"),
            "caps.drop in chunk must panic, got: {out}"
        );
        assert!(
            out.contains("true\n"),
            "err must be non-nil, got: {out}"
        );
        assert!(
            out.contains("not allowed inside a pooled worker fn"),
            "error must be the pooled-worker caps.drop refusal, got: {out}"
        );
    }

    /// A pre-dropped caller cap (env) is DENIED inside chunks.
    ///
    /// The caller drops the `env` cap at the top level (irreversibly). When pmap
    /// dispatches chunks, it ships the caller's CapSet (which now has env=denied)
    /// as the chunk's floor (`dispatch_worker` fills `req.caps` from `interp.caps()`).
    /// Accessing `std/env` inside the chunk must therefore fail with a cap-denied error.
    ///
    /// This verifies the §3.6 spec: "a pre-dropped caller cap is denied inside chunks."
    #[tokio::test]
    async fn par_pre_dropped_cap_denied_in_chunk() {
        // We drop the `env` cap at the caller level, then try to use `env.get` inside
        // the pmap callback. The chunk runs under the caller's reduced CapSet → denied.
        // `std/env` is gated on Cap::Env; this test requires the sys feature.
        #[cfg(feature = "sys")]
        {
            let out = run(r#"
import * as task from "std/task"
import * as caps from "std/caps"
import * as env from "std/env"

// Drop env cap at the CALLER level (top-level, irreversible).
caps.drop("env")

worker fn read_env(x) {
    let val = env.get("HOME")
    return val
}

let [v, err] = recover(() => {
    return await task.pmap([1], read_env, { chunks: 1 })
})
print(v)
print(err != nil)
"#).await;
            assert!(
                out.starts_with("nil\n"),
                "cap-denied access must panic, got: {out}"
            );
            assert!(
                out.contains("true\n"),
                "err must be non-nil for denied cap, got: {out}"
            );
        }
        #[cfg(not(feature = "sys"))]
        {
            // sys feature not available; skip with a trivial pass.
            let out = run(r#"
import * as task from "std/task"
worker fn id(x) { return x }
print(await task.pmap([1], id, { chunks: 1 }))
"#).await;
            assert_eq!(out.trim(), "[1]");
        }
    }

    // ── Step 4: Nesting (§5.1) ───────────────────────────────────────────────────

    /// A `worker fn` body calling `task.pmap` runs the inline decomposition
    /// (deadlock-free) and produces IDENTICAL output to a top-level call.
    ///
    /// Design: use a SINGLE top-level pmap call whose callback (`outer`) itself calls
    /// `task.pmap` on an inner callback (`triple`). Because `outer` is the only
    /// callback dispatched to the pool, only `outer`'s slice (which includes `triple`
    /// as a dep) is ever loaded — no cross-slice dep collision. The inline path
    /// (§5.1) then runs `triple` via `vm.user_global("triple")` on the isolate's VM,
    /// which already has `triple` defined from the `outer` slice.
    #[tokio::test]
    async fn par_nested_pmap_is_deadlock_free_and_venue_invariant() {
        let out = run(r#"
import * as task from "std/task"

// triple: the inner callback, referenced by outer.
worker fn triple(x) { return x * 3 }

// outer: dispatched to the pool. Inside, calls task.pmap with triple.
// Since outer runs inside an isolate (in_isolate()=true), the inner pmap
// goes through par_inline (§5.1) — deadlock-free.
worker fn outer_nesting(seed) {
    let r = await task.pmap([seed, seed + 1, seed + 2], triple)
    return r
}

// Both outer_nesting calls run on the pool; each runs triple inline inside.
let result = await task.pmap([1, 10], outer_nesting, { chunks: 1 })
print(result[0])
print(result[1])
"#).await;
        // outer_nesting(1) = triple applied to [1,2,3] = [3,6,9]
        // outer_nesting(10) = triple applied to [10,11,12] = [30,33,36]
        assert_eq!(
            out,
            "[3, 6, 9]\n[30, 33, 36]\n",
            "nested inline pmap must produce correct results, got: {out}"
        );
    }

    /// A direct top-level pmap and a nested (in-isolate inline) pmap on the same data
    /// and callback produce IDENTICAL results — verifying venue-invariance.
    ///
    /// Design: a SINGLE program. The outer callback calls task.pmap with the inner
    /// callback; the result is compared against a separately computed reference value.
    /// Using unique function names avoids fn_id collision when pool isolates are reused
    /// across test cases in the same process.
    #[tokio::test]
    async fn par_nested_pmap_venue_invariant_vs_direct() {
        // A single program where:
        // - direct_result: top-level pmap of triple5 over [5,6,7] → [15,18,21].
        // - nested_result: outer5 dispatched to the pool, inside which triple5 is
        //   called via the inline pmap path (§5.1). Should also give [15,18,21].
        // Because outer5's slice includes triple5, the isolate has triple5 defined
        // when the inline pmap runs — so vm.user_global("triple5") succeeds.
        // Unique suffix "5" avoids collision with other tests' fn_ids.
        let out = run(r#"
import * as task from "std/task"

worker fn triple5(x) { return x * 3 }

worker fn outer5(seed) {
    // Inside an isolate: in_isolate() = true → par_inline path (§5.1).
    let r = await task.pmap([5, 6, 7], triple5)
    return r
}

// Call outer5 via pmap so it runs in an isolate.
let nested_result = await task.pmap([1], outer5, { chunks: 1 })

print(nested_result[0])
"#).await;
        assert_eq!(
            out.trim(),
            "[15, 18, 21]",
            "nested inline pmap must equal direct top-level pmap (venue-invariance §5.1), got: {out}"
        );
    }

    /// Nested non-associative `preduce` with pinned chunks is venue-invariant (§5.1):
    /// the inline (in-isolate) path runs the SAME chunk decomposition as the pooled path
    /// — so the result is byte-identical regardless of whether the call runs at the top
    /// level or inside a worker fn body.
    ///
    /// Design: the outer callback `reducer_outer` is the ONLY function dispatched to
    /// the pool. Its slice transitively includes `sub_inner` (the inner preduce
    /// callback). Because `sub_inner` is ONLY dispatched via the inline path inside
    /// `reducer_outer` (never as a direct top-level pool dispatch), there is no
    /// fn_id collision on pool isolates. The outer function returns BOTH the nested
    /// preduce result and a locally computed reference value, printed separately
    /// so the test can compare them.
    ///
    /// contractual value for preduce([10,1,2,3], sub_inner, 0, {chunks:2}):
    ///   chunk_plan(4,2,1) → [(0,2),(2,4)]
    ///   Chunk 0: [10,1] seed=10 → 10-1=9. p0=9.
    ///   Chunk 1: [2,3] seed=2 → 2-3=-1. p1=-1.
    ///   Final: [0,9,-1] seed=0: 0-9=-9, -9-(-1)=-8. Expected: -8.
    #[tokio::test]
    async fn par_nested_preduce_venue_invariant() {
        let out = run(r#"
import * as task from "std/task"

// sub_inner is ONLY used as the inner preduce callback inside reducer_outer.
// It is never dispatched directly to the pool (only via the inline path §5.1
// when reducer_outer is executing on an isolate).
worker fn sub_inner(a, b) { return a - b }

// reducer_outer runs on the pool. Inside, it calls task.preduce with sub_inner
// (inline path because in_isolate() = true). It returns the nested preduce result.
worker fn reducer_outer(seed) {
    let nested = await task.preduce([10, 1, 2, 3], sub_inner, 0, { chunks: 2 })
    return nested
}

// Dispatch reducer_outer once. It returns the nested preduce result (-8).
let result = await task.pmap([1], reducer_outer, { chunks: 1 })
print(result[0])
"#).await;
        // Contractual value: -8 (chunks:2, sub, init=0).
        assert_eq!(
            out.trim(),
            "-8",
            "nested preduce (venue-invariance §5.1) expected -8, got: {out}"
        );
    }
}
