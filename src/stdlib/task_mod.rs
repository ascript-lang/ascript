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
            // PAR §2.1 — Task 2.2 ships pmap orchestration; preduce (Task 2.3) is pending.
            "pmap" => self.task_pmap(args, span).await,
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
