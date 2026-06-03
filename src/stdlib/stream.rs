//! `std/stream` — lazy, pull-based streams with a chain of combinator stages.
//!
//! NOT feature-gated: it builds on the core `Value` model, generators (`crate::coro`)
//! and the resource table, all of which are present under `--no-default-features`.
//!
//! A stream is a **source** plus a **chain of transform stages**. Nothing runs until
//! a *terminal* (`collect`/`forEach`/`reduce`/`count`/`find`/`first`) pulls. Each
//! lazy combinator (`map`/`filter`/`take`/`drop`/`flatMap`/`enumerate`/`zip`) is
//! O(1): it clones the prior stream's source + stages, appends one stage, and
//! registers a NEW stream handle. The pull engine ([`Interp::pull_next`]) drives one
//! raw item from the source at a time and threads it through the stages in order,
//! so `f` in `map(s, f)` runs *only* for items a terminal actually pulls — `take(map(
//! range(0, 1e6), f), 3)` calls `f` exactly 3 times.
//!
//! API (all stream args first, as in the rest of the stdlib).
//!
//! Sources:
//!
//!   - `stream.from(x)`           — `x` is an ARRAY (index-pull) or a GENERATOR (resume-pull)
//!   - `stream.range(start, end, step?)` — numeric, exclusive `end`, default `step` 1
//!
//! Lazy combinators (return a NEW stream; no work yet):
//!
//!   - `stream.map(s, fn)`        — `fn(value) -> value`
//!   - `stream.filter(s, fn)`     — keep items where `fn(value)` is truthy
//!   - `stream.take(s, n)`        — first `n` items, then stop (short-circuits the source)
//!   - `stream.drop(s, n)`        — skip the first `n` items
//!   - `stream.flatMap(s, fn)`    — `fn(value) -> array`, flattened one level
//!   - `stream.enumerate(s)`      — `value -> [index, value]`
//!   - `stream.zip(s, t)`         — `[a, b]` pairs, ends when EITHER ends
//!
//! Terminals (drive the pull):
//!
//!   - `stream.collect(s) -> array`
//!   - `stream.forEach(s, fn) -> nil`
//!   - `stream.reduce(s, fn, init) -> value`   — `fn(acc, value) -> acc`
//!   - `stream.count(s) -> number`
//!   - `stream.find(s, fn) -> value | nil`     — first item where `fn` is truthy (short-circuits)
//!   - `stream.first(s) -> value | nil`        — first item (short-circuits)
//!
//! **Single consumption.** Pulling MUTATES the source cursor and per-stage counters
//! (held inside the owned `StreamState`), so a stream is consumed by exactly one
//! terminal. Re-running a terminal on an already-drained stream yields nothing
//! (`collect` → `[]`). Branch with `zip`/separate `from(...)` calls instead.
//!
//! **Borrow discipline.** `pull_next` `take_resource`s the whole `StreamState` OUT of
//! the table, drives it entirely on the stack (so stage `fn`s and a generator
//! source's `resume` are awaited with NO `resources`/`RefCell` borrow held), then
//! `return_resource`s it. `zip`'s nested `pull_next(other_id)` is safe because the
//! other stream is a *different* id and this stream's state is already out.

