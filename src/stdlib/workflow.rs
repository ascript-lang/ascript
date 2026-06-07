//! `std/workflow` — durable execution via event-sourced deterministic replay
//! (SP9 §2). The Temporal/Restate/Cloudflare model, NOT continuation serialization
//! (architecturally impossible with live native handles — see spec §2.1).
//!
//! A **workflow** is deterministic AScript code: control flow plus calls to
//! **activities** through the workflow **ctx**. Non-deterministic effects (I/O, time,
//! randomness) happen ONLY inside activities; the engine persists an append-only
//! **event log** of every activity's *result* (a `Value` serialized via
//! `json::to_json_lossy`). On `resume` after a crash, the workflow code re-runs from
//! the top, but each `ctx.call`/`ctx.now`/`ctx.random` returns its recorded result
//! from the log instead of re-executing — so the workflow deterministically
//! fast-forwards to where it left off. The continuation is reconstructed by replay,
//! never serialized; workflow code runs on an ordinary stack (no model-2b VM).
//!
//! Built on the SP9 §3 [`crate::det::DeterminismContext`]: `workflow.run` enters
//! Record mode, `workflow.resume` enters Replay mode primed from the log.
//!
//! Surface (tagged Objects, no new `Value` variant — mirrors `std/schema`):
//! - `activity(name, fn)` → `{__kind:"activity", name, fn}`.
//! - the `ctx` passed to a workflow body → `{__kind:"workflow_ctx"}`; its methods
//!   (`call`/`now`/`random`/`uuid`/`sleep`) are intercepted at the call site (the
//!   same hook `std/schema` uses) and routed to [`crate::interp::Interp`].
//! - `run(wf, input, {log})` / `resume(wf, input, {log})` drive the workflow.
//!
//! Feature-gated on `workflow` (depends on `data` for the JSON log); under
//! `--no-default-features` the whole module is compiled out and `import
//! "std/workflow"` is an unknown-module error (symmetric on both engines).

use crate::det::{DetEvent, DeterminismContext, Mode};
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

/// The tag marking a workflow-context Object. The call-site hook in the evaluators
/// checks this to route `ctx.<method>(...)`.
pub const CTX_KIND: &str = "workflow_ctx";
/// The tag marking an activity Object.
pub const ACTIVITY_KIND: &str = "activity";

/// `std/workflow` exports brought in by `import`.
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("activity", super::bi("workflow.activity")),
        ("run", super::bi("workflow.run")),
        ("resume", super::bi("workflow.resume")),
    ]
}

/// Build a tagged activity Object `{__kind:"activity", name, fn}`.
pub fn make_activity(name: String, func: Value) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("__kind".to_string(), Value::Str(ACTIVITY_KIND.into()));
    m.insert("name".to_string(), Value::Str(name.into()));
    m.insert("fn".to_string(), func);
    Value::Object(crate::value::ObjectCell::new(m))
}

/// Build the tagged workflow-context Object passed to a workflow body. It carries no
/// per-instance state (the recorded event stream lives in the `Interp`'s
/// `DeterminismContext`); the tag is what the call-site hook dispatches on.
pub fn make_ctx() -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("__kind".to_string(), Value::Str(CTX_KIND.into()));
    Value::Object(crate::value::ObjectCell::new(m))
}

/// True iff `v` is the workflow-context tagged Object.
pub fn is_ctx_value(v: &Value) -> bool {
    tagged_kind(v) == Some(CTX_KIND)
}

/// True iff `v` is an activity tagged Object.
pub fn is_activity_value(v: &Value) -> bool {
    tagged_kind(v) == Some(ACTIVITY_KIND)
}

/// The methods callable on a `ctx` (the call-site hook routes these to
/// `Interp::call_workflow_ctx`). A bare `ctx.now` (member read, no call) is NOT a
/// method — it reads the (absent) field, returning nil — mirroring schema's
/// call-position-only limitation.
pub fn is_ctx_method(name: &str) -> bool {
    matches!(name, "call" | "now" | "random" | "uuid" | "sleep")
}

