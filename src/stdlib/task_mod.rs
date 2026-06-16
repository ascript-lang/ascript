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
            // PAR §2.1 — Task 2.1 ships validation; orchestration (Task 2.2) is pending.
            "pmap" => self.task_pmap_validate(args, span).await,
            "preduce" => self.task_preduce_validate(args, span).await,
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
        let (attempts, base_ms, max_ms, jitter) = match opts.kind() {
            ValueKind::Nil => (3usize, 100u64, None::<u64>, false),
            ValueKind::Object(o) => {
                let attempts = match o.get("attempts") {
                    Some(v) => {
                        let n = super::want_number(&v, span, "task.retry attempts")?;
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
                        let n = super::want_number(&v, span, "task.retry baseMs")?;
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
                        let n = super::want_number(&v, span, "task.retry maxMs")?;
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
                let jitter = matches!(o.get("jitter").as_ref().map(Value::kind), Some(ValueKind::Bool(true)));
                (attempts, base_ms, max_ms, jitter)
            }
            _ => {
                return Err(AsError::at(
                    format!(
                        "task.retry opts must be an object or nil, got {}",
                        crate::interp::type_name(&opts)
                    ),
                    span,
                )
                .into());
            }
        };

        let mut last_panic: Option<crate::error::AsError> = None;

        for attempt in 0..attempts {
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
                            (delay / 2).saturating_mul((frac * 1000.0) as u64) / 1000,
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

    // ── PAR Task 2.1: validation stubs ───────────────────────────────────────
    // Full orchestration (Task 2.2) is not yet implemented. These methods perform
    // ALL synchronous validation (input classification, callback name check, opts
    // parsing) — which is what Task 2.1 tests — then return an unimplemented error.
    // Task 2.2 will replace the body after the validation block with real dispatch.

    /// `task.pmap(data, f, opts?) -> future<array>` — validation only (Task 2.1).
    async fn task_pmap_validate(
        &self,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        // Step 1: classify input (panics for non-array/non-frozen-array input).
        let _input = classify_par_input(&arg(args, 0), "task.pmap", span)?;
        // Step 2: validate callback (panics for non-named-worker-fn).
        let _entry_name = par_callback_name(&arg(args, 1), "task.pmap", span)?;
        // Step 3: parse opts (panics for invalid opts).
        let (_cap, _min_chunk) = par_opts(&arg(args, 2), "task.pmap", span)?;
        // Orchestration (Task 2.2) not yet implemented.
        Err(AsError::at(
            "task.pmap: orchestration not yet implemented (Task 2.2 pending)",
            span,
        )
        .into())
    }

    /// `task.preduce(data, f, init, opts?) -> future<T>` — validation only (Task 2.1).
    async fn task_preduce_validate(
        &self,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        // Step 1: classify input.
        let _input = classify_par_input(&arg(args, 0), "task.preduce", span)?;
        // Step 2: validate callback.
        let _entry_name = par_callback_name(&arg(args, 1), "task.preduce", span)?;
        // Step 3: init arg (position 2) — sendability checked up front in Task 2.3.
        let _init = arg(args, 2);
        // Step 4: parse opts.
        let (_cap, _min_chunk) = par_opts(&arg(args, 3), "task.preduce", span)?;
        // Orchestration (Task 2.3) not yet implemented.
        Err(AsError::at(
            "task.preduce: orchestration not yet implemented (Task 2.3 pending)",
            span,
        )
        .into())
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
    /// Number of elements in the input. Used by the orchestrator (Task 2.2).
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        match self {
            ParInput::Frozen { len, .. } => *len,
            ParInput::Plain { elems } => elems.len(),
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
}
