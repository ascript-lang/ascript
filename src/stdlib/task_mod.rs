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
use crate::value::Value;

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
    ]
}

/// Build a `[value, nil]` ok Result pair.
fn ok_pair(value: Value) -> Value {
    make_pair(value, Value::Nil)
}

/// Build a `[nil, {message}]` error Result pair.
fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
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
            _ => Err(AsError::at(format!("unknown function 'task.{}'", func), span).into()),
        }
    }

    /// `spawn(futureOr0ArgFn) -> future`. A future passes straight through; a
    /// 0-arg function is called now (its async-fn call already returns a future;
    /// a sync return value is wrapped in an already-resolved future).
    async fn task_spawn(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let v = arg(args, 0);
        match v {
            // `spawn` is the explicit opt-out of cancel-on-drop: detach the backing
            // task so it runs to completion (fire-and-forget) regardless of whether
            // the returned handle is awaited or dropped.
            Value::Future(f) => {
                f.detach();
                Ok(Value::Future(f))
            }
            callable @ (Value::Function(_)
            | Value::Builtin(_)
            | Value::BoundMethod(_)
            | Value::NativeMethod(_)) => {
                let r = self.call_value(callable, Vec::new(), span).await?;
                match r {
                    Value::Future(f) => {
                        f.detach();
                        Ok(Value::Future(f))
                    }
                    other => Ok(Value::Future(SharedFuture::resolved(Ok(other)))),
                }
            }
            other => Err(AsError::at(
                format!(
                    "task.spawn expects a future or a 0-argument function, got {}",
                    crate::interp::type_name(&other)
                ),
                span,
            )
            .into()),
        }
    }

    /// `gather([futures]) -> [values]`. Awaits every element in order; non-future
    /// elements are taken as-is. The first error short-circuits.
    async fn task_gather(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let array = want_array(&arg(args, 0), span, "task.gather")?;
        // Snapshot the elements so we don't hold the array borrow across `.await`.
        let items: Vec<Value> = array.borrow().clone();
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Value::Future(f) => out.push(f.get().await?),
                other => out.push(other),
            }
        }
        Ok(Value::Array(std::rc::Rc::new(std::cell::RefCell::new(out))))
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
            match item {
                Value::Future(f) => {
                    let w = winner.clone();
                    let jh = tokio::task::spawn_local(async move {
                        let r = f.get().await;
                        w.resolve(r);
                    });
                    resolver_guards.push(AbortOnDrop(jh.abort_handle()));
                }
                // A non-future element is already "done": it wins instantly.
                other => winner.resolve(Ok(other)),
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
        let fut = match v {
            Value::Future(f) => f,
            // A non-future second arg is already complete: never times out.
            other => return Ok(ok_pair(other)),
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
    /// `Control::Panic` (and only on panic — returned `[nil, err]` pairs are
    /// NOT retried; retry is on Tier-2 panics only), waits
    /// `baseMs * 2^attemptIndex` ms (capped at `opts.maxMs` if given) then
    /// retries. If `opts.jitter` is `true`, adds a uniform random fraction of
    /// the delay (up to +50%). After all attempts fail, re-raises the LAST panic.
    ///
    /// Non-panic errors (`Control::Propagate`, `Control::Exit`) are passed
    /// through immediately without retry.
    async fn task_retry(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let func = arg(args, 0);
        let opts = arg(args, 1);

        // Parse options.
        let (attempts, base_ms, max_ms, jitter) = match &opts {
            Value::Nil => (3usize, 100u64, None::<u64>, false),
            Value::Object(o) => {
                let o = o.borrow();
                let attempts = match o.get("attempts") {
                    Some(v) => {
                        let n = super::want_number(v, span, "task.retry attempts")?;
                        if n < 1.0 || n.fract() != 0.0 {
                            return Err(AsError::at(
                                "task.retry: attempts must be a positive integer",
                                span,
                            )
                            .into());
                        }
                        n as usize
                    }
                    None => 3,
                };
                let base_ms = match o.get("baseMs") {
                    Some(v) => {
                        let n = super::want_number(v, span, "task.retry baseMs")?;
                        if n < 0.0 {
                            return Err(AsError::at(
                                "task.retry: baseMs must be non-negative",
                                span,
                            )
                            .into());
                        }
                        n as u64
                    }
                    None => 100,
                };
                let max_ms = match o.get("maxMs") {
                    Some(v) => {
                        let n = super::want_number(v, span, "task.retry maxMs")?;
                        if n < 0.0 {
                            return Err(AsError::at(
                                "task.retry: maxMs must be non-negative",
                                span,
                            )
                            .into());
                        }
                        Some(n as u64)
                    }
                    None => None,
                };
                let jitter = matches!(o.get("jitter"), Some(Value::Bool(true)));
                (attempts, base_ms, max_ms, jitter)
            }
            other => {
                return Err(AsError::at(
                    format!(
                        "task.retry opts must be an object or nil, got {}",
                        crate::interp::type_name(other)
                    ),
                    span,
                )
                .into());
            }
        };

        let mut last_panic: Option<crate::error::AsError> = None;

        for attempt in 0..attempts {
            // Call the function. If it is an async fn, call_value returns
            // Ok(Value::Future(..)) immediately — we must drive the future to
            // completion by awaiting it before inspecting the result.
            let call_result = self.call_value(func.clone(), vec![], span).await;
            let result = match call_result {
                Ok(Value::Future(f)) => f.get().await,
                other => other,
            };
            match result {
                // Success: return immediately (no retry of ok values or [nil,err] pairs).
                Ok(v) => return Ok(v),
                // Panic: retry if attempts remain.
                Err(Control::Panic(e)) => {
                    last_panic = Some(e);
                    // If this was the last attempt, break to re-raise below.
                    if attempt + 1 >= attempts {
                        break;
                    }
                    // Compute exponential backoff delay.
                    // Cap shift at 62 so 1u64 << shift never overflows.
                    let shift = attempt.min(62) as u32;
                    let multiplier = 1u64 << shift;
                    let delay = base_ms.saturating_mul(multiplier);
                    let mut delay = if let Some(max) = max_ms {
                        delay.min(max)
                    } else {
                        delay
                    };
                    if jitter {
                        // Add up to +50% jitter.
                        let frac = retry_rand_f64();
                        delay = delay.saturating_add(
                            (delay / 2)
                                .saturating_mul((frac * 1000.0) as u64)
                                / 1000,
                        );
                    }
                    if delay > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                }
                // Propagate / Exit: not retryable — pass through unchanged.
                Err(other) => return Err(other),
            }
        }

        // All attempts exhausted — re-raise the last panic.
        Err(Control::Panic(last_panic.expect("at least one attempt")))
    }
}

/// Minimal xorshift64* PRNG for retry jitter. Thread-local, seeded from the
/// system clock. NOT cryptographic — adequate for backoff jitter only.
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
}