/// Read the `__kind` tag of an Object value, if it is one.
fn tagged_kind(v: &Value) -> Option<&'static str> {
    if let Value::Object(o) = v {
        if let Some(Value::Str(k)) = o.borrow().get("__kind") {
            return match &**k {
                CTX_KIND => Some(CTX_KIND),
                ACTIVITY_KIND => Some(ACTIVITY_KIND),
                _ => None,
            };
        }
    }
    None
}

/// Serialize a `Value` to a JSON string for the event log via the total
/// `to_json_lossy` codec (the same `std/log` relies on — cycles→`"[Circular]"`,
/// NaN→null, never panics).
pub fn to_json_string(v: &Value) -> String {
    crate::stdlib::json::to_json_lossy(v, &mut Vec::new()).to_string()
}

/// Parse a JSON string from the event log back into a `Value`.
pub fn from_json_string(s: &str) -> Value {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(jv) => crate::stdlib::json::to_ascript(&jv),
        Err(_) => Value::Nil,
    }
}

/// A stable hash of an activity's name + JSON-serialized args, used to pin the call
/// signature so a workflow-code change that reorders effects is caught as a
/// non-determinism error rather than silently replaying a wrong value.
pub fn signature_hash(name: &str, args: &[Value]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut h);
    for a in args {
        to_json_string(a).hash(&mut h);
    }
    h.finish()
}

/// Serialize the in-memory `DetEvent` stream to the newline-delimited JSON log
/// format (spec §2.3): one record per line, `seq`-ordered. Reuses `serde_json` (the
/// `data` feature the `workflow` feature depends on).
fn events_to_log(events: &[DetEvent]) -> String {
    let mut out = String::new();
    for (seq, ev) in events.iter().enumerate() {
        let rec = event_to_json(seq, ev);
        out.push_str(&rec.to_string());
        out.push('\n');
    }
    out
}

/// One `DetEvent` → a JSON log record.
fn event_to_json(seq: usize, ev: &DetEvent) -> serde_json::Value {
    use serde_json::json;
    match ev {
        DetEvent::ClockRead { value } => json!({"seq": seq, "kind": "ClockRead", "value": value}),
        DetEvent::MonotonicRead { value } => {
            json!({"seq": seq, "kind": "MonotonicRead", "value": value})
        }
        DetEvent::RandomRead { value } => json!({"seq": seq, "kind": "RandomRead", "value": value}),
        DetEvent::TimerSet { wake } => json!({"seq": seq, "kind": "TimerSet", "wake": wake}),
        DetEvent::ActivityCompleted {
            name,
            args_hash,
            result_json,
        } => json!({
            "seq": seq,
            "kind": "ActivityCompleted",
            "name": name,
            "argsHash": args_hash.to_string(),
            // The activity result is itself JSON; embed it as a parsed value so the
            // log is human-inspectable, falling back to the raw string.
            "result": serde_json::from_str::<serde_json::Value>(result_json)
                .unwrap_or(serde_json::Value::Null),
        }),
        // Workers Spec B (Task 12): cross-isolate boundary events. The structured-
        // clone bytes are stored as a JSON number array (no new base64 dep — the
        // `workflow` feature only pulls `data`/`serde_json`).
        DetEvent::ActorCall {
            method,
            result,
            panic,
        } => json!({
            "seq": seq,
            "kind": "ActorCall",
            "method": method,
            "result": result,
            "panic": panic,
        }),
        DetEvent::GeneratorYield { value, panic } => json!({
            "seq": seq,
            "kind": "GeneratorYield",
            "value": value,
            "panic": panic,
        }),
    }
}