use super::{arg, bi, want_number};
use crate::coro::GeneratorHandle;
use crate::error::AsError;
use crate::interp::{Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::{NativeKind, Value};
use async_recursion::async_recursion;
use std::collections::VecDeque;
use std::rc::Rc;

/// The source feeding a stream: where raw items come from before any stage runs.
pub enum StreamSource {
    /// A materialized array, pulled by an advancing cursor.
    Array { items: Vec<Value>, cursor: usize },
    /// A numeric range `cur, cur+step, ...` while `< end` (step > 0) / `> end` (step < 0).
    Range { cur: f64, end: f64, step: f64 },
    /// A script generator (`Value::Generator`), pulled via `resume(nil)`.
    Generator(Rc<GeneratorHandle>),
}

/// One transform stage in a stream's chain. Stages that need per-pull state carry
/// it inline (take/drop counters, enumerate index, flatMap buffer) so it survives
/// across pulls (the whole `StreamState` is returned to the table between pulls).
pub enum Stage {
    /// `fn(value) -> value`.
    Map(Value),
    /// keep items where `fn(value)` is truthy.
    Filter(Value),
    /// emit at most `remaining` more items, then end the stream.
    Take { remaining: usize },
    /// skip the next `remaining` items, then pass through.
    Drop { remaining: usize },
    /// `fn(value) -> array`; `buffer` holds the expansion not yet served.
    FlatMap {
        func: Value,
        buffer: VecDeque<Value>,
    },
    /// wrap each item as `[index, value]`, `index` starting at 0.
    Enumerate { index: f64 },
    /// pair each item with the next item pulled from stream `other_id`; ends when
    /// either side ends.
    Zip { other_id: u64 },
}

/// The resource behind a `Value::Native(stream)` handle: a source + an ordered chain
/// of stages. Mutated in place (cursor / stage counters / flatMap buffer) as it is
/// pulled, so a stream is single-consumption.
pub struct StreamState {
    pub source: StreamSource,
    pub stages: Vec<Stage>,
}

/// Deep-clone a source so a new combinator's stream is independent of the prior one
/// up to the (single-consumption) shared generator handle: an array source copies its
/// items + cursor; a range copies its scalars; a generator shares the SAME handle
/// `Rc` (a generator cannot be duplicated — both streams would drive the one body).
fn clone_source(src: &StreamSource) -> StreamSource {
    match src {
        StreamSource::Array { items, cursor } => StreamSource::Array {
            items: items.clone(),
            cursor: *cursor,
        },
        StreamSource::Range { cur, end, step } => StreamSource::Range {
            cur: *cur,
            end: *end,
            step: *step,
        },
        StreamSource::Generator(g) => StreamSource::Generator(g.clone()),
    }
}

/// Clone a stage (including its current runtime counters/buffer) for the
/// derived stream a combinator builds.
fn clone_stage(stage: &Stage) -> Stage {
    match stage {
        Stage::Map(f) => Stage::Map(f.clone()),
        Stage::Filter(f) => Stage::Filter(f.clone()),
        Stage::Take { remaining } => Stage::Take {
            remaining: *remaining,
        },
        Stage::Drop { remaining } => Stage::Drop {
            remaining: *remaining,
        },
        Stage::FlatMap { func, buffer } => Stage::FlatMap {
            func: func.clone(),
            buffer: buffer.clone(),
        },
        Stage::Enumerate { index } => Stage::Enumerate { index: *index },
        Stage::Zip { other_id } => Stage::Zip {
            other_id: *other_id,
        },
    }
}

/// The export list (binding name → builtin) for `import ... from "std/stream"`.
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("from", bi("stream.from")),
        ("range", bi("stream.range")),
        ("map", bi("stream.map")),
        ("filter", bi("stream.filter")),
        ("take", bi("stream.take")),
        ("drop", bi("stream.drop")),
        ("flatMap", bi("stream.flatMap")),
        ("enumerate", bi("stream.enumerate")),
        ("zip", bi("stream.zip")),
        ("collect", bi("stream.collect")),
        ("forEach", bi("stream.forEach")),
        ("reduce", bi("stream.reduce")),
        ("count", bi("stream.count")),
        ("find", bi("stream.find")),
        ("first", bi("stream.first")),
    ]
}