/// Parse a JSON value that is a number array of bytes back into `Vec<u8>` (the
/// boundary-event byte encoding). A non-array / out-of-range entry yields an empty
/// vec (best-effort; the runtime replay-mismatch guard is authoritative).
fn json_bytes(v: Option<&serde_json::Value>) -> Vec<u8> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|n| n.as_u64())
                .map(|n| n as u8)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the newline-JSON log back into a `DetEvent` stream (resume). A malformed
/// line is skipped (best-effort: the runtime replay-mismatch detector is the
/// authoritative guard).
fn log_to_events(text: &str) -> Vec<DetEvent> {
    let mut events = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = rec.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        match kind {
            "ClockRead" => {
                if let Some(v) = rec.get("value").and_then(|v| v.as_f64()) {
                    events.push(DetEvent::ClockRead { value: v });
                }
            }
            "MonotonicRead" => {
                if let Some(v) = rec.get("value").and_then(|v| v.as_f64()) {
                    events.push(DetEvent::MonotonicRead { value: v });
                }
            }
            "RandomRead" => {
                if let Some(v) = rec.get("value").and_then(|v| v.as_f64()) {
                    events.push(DetEvent::RandomRead { value: v });
                }
            }
            "TimerSet" => {
                if let Some(w) = rec.get("wake").and_then(|v| v.as_f64()) {
                    events.push(DetEvent::TimerSet { wake: w });
                }
            }
            "ActivityCompleted" => {
                let name = rec
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args_hash = rec
                    .get("argsHash")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let result_json = rec
                    .get("result")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string());
                events.push(DetEvent::ActivityCompleted {
                    name,
                    args_hash,
                    result_json,
                });
            }
            "ActorCall" => {
                let method = rec
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let result = json_bytes(rec.get("result"));
                let panic = rec
                    .get("panic")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                events.push(DetEvent::ActorCall {
                    method,
                    result,
                    panic,
                });
            }
            "GeneratorYield" => {
                let value = rec
                    .get("value")
                    .filter(|v| !v.is_null())
                    .map(|v| json_bytes(Some(v)));
                let panic = rec
                    .get("panic")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                events.push(DetEvent::GeneratorYield { value, panic });
            }
            _ => {}
        }
    }
    events
}

/// Read `{log: "path", durability?: "fsync"|"buffered"}` from a workflow options
/// Object, returning `(log_path, fsync)`. `log` is required.
fn read_options(opts: &Value, span: Span) -> Result<(String, bool), Control> {
    let Value::Object(o) = opts else {
        return Err(AsError::at(
            "workflow: options must be an object with a `log` path",
            span,
        )
        .into());
    };
    let m = o.borrow();
    let log = match m.get("log") {
        Some(Value::Str(s)) => s.to_string(),
        _ => {
            return Err(AsError::at(
                "workflow: options.log must be a string file path",
                span,
            )
            .into())
        }
    };
    let fsync = !matches!(m.get("durability"), Some(Value::Str(s)) if &**s == "buffered");
    Ok((log, fsync))
}

impl Interp {
    /// `std/workflow` dispatch (the `workflow.*` builtins). All three entry points
    /// take `&self` so they reach the per-`Interp` determinism context + can call
    /// user closures via `call_value`.
    pub(crate) async fn call_workflow(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            // `activity(name, fn)` → a tagged Object wrapping the side-effecting fn.
            // A bare `activity(...)` outside a workflow is just a callable record;
            // recording happens only via `ctx.call`.
            "activity" => {
                let name = match args.first() {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => {
                        return Err(AsError::at(
                            "workflow.activity(name, fn): name must be a string",
                            span,
                        )
                        .into())
                    }
                };
                let func_val = args.get(1).cloned().unwrap_or(Value::Nil);
                if !is_callable(&func_val) {
                    return Err(AsError::at(
                        "workflow.activity(name, fn): fn must be a function",
                        span,
                    )
                    .into());
                }
                Ok(make_activity(name, func_val))
            }
            "run" => self.workflow_run(args, span, /*replay=*/ false).await,
            "resume" => self.workflow_run(args, span, /*replay=*/ true).await,
            _ => Err(AsError::at(format!("std/workflow has no function '{}'", func), span).into()),
        }
    }

    /// Shared `run`/`resume` driver. `replay=false` (run): enter Record mode with a
    /// fresh log. `replay=true` (resume): if the log ends in a completion, return the
    /// recorded result (idempotent); else enter Replay mode primed from the log and
    /// re-run the workflow, fast-forwarding through recorded effects and recording any
    /// new ones from the crash point.
    async fn workflow_run(
        &self,
        args: &[Value],
        span: Span,
        replay: bool,
    ) -> Result<Value, Control> {
        let wf = args.first().cloned().unwrap_or(Value::Nil);
        if !is_callable(&wf) {
            return Err(AsError::at("workflow.run/resume: first arg must be a function", span).into());
        }
        let input = args.get(1).cloned().unwrap_or(Value::Nil);
        let opts = args.get(2).cloned().unwrap_or(Value::Nil);
        let (log_path, fsync) = read_options(&opts, span)?;

        // Seed the determinism context from the log path so a record/resume pair on
        // the same log uses the same RNG seed (deterministic across the boundary).
        let seed = crate::det::deterministic_start_ms(log_path_seed(&log_path)) as u64;
        let start_ms = crate::interp::real_now_ms();

        if replay {
            // Read the existing log. If it does not exist yet, resume behaves like a
            // fresh run (record from the top).
            let existing = std::fs::read_to_string(&log_path).unwrap_or_default();
            let recorded = log_to_events(&existing);
            // Idempotent completion check: if the log already holds a recorded
            // WorkflowCompleted result line, return it without re-running.
            if let Some(result) = completed_result(&existing) {
                return Ok(result);
            }
            let ctx = DeterminismContext::replay(seed, start_ms, recorded);
            let prev = self.install_determinism(ctx);
            let outcome = self.drive_workflow(wf, input, span).await;
            self.finish_workflow(outcome, &log_path, fsync, prev, span)
                .await
        } else {
            let ctx = DeterminismContext::record(seed, start_ms);
            let prev = self.install_determinism(ctx);
            let outcome = self.drive_workflow(wf, input, span).await;
            self.finish_workflow(outcome, &log_path, fsync, prev, span)
                .await
        }
    }

    /// Build the `ctx`, call `wf(ctx, input)`, and await its result if it is async.
    async fn drive_workflow(
        &self,
        wf: Value,
        input: Value,
        span: Span,
    ) -> Result<Value, Control> {
        let ctx = make_ctx();
        let result = self.call_value(wf, vec![ctx, input], span).await?;
        // A workflow body may be `async fn` (returns a Future) — await it.
        match result {
            Value::Future(f) => f.get().await,
            other => Ok(other),
        }
    }

    /// Persist the event log + restore the previous determinism context, then return
    /// the workflow result (or propagate its error). On success the terminal
    /// `WorkflowCompleted` line is appended so a later `resume` is idempotent.
    async fn finish_workflow(
        &self,
        outcome: Result<Value, Control>,
        log_path: &str,
        fsync: bool,
        prev: Option<DeterminismContext>,
        span: Span,
    ) -> Result<Value, Control> {
        let ctx = self.take_determinism();
        self.restore_determinism(prev);
        let events = ctx.map(|c| c.events).unwrap_or_default();
        // Always flush the recorded effect stream (even on error: a crash mid-run
        // leaves a partial log that a later resume fast-forwards through).
        let mut log = events_to_log(&events);
        if let Ok(ref result) = outcome {
            // Append the terminal completion record so `resume` is idempotent.
            let rec = serde_json::json!({
                "kind": "WorkflowCompleted",
                "result": serde_json::from_str::<serde_json::Value>(&to_json_string(result))
                    .unwrap_or(serde_json::Value::Null),
            });
            log.push_str(&rec.to_string());
            log.push('\n');
        }
        write_log(log_path, &log, fsync, span)?;
        outcome
    }

    /// `ctx.<method>(...)` dispatch (routed here by the call-site hook in both
    /// engines when the receiver is the workflow-context tagged Object). Outside a
    /// workflow (no determinism context) these are an error — `ctx` only exists
    /// inside a `run`/`resume`.
    pub(crate) async fn call_workflow_ctx(
        &self,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        if !self.is_deterministic() {
            return Err(AsError::at(
                format!("workflow ctx.{} called outside a workflow", name),
                span,
            )
            .into());
        }
        // args[0] is the receiver ctx (pushed by the hook); the user args follow.
        let user = &args[1..];
        match name {
            "now" => Ok(Value::Number(self.clock_now_ms())),
            "random" => Ok(Value::Number(self.next_seeded_f64().unwrap_or(0.0))),
            "uuid" => {
                let mut bytes = [0u8; 16];
                self.fill_seeded_bytes(&mut bytes);
                bytes[6] = (bytes[6] & 0x0f) | 0x40;
                bytes[8] = (bytes[8] & 0x3f) | 0x80;
                Ok(Value::Str(uuid::Uuid::from_bytes(bytes).to_string().into()))
            }
            "sleep" => {
                let ms = match user.first() {
                    Some(Value::Number(n)) => *n,
                    _ => 0.0,
                };
                // Durable timer: advance the virtual clock + record a TimerSet; no
                // real delay. On resume the recorded TimerSet is consumed and the
                // clock fast-forwards.
                self.with_determinism_mut(|c| {
                    if c.mode == Mode::Replay {
                        // Consume the recorded TimerSet (advance cursor) if present.
                        if let Some(DetEvent::TimerSet { wake }) = c.events.get(c.cursor).cloned() {
                            c.cursor += 1;
                            c.clock.set_now(wake);
                            return;
                        }
                        // No recorded timer (crash point): switch to record.
                        c.mode = Mode::Record;
                    }
                    c.clock.advance(ms);
                    let wake = c.clock.now_ms();
                    c.events.push(DetEvent::TimerSet { wake });
                });
                Ok(Value::Nil)
            }
            "call" => self.ctx_call_activity(user, span).await,
            _ => Err(AsError::at(format!("workflow ctx has no method '{}'", name), span).into()),
        }
    }

    /// `ctx.call(activity, ...args)`: record-or-replay an activity result by its
    /// sequence position. Record: run the activity fn for real, JSON-serialize the
    /// result, append `ActivityCompleted`. Replay: consume the next recorded event,
    /// assert its signature matches (else a non-determinism panic), and return its
    /// result WITHOUT executing the side effect; on a missing event (the crash
    /// point) switch to Record and execute for real.
    async fn ctx_call_activity(&self, user: &[Value], span: Span) -> Result<Value, Control> {
        let activity = user.first().cloned().unwrap_or(Value::Nil);
        if !is_activity_value(&activity) {
            return Err(AsError::at(
                "workflow ctx.call(activity, ...): first arg must be an activity",
                span,
            )
            .into());
        }
        let act_args: Vec<Value> = user.get(1..).map(|s| s.to_vec()).unwrap_or_default();
        let (act_name, act_fn) = activity_parts(&activity);
        let sig = signature_hash(&act_name, &act_args);

        // Replay: try to consume a recorded ActivityCompleted at the cursor.
        let replay_hit = self.with_determinism_mut(|c| {
            if c.mode != Mode::Replay {
                return Ok::<Option<Value>, String>(None);
            }
            match c.events.get(c.cursor).cloned() {
                Some(DetEvent::ActivityCompleted {
                    name,
                    args_hash,
                    result_json,
                }) => {
                    // Replay-mismatch detection: the recorded signature must match.
                    if name != act_name || args_hash != sig {
                        return Err(format!(
                            "workflow non-determinism: expected activity '{}' at seq {}, got '{}'",
                            name, c.cursor, act_name
                        ));
                    }
                    c.cursor += 1;
                    Ok(Some(from_json_string(&result_json)))
                }
                Some(_) => Err(format!(
                    "workflow non-determinism: expected activity '{}' at seq {}, got a non-activity event",
                    act_name, c.cursor
                )),
                None => {
                    // Crash point: nothing recorded here → switch to record + run.
                    c.mode = Mode::Record;
                    Ok(None)
                }
            }
        });
        match replay_hit {
            Some(Ok(Some(v))) => return Ok(v), // replayed, no side effect
            Some(Err(msg)) => return Err(AsError::at(msg, span).into()),
            Some(Ok(None)) | None => {} // record path below
        }

        // Record path: run the activity for real, await if async, serialize result.
        let result = self.call_value(act_fn, act_args, span).await?;
        let result = match result {
            Value::Future(f) => f.get().await?,
            other => other,
        };
        // Constraint: only Value-serializable results persist. A native handle /
        // function / class is a constraint violation at record time.
        if !is_serializable(&result) {
            return Err(AsError::at(
                "workflow: activity result is not serializable (return data, not a native handle/function/class)",
                span,
            )
            .into());
        }
        let result_json = to_json_string(&result);
        self.with_determinism_mut(|c| {
            c.events.push(DetEvent::ActivityCompleted {
                name: act_name.clone(),
                args_hash: sig,
                result_json: result_json.clone(),
            });
        });
        Ok(result)
    }
}