/// Require that `v` is a stream handle, returning its resource id; Tier-2 panic
/// otherwise (argument-type misuse, spec §11.3).
fn require_stream_id(v: &Value, span: Span, ctx: &str) -> Result<u64, Control> {
    match v {
        Value::Native(obj) if obj.kind == NativeKind::Stream => Ok(obj.id),
        _ => Err(AsError::at(
            format!(
                "{} expects a stream, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

/// Require that `v` is callable (function / builtin / bound method / class);
/// Tier-2 panic otherwise. We don't fully type-check here — `call_value` raises a
/// clean "value is not callable" if it isn't — but rejecting obvious non-callables
/// early gives a better message naming the stream op.
fn require_callable(v: &Value, span: Span, ctx: &str) -> Result<Value, Control> {
    match v {
        // `Value::Closure` is the VM's compiled-function value — equally callable
        // via `call_value` (the V4-T5 bridge). The tree-walker never produces a
        // Closure, so this arm is inert there; it only matters for VM programs.
        Value::Function(_)
        | Value::Closure(_)
        | Value::Builtin(_)
        | Value::BoundMethod(_)
        | Value::NativeMethod(_)
        | Value::Class(_)
        | Value::ClassMethod(_, _) => Ok(v.clone()),
        _ => Err(AsError::at(
            format!(
                "{} expects a function, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

impl Interp {
    /// Module-level dispatch for `std/stream`. Every op is async because lazy
    /// combinators that *build* streams are O(1) but terminals (and `pull_next`)
    /// drive user `fn`s / generator `resume`, which are async.
    pub(crate) async fn call_stream(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            // ── sources ──────────────────────────────────────────────────────
            "from" => self.stream_from(args, span),
            "range" => self.stream_range(args, span),
            // ── lazy combinators ─────────────────────────────────────────────
            "map" => self.stream_with_stage(args, span, "stream.map", Stage::Map),
            "filter" => self.stream_with_stage(args, span, "stream.filter", Stage::Filter),
            "flatMap" => self.stream_with_stage(args, span, "stream.flatMap", |f| Stage::FlatMap {
                func: f,
                buffer: VecDeque::new(),
            }),
            "take" => self.stream_take_drop(args, span, true),
            "drop" => self.stream_take_drop(args, span, false),
            "enumerate" => self.stream_enumerate(args, span),
            "zip" => self.stream_zip(args, span),
            // ── terminals ────────────────────────────────────────────────────
            "collect" => self.stream_collect(args, span).await,
            "forEach" => self.stream_for_each(args, span).await,
            "reduce" => self.stream_reduce(args, span).await,
            "count" => self.stream_count(args, span).await,
            "find" => self.stream_find(args, span).await,
            "first" => self.stream_first(args, span).await,
            _ => Err(AsError::at(format!("std/stream has no function '{}'", func), span).into()),
        }
    }

    /// Register a `StreamState` behind a fresh stream handle.
    fn register_stream(&self, state: StreamState) -> Value {
        self.register_resource(
            NativeKind::Stream,
            indexmap::IndexMap::new(),
            ResourceState::Stream(Box::new(state)),
        )
    }

    // ── sources ────────────────────────────────────────────────────────────────

    /// `stream.from(x)` — array (index-pull) or generator (resume-pull) source.
    fn stream_from(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let source = match arg(args, 0) {
            Value::Array(a) => StreamSource::Array {
                items: a.borrow().clone(),
                cursor: 0,
            },
            Value::Generator(g) => StreamSource::Generator(g),
            other => {
                return Err(AsError::at(
                    format!(
                        "stream.from expects an array or generator, got {}",
                        crate::interp::type_name(&other)
                    ),
                    span,
                )
                .into());
            }
        };
        Ok(self.register_stream(StreamState {
            source,
            stages: Vec::new(),
        }))
    }

    /// `stream.range(start, end, step?)` — numeric, exclusive end, default step 1.
    fn stream_range(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let start = want_number(&arg(args, 0), span, "stream.range start")?;
        let end = want_number(&arg(args, 1), span, "stream.range end")?;
        let step = match arg(args, 2) {
            Value::Nil => 1.0,
            v => want_number(&v, span, "stream.range step")?,
        };
        if step == 0.0 {
            return Err(AsError::at("stream.range step must be non-zero", span).into());
        }
        Ok(self.register_stream(StreamState {
            source: StreamSource::Range {
                cur: start,
                end,
                step,
            },
            stages: Vec::new(),
        }))
    }

    // ── lazy combinators ─────────────────────────────────────────────────────

    /// Shared builder for the single-fn combinators (`map`/`filter`/`flatMap`):
    /// take the prior stream's state out, clone source + stages, append the stage
    /// `mk(fn)` produces, register a new stream, and put the prior state back. O(1)
    /// (no item is pulled). The prior stream remains usable as its own handle.
    fn stream_with_stage(
        &self,
        args: &[Value],
        span: Span,
        ctx: &str,
        mk: impl FnOnce(Value) -> Stage,
    ) -> Result<Value, Control> {
        let id = require_stream_id(&arg(args, 0), span, ctx)?;
        let func = require_callable(&arg(args, 1), span, ctx)?;
        let (source, mut stages) = self.snapshot_stream(id, span, ctx)?;
        stages.push(mk(func));
        Ok(self.register_stream(StreamState { source, stages }))
    }

    /// `stream.take(s, n)` / `stream.drop(s, n)` — append a counting stage. `n` is a
    /// non-negative integer count.
    fn stream_take_drop(
        &self,
        args: &[Value],
        span: Span,
        is_take: bool,
    ) -> Result<Value, Control> {
        let ctx = if is_take {
            "stream.take"
        } else {
            "stream.drop"
        };
        let id = require_stream_id(&arg(args, 0), span, ctx)?;
        let n = want_number(&arg(args, 1), span, ctx)?;
        if n < 0.0 || n.fract() != 0.0 {
            return Err(AsError::at(
                format!("{} count must be a non-negative integer", ctx),
                span,
            )
            .into());
        }
        let n = n as usize;
        let (source, mut stages) = self.snapshot_stream(id, span, ctx)?;
        stages.push(if is_take {
            Stage::Take { remaining: n }
        } else {
            Stage::Drop { remaining: n }
        });
        Ok(self.register_stream(StreamState { source, stages }))
    }

    /// `stream.enumerate(s)` — append an indexing stage (`value -> [index, value]`).
    fn stream_enumerate(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_stream_id(&arg(args, 0), span, "stream.enumerate")?;
        let (source, mut stages) = self.snapshot_stream(id, span, "stream.enumerate")?;
        stages.push(Stage::Enumerate { index: 0.0 });
        Ok(self.register_stream(StreamState { source, stages }))
    }

    /// `stream.zip(s, t)` — append a stage pairing `s`'s items with `t`'s. `t` is
    /// consumed lazily by the pull (its handle id is stored); the pairing ends when
    /// either side ends.
    fn stream_zip(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_stream_id(&arg(args, 0), span, "stream.zip")?;
        let other_id = require_stream_id(&arg(args, 1), span, "stream.zip")?;
        if id == other_id {
            return Err(AsError::at("stream.zip cannot zip a stream with itself", span).into());
        }
        let (source, mut stages) = self.snapshot_stream(id, span, "stream.zip")?;
        stages.push(Stage::Zip { other_id });
        Ok(self.register_stream(StreamState { source, stages }))
    }

    /// Take the stream `id` out, clone its (source, stages), put it back, and hand
    /// the clone to the caller. The clone is what a combinator extends; the original
    /// handle is untouched so it stays independently consumable.
    fn snapshot_stream(
        &self,
        id: u64,
        span: Span,
        ctx: &str,
    ) -> Result<(StreamSource, Vec<Stage>), Control> {
        match self.take_resource(id) {
            Some(ResourceState::Stream(state)) => {
                let source = clone_source(&state.source);
                let stages = state.stages.iter().map(clone_stage).collect();
                // Put the original back so its own handle remains usable.
                self.return_resource(id, ResourceState::Stream(state));
                Ok((source, stages))
            }
            Some(other) => {
                // Not a stream after all — restore whatever it was and panic.
                self.return_resource(id, other);
                Err(AsError::at(format!("{} expects a stream", ctx), span).into())
            }
            None => Err(AsError::at(format!("{}: stream is closed", ctx), span).into()),
        }
    }

    // ── the pull engine ──────────────────────────────────────────────────────

    /// Drive the stream one item forward: pull a raw item from the source and thread
    /// it through every stage in order. `Ok(Some(v))` is the next produced value,
    /// `Ok(None)` is end-of-stream, `Err(c)` propagates a stage `fn` / generator
    /// panic. Mutates the source cursor + stage counters + flatMap buffer in place.
    ///
    /// Borrow safety: the whole `StreamState` is `take_resource`d OUT before any
    /// `.await` (stage fns, generator `resume`, nested `zip` pull) and
    /// `return_resource`d after — no `resources` / `RefCell` borrow is ever held
    /// across an await. `zip`'s nested `pull_next(other_id)` operates on a different
    /// id while this stream's state is on the stack, so there is no re-entrancy clash.
    #[async_recursion(?Send)]
    pub(crate) async fn pull_next(&self, id: u64, span: Span) -> Result<Option<Value>, Control> {
        // Take the whole state out so nothing is borrowed across the awaits below.
        let mut state = match self.take_resource(id) {
            Some(ResourceState::Stream(s)) => s,
            Some(other) => {
                self.return_resource(id, other);
                return Err(AsError::at("stream pull on a non-stream handle", span).into());
            }
            // Already drained/closed: end of stream.
            None => return Ok(None),
        };

        // Drive to the next produced value (or end), threading source items through
        // stages. We loop because Filter/Drop can reject an item, requiring another
        // source pull, and FlatMap may need to refill its buffer.
        let result = self.drive(&mut state, span).await;

        // Always restore the state (mutations to cursor/counters/buffer persist).
        self.return_resource(id, ResourceState::Stream(state));
        result
    }

    /// The core pull loop, operating on an owned `StreamState`. Separated from
    /// `pull_next` so the state is restored by the caller on every exit path.
    #[async_recursion(?Send)]
    async fn drive(&self, state: &mut StreamState, span: Span) -> Result<Option<Value>, Control> {
        'outer: loop {
            // 0) Short-circuit: if any Take stage is exhausted the stream is over.
            //    Checked BEFORE pulling/serving an item so upstream work (the source,
            //    a `map` fn) does NOT run for an item Take would only discard — this
            //    is what makes `take(map(range(0, 1e6), f), 3)` call `f` exactly 3
            //    times. (A Take stage decrements when it passes an item, so once it
            //    hits 0 no further item should be produced at all.)
            if state
                .stages
                .iter()
                .any(|s| matches!(s, Stage::Take { remaining: 0 }))
            {
                return Ok(None);
            }

            // 1) Serve any value already buffered by a FlatMap stage (and run the
            //    *downstream* stages on it). If a FlatMap has a non-empty buffer we
            //    must not pull a fresh source item yet.
            if let Some((flat_idx, value)) = take_buffered(state) {
                match self
                    .run_stages_from(state, flat_idx + 1, value, span)
                    .await?
                {
                    StageOutcome::Emit(v) => return Ok(Some(v)),
                    StageOutcome::Skip => continue 'outer,
                    StageOutcome::End => return Ok(None),
                }
            }

            // 2) Pull the next raw item from the source.
            let raw = match self.next_source(state, span).await? {
                Some(v) => v,
                None => return Ok(None), // source exhausted
            };

            // 3) Thread it through all stages from the top.
            match self.run_stages_from(state, 0, raw, span).await? {
                StageOutcome::Emit(v) => return Ok(Some(v)),
                StageOutcome::Skip => continue 'outer,
                StageOutcome::End => return Ok(None),
            }
        }
    }

    /// Pull the next raw item from the source, advancing its cursor. `Ok(None)` =
    /// source exhausted. The generator source `resume` is awaited with the state
    /// owned on the stack (no borrow held).
    async fn next_source(
        &self,
        state: &mut StreamState,
        _span: Span,
    ) -> Result<Option<Value>, Control> {
        match &mut state.source {
            StreamSource::Array { items, cursor } => {
                if *cursor < items.len() {
                    let v = items[*cursor].clone();
                    *cursor += 1;
                    Ok(Some(v))
                } else {
                    Ok(None)
                }
            }
            StreamSource::Range { cur, end, step } => {
                let more = if *step > 0.0 {
                    *cur < *end
                } else {
                    *cur > *end
                };
                if more {
                    let v = *cur;
                    *cur += *step;
                    Ok(Some(Value::Number(v)))
                } else {
                    Ok(None)
                }
            }
            StreamSource::Generator(g) => {
                // resume is async; the state is owned on the stack, no borrow held.
                let g = g.clone();
                match g.resume(Value::Nil).await? {
                    Some(v) => Ok(Some(v)),
                    None => Ok(None),
                }
            }
        }
    }

    /// Thread `value` through `state.stages[from..]` in order. Returns whether to
    /// `Emit` the final value, `Skip` it (a Filter rejected it / a Drop swallowed it
    /// / a FlatMap buffered its expansion), or `End` the whole stream (a Take hit 0 /
    /// a Zip partner ended).
    #[async_recursion(?Send)]
    async fn run_stages_from(
        &self,
        state: &mut StreamState,
        from: usize,
        mut value: Value,
        span: Span,
    ) -> Result<StageOutcome, Control> {
        let n = state.stages.len();
        let mut i = from;
        while i < n {
            match &mut state.stages[i] {
                Stage::Map(func) => {
                    let func = func.clone();
                    value = self.call_value(func, vec![value], span).await?;
                }
                Stage::Filter(func) => {
                    let func = func.clone();
                    let keep = self.call_value(func, vec![value.clone()], span).await?;
                    if !keep.is_truthy() {
                        return Ok(StageOutcome::Skip);
                    }
                }
                Stage::Take { remaining } => {
                    if *remaining == 0 {
                        return Ok(StageOutcome::End);
                    }
                    *remaining -= 1;
                }
                Stage::Drop { remaining } => {
                    if *remaining > 0 {
                        *remaining -= 1;
                        return Ok(StageOutcome::Skip);
                    }
                }
                Stage::FlatMap { func, .. } => {
                    let func = func.clone();
                    let out = self.call_value(func, vec![value.clone()], span).await?;
                    let items = match out {
                        Value::Array(a) => a.borrow().clone(),
                        other => {
                            return Err(AsError::at(
                                format!(
                                    "stream.flatMap callback must return an array, got {}",
                                    crate::interp::type_name(&other)
                                ),
                                span,
                            )
                            .into());
                        }
                    };
                    // Buffer the expansion; `drive` serves it through the downstream
                    // stages one element at a time.
                    if let Stage::FlatMap { buffer, .. } = &mut state.stages[i] {
                        buffer.extend(items);
                    }
                    // Nothing to emit from THIS source item directly; let `drive`
                    // re-enter and drain the buffer (which runs stages i+1.. ).
                    return Ok(StageOutcome::Skip);
                }
                Stage::Enumerate { index } => {
                    let idx = *index;
                    *index += 1.0;
                    value = Value::Array(Rc::new(std::cell::RefCell::new(vec![
                        Value::Number(idx),
                        value,
                    ])));
                }
                Stage::Zip { other_id } => {
                    let other_id = *other_id;
                    // Pull one item from the partner stream. If it has ended, the
                    // zipped stream ends too (the already-pulled `value` is dropped).
                    match self.pull_next(other_id, span).await? {
                        Some(partner) => {
                            value = Value::Array(Rc::new(std::cell::RefCell::new(vec![
                                value, partner,
                            ])));
                        }
                        None => return Ok(StageOutcome::End),
                    }
                }
            }
            i += 1;
        }
        Ok(StageOutcome::Emit(value))
    }

    // ── terminals ──────────────────────────────────────────────────────────────

    /// `stream.collect(s)` — drain the stream into an array.
    async fn stream_collect(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_stream_id(&arg(args, 0), span, "stream.collect")?;
        let mut out = Vec::new();
        while let Some(v) = self.pull_next(id, span).await? {
            out.push(v);
        }
        Ok(Value::Array(Rc::new(std::cell::RefCell::new(out))))
    }

    /// `stream.forEach(s, fn)` — pull every item and call `fn(value)` for its effect.
    async fn stream_for_each(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_stream_id(&arg(args, 0), span, "stream.forEach")?;
        let func = require_callable(&arg(args, 1), span, "stream.forEach")?;
        while let Some(v) = self.pull_next(id, span).await? {
            self.call_value(func.clone(), vec![v], span).await?;
        }
        Ok(Value::Nil)
    }

    /// `stream.reduce(s, fn, init)` — fold with `fn(acc, value) -> acc`.
    async fn stream_reduce(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_stream_id(&arg(args, 0), span, "stream.reduce")?;
        let func = require_callable(&arg(args, 1), span, "stream.reduce")?;
        let mut acc = arg(args, 2);
        while let Some(v) = self.pull_next(id, span).await? {
            acc = self.call_value(func.clone(), vec![acc, v], span).await?;
        }
        Ok(acc)
    }

    /// `stream.count(s)` — number of items the stream produces.
    async fn stream_count(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_stream_id(&arg(args, 0), span, "stream.count")?;
        let mut n = 0.0;
        while self.pull_next(id, span).await?.is_some() {
            n += 1.0;
        }
        Ok(Value::Number(n))
    }

    /// `stream.find(s, fn)` — first item where `fn(value)` is truthy, else `nil`.
    /// Short-circuits: stops pulling at the first match.
    async fn stream_find(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_stream_id(&arg(args, 0), span, "stream.find")?;
        let func = require_callable(&arg(args, 1), span, "stream.find")?;
        while let Some(v) = self.pull_next(id, span).await? {
            let hit = self.call_value(func.clone(), vec![v.clone()], span).await?;
            if hit.is_truthy() {
                return Ok(v);
            }
        }
        Ok(Value::Nil)
    }

    /// `stream.first(s)` — the first produced item, or `nil` if empty.
    /// Short-circuits: pulls exactly one item.
    async fn stream_first(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let id = require_stream_id(&arg(args, 0), span, "stream.first")?;
        Ok(self.pull_next(id, span).await?.unwrap_or(Value::Nil))
    }
}

/// What threading a value through a chain of stages yielded.
enum StageOutcome {
    /// Produce this value to the consumer.
    Emit(Value),
    /// Reject/swallow this item — pull another from the source.
    Skip,
    /// End the whole stream now.
    End,
}

/// If any FlatMap stage has a buffered expansion, pop the FIRST (earliest in the
/// chain) such buffer's front element and return it alongside that stage's index, so
/// `drive` can run the *downstream* stages on it. Returns `None` when no buffer holds
/// a value. Picking the earliest stage keeps chained flatMaps correctly ordered.
fn take_buffered(state: &mut StreamState) -> Option<(usize, Value)> {
    for (i, stage) in state.stages.iter_mut().enumerate() {
        if let Stage::FlatMap { buffer, .. } = stage {
            if let Some(v) = buffer.pop_front() {
                return Some((i, v));
            }
        }
    }
    None
}

// ── tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    #[tokio::test]
    async fn map_doubles() {
        let out = run(r#"
import { from, map, collect } from "std/stream"
print(await collect(map(from([1, 2, 3]), (x) => x * 2)))
"#)
        .await;
        assert_eq!(out, "[2, 4, 6]\n");
    }

    #[tokio::test]
    async fn laziness_take_map_range_runs_fn_only_for_pulled_items() {
        let out = run(r#"
import { from, range, map, take, collect } from "std/stream"
let c = [0]
let s = take(map(range(0, 1000000), (x) => { c[0] = c[0] + 1; return x }), 3)
await collect(s)
print(c[0])
"#)
        .await;
        assert_eq!(out, "3\n");
    }

    #[tokio::test]
    async fn filter_keeps_truthy() {
        let out = run(r#"
import { from, filter, collect } from "std/stream"
print(await collect(filter(from([1, 2, 3, 4, 5]), (x) => x % 2 == 0)))
"#)
        .await;
        assert_eq!(out, "[2, 4]\n");
    }

    #[tokio::test]
    async fn drop_skips_prefix() {
        let out = run(r#"
import { from, drop, collect } from "std/stream"
print(await collect(drop(from([1, 2, 3, 4, 5]), 2)))
"#)
        .await;
        assert_eq!(out, "[3, 4, 5]\n");
    }

    #[tokio::test]
    async fn flat_map_flattens_one_level() {
        let out = run(r#"
import { from, flatMap, collect } from "std/stream"
print(await collect(flatMap(from([1, 2]), (x) => [x, x])))
"#)
        .await;
        assert_eq!(out, "[1, 1, 2, 2]\n");
    }

    #[tokio::test]
    async fn enumerate_pairs_index_value() {
        let out = run(r#"
import { from, enumerate, collect } from "std/stream"
print(await collect(enumerate(from([10, 20]))))
"#)
        .await;
        assert_eq!(out, "[[0, 10], [1, 20]]\n");
    }

    #[tokio::test]
    async fn zip_pairs_until_shorter_ends() {
        let out = run(r#"
import { from, zip, collect } from "std/stream"
print(await collect(zip(from([1, 2]), from([3, 4, 5]))))
"#)
        .await;
        assert_eq!(out, "[[1, 3], [2, 4]]\n");
    }

    #[tokio::test]
    async fn reduce_sums() {
        let out = run(r#"
import { from, reduce } from "std/stream"
print(await reduce(from([1, 2, 3]), (a, b) => a + b, 0))
"#)
        .await;
        assert_eq!(out, "6\n");
    }

    #[tokio::test]
    async fn count_counts() {
        let out = run(r#"
import { from, filter, count } from "std/stream"
print(await count(filter(from([1, 2, 3, 4]), (x) => x > 1)))
"#)
        .await;
        assert_eq!(out, "3\n");
    }

    #[tokio::test]
    async fn find_returns_first_match() {
        let out = run(r#"
import { from, find } from "std/stream"
print(await find(from([1, 2, 3, 4]), (x) => x > 2))
"#)
        .await;
        assert_eq!(out, "3\n");
    }

    #[tokio::test]
    async fn find_none_returns_nil() {
        let out = run(r#"
import { from, find } from "std/stream"
print(await find(from([1, 2]), (x) => x > 9))
"#)
        .await;
        assert_eq!(out, "nil\n");
    }

    #[tokio::test]
    async fn first_returns_head_then_nil() {
        let out = run(r#"
import { from, first } from "std/stream"
print(await first(from([7, 8, 9])))
print(await first(from([])))
"#)
        .await;
        assert_eq!(out, "7\nnil\n");
    }

    #[tokio::test]
    async fn generator_source_collects() {
        let out = run(r#"
import { from, collect } from "std/stream"
async fn* g() { yield 1; yield 2; yield 3 }
print(await collect(from(g())))
"#)
        .await;
        assert_eq!(out, "[1, 2, 3]\n");
    }

    #[tokio::test]
    async fn generator_source_lazy_with_map_take() {
        // take(map(generator)) must drive the generator only as far as needed.
        let out = run(r#"
import { from, map, take, collect } from "std/stream"
fn* nats() {
    let i = 0
    while (true) { yield i; i = i + 1 }
}
print(await collect(take(map(from(nats()), (x) => x * 10), 3)))
"#)
        .await;
        assert_eq!(out, "[0, 10, 20]\n");
    }

    #[tokio::test]
    async fn range_with_step() {
        let out = run(r#"
import { range, collect } from "std/stream"
print(await collect(range(0, 10, 3)))
"#)
        .await;
        assert_eq!(out, "[0, 3, 6, 9]\n");
    }

    #[tokio::test]
    async fn chained_flat_map_then_filter() {
        let out = run(r#"
import { from, flatMap, filter, collect } from "std/stream"
let s = filter(flatMap(from([1, 2, 3]), (x) => [x, x * 10]), (v) => v > 5)
print(await collect(s))
"#)
        .await;
        // 1->[1,10], 2->[2,20], 3->[3,30]; keep >5: 10, 20, 30
        assert_eq!(out, "[10, 20, 30]\n");
    }

    #[tokio::test]
    async fn single_consumption_recollect_is_empty() {
        let out = run(r#"
import { from, collect } from "std/stream"
let s = from([1, 2, 3])
print(await collect(s))
print(await collect(s))
"#)
        .await;
        assert_eq!(out, "[1, 2, 3]\n[]\n");
    }

    #[tokio::test]
    async fn zip_with_self_is_tier2_panic() {
        // `zip(s, s)` would drive one mutable source from both sides — rejected at
        // build time (the guard in `stream_zip`). A Tier-2 panic surfaces as an Err
        // from `run_source`.
        let result = crate::run_source(
            r#"
import { from, zip, collect } from "std/stream"
let s = from([1, 2])
await collect(zip(s, s))
"#,
        )
        .await;
        assert!(result.is_err(), "expected Tier-2 panic, got: {:?}", result);
    }

    #[tokio::test]
    async fn range_negative_step_counts_down() {
        let out = run(r#"
import { range, collect } from "std/stream"
print(await collect(range(10, 0, -2)))
"#)
        .await;
        assert_eq!(out, "[10, 8, 6, 4, 2]\n");
    }

    #[tokio::test]
    async fn find_short_circuits_pulling_only_through_the_match() {
        // Mirror the take-laziness test: a counter-incrementing map over a huge
        // range, then `find(x == 5)`. The map must run only for items pulled up to
        // (and including) the match — 0..=5 → 6 calls — proving find stops early.
        let out = run(r#"
import { from, range, map, find } from "std/stream"
let c = [0]
let s = map(range(0, 1000000), (x) => { c[0] = c[0] + 1; return x })
print(await find(s, (x) => x == 5))
print(c[0])
"#)
        .await;
        assert_eq!(out, "5\n6\n");
    }
}