/// Extract `(name, fn)` from an activity tagged Object.
fn activity_parts(activity: &Value) -> (String, Value) {
    if let Value::Object(o) = activity {
        let m = o.borrow();
        let name = match m.get("name") {
            Some(Value::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let func = m.get("fn").cloned().unwrap_or(Value::Nil);
        (name, func)
    } else {
        (String::new(), Value::Nil)
    }
}

/// A callable value (function/closure/builtin/bound-method).
fn is_callable(v: &Value) -> bool {
    matches!(
        v,
        Value::Function(_)
            | Value::Closure(_)
            | Value::Builtin(_)
            | Value::BoundMethod(_)
            | Value::NativeMethod(_)
    )
}

/// A `Value` the JSON codec round-trips for the durable log: data only, never a
/// live native handle / function / class.
fn is_serializable(v: &Value) -> bool {
    !matches!(
        v,
        Value::Native(_)
            | Value::NativeMethod(_)
            | Value::Function(_)
            | Value::Closure(_)
            | Value::Builtin(_)
            | Value::Class(_)
            | Value::BoundMethod(_)
            | Value::Future(_)
            | Value::Generator(_)
    )
}

/// A small deterministic seed from the log path so a record/resume pair on the same
/// log share an RNG seed.
fn log_path_seed(path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

/// If the log text ends with a `WorkflowCompleted` record, return its result Value
/// (idempotent-resume short circuit).
fn completed_result(text: &str) -> Option<Value> {
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: serde_json::Value = serde_json::from_str(line).ok()?;
        if rec.get("kind").and_then(|k| k.as_str()) == Some("WorkflowCompleted") {
            let result = rec.get("result").cloned().unwrap_or(serde_json::Value::Null);
            return Some(crate::stdlib::json::to_ascript(&result));
        }
        // The last non-empty line was not a completion → not idempotent-done.
        return None;
    }
    None
}

/// Write the event log to disk, fsync-ing when `fsync` (durability "fsync").
fn write_log(path: &str, contents: &str, fsync: bool, span: Span) -> Result<(), Control> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)
        .map_err(|e| AsError::at(format!("workflow: cannot write log '{}': {}", path, e), span))?;
    f.write_all(contents.as_bytes())
        .map_err(|e| AsError::at(format!("workflow: log write failed: {}", e), span))?;
    if fsync {
        let _ = f.sync_all();
    }
    Ok(())
}

// Keep `Rc`/`RefCell` imports used (the tagged-object constructors build cells).
const _: fn() = || {
    let _ = Rc::new(RefCell::new(0u8));
};
