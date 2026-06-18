//! `std/resilience` — backend-hosting policy kit (RESIL §2).
//!
//! Policies are tagged `Value::Object`s with a `__resil` kind field
//! (mirroring `std/schema`'s `{__kind: …}` convention). The call-site hook
//! that makes `b.call(fn)` work is Task 1.2; this module (Task 1.1) only
//! registers the module, adds `NativeKind::Resilience`, and implements the
//! `breaker(opts)` constructor.
//!
//! ## Tagged-object model (§2.2)
//!
//! A policy object carries:
//! - `__resil: "<kind>"` — kind ∈ `RESIL_KINDS`.
//! - Config fields readable as plain members.
//! - Mutable state fields (`__state`, `__failures`, …) — plain `Value`s
//!   mutated under short borrows (no borrow across `.await`).
//! - `__local`: a `Value::Native(NativeKind::Resilience, id=u64::MAX)` —
//!   the non-sendable marker (the `noop_handle` precedent from
//!   `telemetry/mod.rs:113`). This makes the worker airlock reject a policy
//!   crossing an isolate boundary loudly instead of silently deep-copying
//!   its counters into a divergent twin.

use super::arg;
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::{NativeKind, NativeObject, Value, ValueKind};
use indexmap::IndexMap;
use std::rc::Rc;

// ── per-isolate state ─────────────────────────────────────────────────────────

/// Per-isolate singleflight table (§3.6) + metrics registry (§6.1).
///
/// Lives on `Interp.resilience` (a `RefCell<ResilState>`, `#[cfg(feature =
/// "resilience")]`). Tasks 3.2/3.3/5.x consume it; this task (3.1) only
/// declares the struct so there is ONE Interp touch total.
#[derive(Default)]
// `flights` is read in Task 3.2 (singleflight), `registry` in Phase 5 (§6.1) — pre-declared
// here as the single Interp touch.
#[allow(dead_code)]
pub(crate) struct ResilState {
    /// Active singleflight flights keyed by the user-supplied string key.
    /// Each value is the `SharedFuture` for the ONE in-progress execution;
    /// concurrent callers with the same key clone-and-await it instead of
    /// launching a second invocation. Entries are removed when the flight
    /// resolves (Task 3.2).
    pub(crate) flights: IndexMap<String, crate::task::SharedFuture>,
    /// Minimal per-isolate metrics registry (§6.1). Phase 5 fills this;
    /// currently empty (the `Default` impl gives zero cost).
    pub(crate) registry: ResilRegistry,
}

/// Per-isolate minimal metrics registry — Phase 5 will add counter/gauge
/// fields here. `#[derive(Default)]` so `ResilState::default()` is free.
#[derive(Default)]
pub(crate) struct ResilRegistry {
    // Phase 5 fills this (§6.1).
}

// ── public exports ────────────────────────────────────────────────────────────

/// The export list (binding name → value) for `import * from "std/resilience"`.
///
/// Task 1.1 ships only `breaker`; subsequent tasks will extend this list.
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("breaker", super::bi("resilience.breaker")),
        ("limiter", super::bi("resilience.limiter")),
        ("keyedLimiter", super::bi("resilience.keyedLimiter")),
        ("bulkhead", super::bi("resilience.bulkhead")),
        ("retry", super::bi("resilience.retry")),
        ("fallback", super::bi("resilience.fallback")),
    ]
}

// ── tagged-object helpers ─────────────────────────────────────────────────────

/// Known `__resil` kind tags — kept narrow so an unrelated user object that
/// happens to carry a `__resil` field is never hijacked by the call-site hook.
pub(crate) const RESIL_KINDS: &[&str] =
    &["breaker", "limiter", "keyedLimiter", "bulkhead", "retry", "memoize"];

/// True iff `v` is a resilience policy object: a `Value::Object` whose `__resil`
/// field is a string in `RESIL_KINDS`.
pub(crate) fn is_resilience_value(v: &Value) -> bool {
    resil_kind(v).map(|k| RESIL_KINDS.contains(&&*k)).unwrap_or(false)
}

/// The method names that route through the call-site hook (Task 1.2).
pub(crate) fn is_resilience_method(name: &str) -> bool {
    matches!(
        name,
        "call" | "state" | "stats" | "reset" | "acquire" | "tryAcquire" | "run" | "get"
            | "delete" | "clear"
    )
}

/// Extract the `__resil` field from an object, or `None`.
fn resil_kind(v: &Value) -> Option<Rc<str>> {
    match v.kind() {
        ValueKind::Object(o) => match o.get("__resil").as_ref().map(|v| v.kind()) {
            Some(ValueKind::Str(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Build the `__local` non-sendable marker: a `Value::Native` with
/// `NativeKind::Resilience`, id `u64::MAX`, and empty fields.
/// Mirrors `noop_handle` in `src/stdlib/telemetry/mod.rs:114`.
fn local_marker() -> Value {
    Value::native(Rc::new(NativeObject {
        id: u64::MAX,
        kind: NativeKind::Resilience,
        fields: IndexMap::new(),
    }))
}

// ── breaker constructor ───────────────────────────────────────────────────────

/// Build a circuit-breaker policy object from an options `Value::Object`.
///
/// Defaults (§3.1):
/// - `name`: `"default"`
/// - `failureRate`: `0.5`
/// - `window`: `20`
/// - `minCalls`: `10`
/// - `cooldownMs`: `30000`
/// - `halfOpenMax`: `3`
///
/// Validation (Tier-2 panic on misuse):
/// - `failureRate` ∈ (0, 1]
/// - `window`, `minCalls`, `cooldownMs`, `halfOpenMax` positive integers (≥ 1)
/// - `name` a string
fn make_breaker(opts: Value, span: Span) -> Result<Value, Control> {
    // ── extract opts fields ───────────────────────────────────────────────────
    let name = match &opts.kind() {
        ValueKind::Object(o) => match o.get("name") {
            Some(v) => match v.kind() {
                ValueKind::Str(s) => s.to_string(),
                ValueKind::Nil => "default".to_string(),
                _ => {
                    return Err(AsError::at(
                        "breaker: 'name' must be a string",
                        span,
                    )
                    .into())
                }
            },
            None => "default".to_string(),
        },
        ValueKind::Nil => "default".to_string(),
        _ => {
            return Err(AsError::at(
                "breaker: expected an options object, got non-object",
                span,
            )
            .into())
        }
    };

    let failure_rate = opt_f64(&opts, "failureRate", 0.5);
    let window = opt_pos_int(&opts, "window", 20);
    let min_calls = opt_pos_int(&opts, "minCalls", 10);
    let cooldown_ms = opt_pos_int(&opts, "cooldownMs", 30000);
    let half_open_max = opt_pos_int(&opts, "halfOpenMax", 3);

    // ── validate ──────────────────────────────────────────────────────────────
    match failure_rate {
        Ok(r) if r > 0.0 && r <= 1.0 => {}
        Ok(_) => {
            return Err(AsError::at(
                "breaker: 'failureRate' must be in (0, 1]",
                span,
            )
            .into())
        }
        Err(msg) => return Err(AsError::at(msg, span).into()),
    }
    let failure_rate = failure_rate.unwrap();

    let window = check_pos_int(window, "window", span)?;
    let min_calls = check_pos_int(min_calls, "minCalls", span)?;
    let cooldown_ms = check_pos_int(cooldown_ms, "cooldownMs", span)?;
    let half_open_max = check_pos_int(half_open_max, "halfOpenMax", span)?;

    // ── build the policy object ───────────────────────────────────────────────
    let mut m: IndexMap<String, Value> = IndexMap::new();
    // kind tag
    m.insert("__resil".to_string(), Value::str("breaker"));
    // config fields
    m.insert("name".to_string(), Value::str(name));
    m.insert("failureRate".to_string(), Value::float(failure_rate));
    m.insert("window".to_string(), Value::int(window as i64));
    m.insert("minCalls".to_string(), Value::int(min_calls as i64));
    m.insert("cooldownMs".to_string(), Value::int(cooldown_ms as i64));
    m.insert("halfOpenMax".to_string(), Value::int(half_open_max as i64));
    // initial state fields (§3.1.2)
    m.insert("__state".to_string(), Value::str("closed"));
    m.insert("__ring".to_string(), Value::array(vec![]));
    m.insert("__ringIdx".to_string(), Value::int(0));
    m.insert("__calls".to_string(), Value::int(0));
    m.insert("__failures".to_string(), Value::int(0));
    m.insert("__rejected".to_string(), Value::int(0));
    m.insert("__openedAtMs".to_string(), Value::nil());
    m.insert("__halfOpenInFlight".to_string(), Value::int(0));
    m.insert("__halfOpenSuccesses".to_string(), Value::int(0));
    // non-sendable marker
    m.insert("__local".to_string(), local_marker());

    Ok(Value::object(m))
}

// ── option-field helpers ──────────────────────────────────────────────────────

/// Read a float field from an Object, returning `default` if absent or nil.
/// Returns an `Err(message)` string if the field is present but not a number.
fn opt_f64(opts: &Value, key: &str, default: f64) -> Result<f64, String> {
    let v = match opts.kind() {
        ValueKind::Object(o) => o.get(key),
        _ => return Ok(default),
    };
    match v {
        None => Ok(default),
        Some(v) => match v.as_f64() {
            Some(n) => Ok(n),
            None => match v.kind() {
                ValueKind::Nil => Ok(default),
                _ => Err(format!(
                    "breaker: '{}' must be a number, got {}",
                    key,
                    crate::interp::type_name(&v)
                )),
            },
        },
    }
}

/// Read a positive-integer field from an Object, returning `default` if absent or nil.
/// Returns an `Err(message)` string if the field is present but not a number.
fn opt_pos_int(opts: &Value, key: &str, default: u64) -> Result<u64, String> {
    let v = match opts.kind() {
        ValueKind::Object(o) => o.get(key),
        _ => return Ok(default),
    };
    match v {
        None => Ok(default),
        Some(v) => match v.as_f64() {
            Some(n) => Ok(n as u64),
            None => match v.kind() {
                ValueKind::Nil => Ok(default),
                _ => Err(format!(
                    "breaker: '{}' must be a number, got {}",
                    key,
                    crate::interp::type_name(&v)
                )),
            },
        },
    }
}

/// Validate that `r` is `>= 1` (positive integer); convert the `Err(msg)` to
/// a `Control::Panic` with `span`.
fn check_pos_int(r: Result<u64, String>, field: &str, span: Span) -> Result<u64, Control> {
    match r {
        Err(msg) => Err(AsError::at(msg, span).into()),
        Ok(0) => Err(AsError::at(
            format!("breaker: '{}' must be a positive integer (>= 1)", field),
            span,
        )
        .into()),
        Ok(n) => Ok(n),
    }
}

// ── call-site hook: method dispatch on a resilience policy object ─────────────

/// Dispatch a method call on a resilience policy object (§2.3 call-site hook).
///
/// Called from BOTH the tree-walker (`member_call_is_hook` / `call_method_recv`,
/// `src/interp.rs`) and the VM (`dispatch_method`, `src/vm/run.rs`), always with
/// the receiver at `args[0]` followed by the user-supplied arguments — the same
/// `[recv, ...args]` convention `call_schema` uses.
///
/// For kinds not yet implemented (all except `breaker` in Task 1.2), every
/// method raises a Tier-2 panic with `"<kind> policy has no method '<name>'"`.
impl Interp {
    pub(crate) async fn call_resilience_method(
        &self,
        name: &str,
        args: &[Value],   // args[0] = the policy receiver
        span: Span,
    ) -> Result<Value, Control> {
        // args[0] is always the receiver (the `[recv, ...user_args]` convention).
        let recv = args.first().cloned().unwrap_or(Value::nil());
        let kind = resil_kind(&recv)
            .map(|k| k.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        match kind.as_str() {
            "breaker" => self.call_breaker_method(&recv, name, args, span).await,
            "limiter" => self.call_limiter_method(&recv, name, args, span).await,
            "keyedLimiter" => self.call_keyed_limiter_method(&recv, name, args, span).await,
            "bulkhead" => self.call_bulkhead_method(&recv, name, args, span).await,
            "retry" => self.call_retry_method(&recv, name, args, span).await,
            other => Err(AsError::at(
                format!("{} policy has no method '{}'", other, name),
                span,
            )
            .into()),
        }
    }

    /// Dispatch a method call on a `breaker` policy object.
    async fn call_breaker_method(
        &self,
        recv: &Value,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match name {
            // state() → returns the __state field value ("closed" | "open" | "halfOpen")
            "state" => {
                let s = match recv.kind() {
                    ValueKind::Object(o) => o
                        .get("__state")
                        .unwrap_or(Value::str("closed")),
                    _ => Value::str("closed"),
                };
                Ok(s)
            }
            // stats() → {state, calls, failures, rejected, windowFailureRate}
            "stats" => {
                let (state, calls, failures, rejected, ring, ring_idx, window, min_calls) = match recv.kind() {
                    ValueKind::Object(o) => (
                        o.get("__state").unwrap_or(Value::str("closed")),
                        o.get("__calls").unwrap_or(Value::int(0)),
                        o.get("__failures").unwrap_or(Value::int(0)),
                        o.get("__rejected").unwrap_or(Value::int(0)),
                        o.get("__ring").unwrap_or(Value::array(vec![])),
                        o.get("__ringIdx").and_then(|v| v.as_int()).unwrap_or(0) as usize,
                        o.get("window").and_then(|v| v.as_int()).unwrap_or(20) as usize,
                        o.get("minCalls").and_then(|v| v.as_int()).unwrap_or(10) as usize,
                    ),
                    _ => (Value::str("closed"), Value::int(0), Value::int(0), Value::int(0),
                          Value::array(vec![]), 0, 20, 10),
                };
                let total_calls = calls.as_int().unwrap_or(0) as usize;
                let window_failure_rate = if total_calls >= min_calls {
                    let filled = total_calls.min(window);
                    let fail_count: i64 = match ring.kind() {
                        ValueKind::Array(a) => {
                            let b = a.borrow();
                            let start = if filled == window { ring_idx } else { 0 };
                            (0..filled).map(|i| {
                                b.get((start + i) % window).and_then(|v| v.as_int()).unwrap_or(0)
                            }).sum()
                        }
                        _ => 0,
                    };
                    fail_count as f64 / filled as f64
                } else {
                    0.0
                };
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("state".to_string(), state);
                m.insert("calls".to_string(), calls);
                m.insert("failures".to_string(), failures);
                m.insert("rejected".to_string(), rejected);
                m.insert("windowFailureRate".to_string(), Value::float(window_failure_rate));
                Ok(Value::object(m))
            }
            // reset() → clears state back to closed, window cleared
            "reset" => {
                if let ValueKind::Object(o) = recv.kind() {
                    o.insert("__state", Value::str("closed"));
                    o.insert("__ring", Value::array(vec![]));
                    o.insert("__ringIdx", Value::int(0));
                    o.insert("__calls", Value::int(0));
                    o.insert("__failures", Value::int(0));
                    o.insert("__rejected", Value::int(0));
                    o.insert("__openedAtMs", Value::nil());
                    o.insert("__halfOpenInFlight", Value::int(0));
                    o.insert("__halfOpenSuccesses", Value::int(0));
                }
                Ok(Value::nil())
            }
            // call(fn) → the full state machine §3.1.2/§3.1.3
            "call" => {
                // args[0] = receiver, args[1] = the fn to call
                let user_fn = args.get(1).cloned().unwrap_or(Value::nil());
                self.breaker_call(recv, user_fn, span).await
            }
            other => Err(AsError::at(
                format!("breaker policy has no method '{}'", other),
                span,
            )
            .into()),
        }
    }

    /// The full `breaker.call(fn)` state machine (§3.1.2/§3.1.3).
    ///
    /// NO `RefCell`/`ObjectCell` borrow is held across `.await`.
    /// `__halfOpenInFlight` is decremented on ALL exit paths (success, failure, panic,
    /// propagate) when the call was admitted as a probe.
    async fn breaker_call(
        &self,
        recv: &Value,
        user_fn: Value,
        span: Span,
    ) -> Result<Value, Control> {
        use crate::interp::{make_pair, result_pair_err};
        use crate::stdlib::time::real_monotonic_ms;

        // ── Step 1: Read state + config (sync, short borrow, drop before await) ─
        let (state, opened_at_ms, cooldown_ms, half_open_in_flight, half_open_max,
             window, min_calls, failure_rate) = {
            match recv.kind() {
                ValueKind::Object(o) => (
                    o.get("__state")
                        .and_then(|v| match v.kind() { ValueKind::Str(s) => Some(s.to_string()), _ => None })
                        .unwrap_or_else(|| "closed".to_string()),
                    o.get("__openedAtMs").and_then(|v| v.as_f64()),
                    o.get("cooldownMs")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(30000.0),
                    o.get("__halfOpenInFlight").and_then(|v| v.as_int()).unwrap_or(0),
                    o.get("halfOpenMax").and_then(|v| v.as_int()).unwrap_or(3),
                    o.get("window").and_then(|v| v.as_int()).unwrap_or(20) as usize,
                    o.get("minCalls").and_then(|v| v.as_int()).unwrap_or(10) as usize,
                    o.get("failureRate").and_then(|v| v.as_f64()).unwrap_or(0.5),
                ),
                _ => ("closed".to_string(), None, 30000.0, 0, 3, 20, 10, 0.5),
            }
        };

        // ── Step 2: Admit/reject decision (sync) ─────────────────────────────────
        let now = self.clock_monotonic_ms(real_monotonic_ms());
        let is_probe;

        match state.as_str() {
            "closed" => {
                // Always admit; not a probe
                is_probe = false;
            }
            "open" => {
                // Check if cooldown has elapsed → transition to halfOpen as probe
                let elapsed = opened_at_ms.map(|at| now - at).unwrap_or(f64::MAX);
                if elapsed >= cooldown_ms {
                    // Transition to halfOpen, admit as probe
                    if let ValueKind::Object(o) = recv.kind() {
                        o.insert("__state", Value::str("halfOpen"));
                        o.insert("__halfOpenInFlight", Value::int(1));
                        o.insert("__halfOpenSuccesses", Value::int(0));
                    }
                    is_probe = true;
                } else {
                    // Still open → reject
                    if let ValueKind::Object(o) = recv.kind() {
                        let rej = o.get("__rejected").and_then(|v| v.as_int()).unwrap_or(0);
                        o.insert("__rejected", Value::int(rej + 1));
                    }
                    let name = match recv.kind() {
                        ValueKind::Object(o) => o.get("name")
                            .and_then(|v| match v.kind() { ValueKind::Str(s) => Some(s.to_string()), _ => None })
                            .unwrap_or_else(|| "default".to_string()),
                        _ => "default".to_string(),
                    };
                    let mut err: IndexMap<String, Value> = IndexMap::new();
                    err.insert("message".to_string(), Value::str(format!("circuit breaker '{}' is open", name)));
                    err.insert("code".to_string(), Value::str("breaker-open"));
                    return Ok(make_pair(Value::nil(), Value::object(err)));
                }
            }
            "halfOpen" => {
                // Admit if probe budget allows
                if half_open_in_flight < half_open_max {
                    if let ValueKind::Object(o) = recv.kind() {
                        o.insert("__halfOpenInFlight", Value::int(half_open_in_flight + 1));
                    }
                    is_probe = true;
                } else {
                    // Budget exhausted → reject
                    if let ValueKind::Object(o) = recv.kind() {
                        let rej = o.get("__rejected").and_then(|v| v.as_int()).unwrap_or(0);
                        o.insert("__rejected", Value::int(rej + 1));
                    }
                    let name = match recv.kind() {
                        ValueKind::Object(o) => o.get("name")
                            .and_then(|v| match v.kind() { ValueKind::Str(s) => Some(s.to_string()), _ => None })
                            .unwrap_or_else(|| "default".to_string()),
                        _ => "default".to_string(),
                    };
                    let mut err: IndexMap<String, Value> = IndexMap::new();
                    err.insert("message".to_string(), Value::str(format!("circuit breaker '{}' is open", name)));
                    err.insert("code".to_string(), Value::str("breaker-open"));
                    return Ok(make_pair(Value::nil(), Value::object(err)));
                }
            }
            // Defensive: unknown state treated as closed
            _ => { is_probe = false; }
        }

        // ── Step 3: Call the user fn (await — NO borrow held) ────────────────────
        let call_result = self.call_value(user_fn, vec![], span).await;
        // Drive any returned future to completion (the task.retry pattern)
        let outcome = match call_result {
            Ok(v) => {
                match v.kind() {
                    ValueKind::Future(_) => {
                        let crate::value::OwnedKind::Future(f) = v.into_kind() else {
                            unreachable!()
                        };
                        f.get().await
                    }
                    _ => Ok(v),
                }
            }
            other => other,
        };

        // ── Step 4 & 5 & 6: Classify outcome, update ring, transition (sync) ─────
        // We use an inner closure to centralize the per-path logic, keeping
        // all borrow-of-object work synchronous and after the await point.
        match outcome {
            // §3.1.3: Propagate — pass through UNrecorded; decrement probe counter
            Err(Control::Propagate(pair)) => {
                if is_probe {
                    if let ValueKind::Object(o) = recv.kind() {
                        let inf = o.get("__halfOpenInFlight").and_then(|v| v.as_int()).unwrap_or(0);
                        o.insert("__halfOpenInFlight", Value::int((inf - 1).max(0)));
                    }
                }
                Err(Control::Propagate(pair))
            }

            // §3.1.3: Panic — count as failure, decrement probe counter, RE-RAISE
            Err(Control::Panic(e)) => {
                // Always decrement probe counter first
                if is_probe {
                    if let ValueKind::Object(o) = recv.kind() {
                        let inf = o.get("__halfOpenInFlight").and_then(|v| v.as_int()).unwrap_or(0);
                        o.insert("__halfOpenInFlight", Value::int((inf - 1).max(0)));
                    }
                }
                // Record failure in ring + counters + transition
                breaker_record_outcome(recv, true /* is_failure */, is_probe,
                                       false /* transition already handled via halfOpen fail below */,
                                       window, min_calls, failure_rate, now, true /* panic path */);
                Err(Control::Panic(e))
            }

            // §3.1.3: Exit — pass through (same as Propagate for our purposes)
            Err(Control::Exit(code)) => {
                if is_probe {
                    if let ValueKind::Object(o) = recv.kind() {
                        let inf = o.get("__halfOpenInFlight").and_then(|v| v.as_int()).unwrap_or(0);
                        o.insert("__halfOpenInFlight", Value::int((inf - 1).max(0)));
                    }
                }
                Err(Control::Exit(code))
            }

            // §3.1.3: Plain value or ok/err pair
            Ok(v) => {
                let is_failure = result_pair_err(&v).is_some();
                // Decrement probe inflight counter for success path (panics do it above)
                if is_probe {
                    if let ValueKind::Object(o) = recv.kind() {
                        let inf = o.get("__halfOpenInFlight").and_then(|v| v.as_int()).unwrap_or(0);
                        o.insert("__halfOpenInFlight", Value::int((inf - 1).max(0)));
                    }
                }
                breaker_record_outcome(recv, is_failure, is_probe, true,
                                       window, min_calls, failure_rate, now, false);
                // Normalize: wrap plain values in [v, nil] so callers always get a pair.
                // If it's already a 2-element array (ok-pair or err-pair), pass through.
                let is_pair = match v.kind() {
                    ValueKind::Array(a) => a.borrow().len() == 2,
                    _ => false,
                };
                if is_pair {
                    Ok(v)
                } else {
                    Ok(make_pair(v, Value::nil()))
                }
            }
        }
    }
}

// ── limiter method dispatch ───────────────────────────────────────────────────

impl Interp {
    /// Dispatch a method call on a `limiter` policy object.
    ///
    /// ## `tryAcquire([n])` — sync, atomic
    /// Refills tokens, then if `__tokens >= n`: consume n, return true.
    /// Else: no-op, return false.  All-or-nothing under one synchronous borrow.
    ///
    /// ## `acquire([n])` — async deficit-sleep loop
    /// Short borrow: refill + check.  If enough tokens: consume + return.
    /// Else: compute deficit sleep duration, DROP borrow, sleep, re-loop.
    /// CRITICAL: no borrow held across the sleep `.await`.
    async fn call_limiter_method(
        &self,
        recv: &Value,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        use crate::stdlib::time::real_monotonic_ms;

        match name {
            "tryAcquire" => {
                // args[0] = recv, args[1] (optional) = n
                let n = args.get(1).and_then(|v| v.as_f64()).unwrap_or(1.0);
                if n < 0.0 {
                    return Err(AsError::at(
                        "limiter.tryAcquire: n must be non-negative",
                        span,
                    ).into());
                }

                // Short synchronous borrow — no .await inside
                let now = self.clock_monotonic_ms(real_monotonic_ms());
                let new_tokens = limiter_refill(recv, now);
                if new_tokens >= n {
                    // Consume n tokens (atomic: check+consume under one sync borrow)
                    if let ValueKind::Object(o) = recv.kind() {
                        o.insert("__tokens", Value::float(new_tokens - n));
                    }
                    Ok(Value::bool_(true))
                } else {
                    Ok(Value::bool_(false))
                }
            }

            "acquire" => {
                // args[0] = recv, args[1] (optional) = n
                let n = args.get(1).and_then(|v| v.as_f64()).unwrap_or(1.0);
                if n < 0.0 {
                    return Err(AsError::at(
                        "limiter.acquire: n must be non-negative",
                        span,
                    ).into());
                }

                loop {
                    // ── Short borrow: refill + check ──────────────────────────
                    let now = self.clock_monotonic_ms(real_monotonic_ms());
                    let new_tokens = limiter_refill(recv, now);
                    let refill_per_sec = match recv.kind() {
                        ValueKind::Object(o) => {
                            o.get("refillPerSec").and_then(|v| v.as_f64()).unwrap_or(0.0)
                        }
                        _ => 0.0,
                    };

                    if new_tokens >= n {
                        // Enough tokens — consume and return
                        if let ValueKind::Object(o) = recv.kind() {
                            o.insert("__tokens", Value::float(new_tokens - n));
                        }
                        return Ok(Value::nil());
                    }

                    // Not enough — compute sleep duration
                    let deficit = n - new_tokens;
                    let sleep_ms = if refill_per_sec > 0.0 {
                        (deficit / refill_per_sec * 1000.0).max(1.0)
                    } else {
                        // Zero refill rate: never refills — avoid infinite loop
                        return Err(AsError::at(
                            "limiter.acquire: cannot acquire tokens from a zero-refill limiter",
                            span,
                        ).into());
                    };

                    // ── Borrow is DROPPED here before .await ──────────────────
                    let duration = std::time::Duration::from_millis(sleep_ms as u64);
                    tokio::time::sleep(duration).await;
                    // Re-loop to re-check after sleep
                }
            }

            other => Err(AsError::at(
                format!("limiter policy has no method '{}'", other),
                span,
            ).into()),
        }
    }
}

// ── breaker_record_outcome: ring update + state transition ───────────────────

/// Record a call outcome into the ring window and perform state transitions.
///
/// Called AFTER the `.await` point in `breaker_call`, so all object borrows
/// here are short synchronous borrows with no `.await` in scope.
///
/// # Parameters
/// - `recv`: the breaker policy object
/// - `is_failure`: true iff the outcome was a failure (error pair or panic)
/// - `is_probe`: true iff this call was a half-open probe
/// - `allow_transition`: true for normal Ok paths; false for the panic path
///   where the probe counter was already decremented BEFORE this call
/// - `window`: ring buffer size (config field)
/// - `min_calls`: minimum calls before verdict (config field)
/// - `failure_rate_threshold`: the `failureRate` config field
/// - `now`: the current monotonic clock value (pre-computed; no det borrow here)
/// - `panic_path`: when true, the `__halfOpenInFlight` was already decremented
///   by the caller — skip the halfOpen success path for probe handling
// The breaker outcome recorder threads the policy's count-window config + the timing
// snapshot explicitly (the state mutation runs in a synchronous section with the ObjectCell
// borrow taken locally — passing the config by value keeps the borrow scopes minimal). The
// nested probe-failure/probe-success branch is kept explicit for the state-machine clarity
// the §3.1.2 transitions demand; collapsing it into a match guard would change the
// non-probe-in-halfOpen fall-through.
#[allow(clippy::too_many_arguments, clippy::collapsible_match)]
fn breaker_record_outcome(
    recv: &Value,
    is_failure: bool,
    is_probe: bool,
    allow_transition: bool,
    window: usize,
    min_calls: usize,
    failure_rate_threshold: f64,
    now: f64,
    _panic_path: bool,
) {
    let o = match recv.kind() {
        ValueKind::Object(o) => o,
        _ => return,
    };

    let current_state = o.get("__state")
        .and_then(|v| match v.kind() { ValueKind::Str(s) => Some(s.to_string()), _ => None })
        .unwrap_or_else(|| "closed".to_string());

    // ── Ring update (always for admitted calls) ──────────────────────────────
    let ring_idx = o.get("__ringIdx").and_then(|v| v.as_int()).unwrap_or(0) as usize;
    let calls = o.get("__calls").and_then(|v| v.as_int()).unwrap_or(0);
    let failures = o.get("__failures").and_then(|v| v.as_int()).unwrap_or(0);

    // Grow or update the ring
    let ring_val = is_failure as i64; // 1 = failure, 0 = success
    let ring = o.get("__ring").unwrap_or(Value::array(vec![]));
    let ring_len = match ring.kind() {
        ValueKind::Array(a) => a.borrow().len(),
        _ => 0,
    };

    if ring_len < window {
        // Ring not yet full — push to grow it
        if let ValueKind::Array(a) = ring.kind() {
            a.borrow_mut().push(Value::int(ring_val));
        }
    } else {
        // Ring full — overwrite the oldest slot (ring_idx % window)
        if let ValueKind::Array(a) = ring.kind() {
            let mut b = a.borrow_mut();
            let slot = ring_idx % window;
            b[slot] = Value::int(ring_val);
        }
    }

    // Advance ring index and call counter
    o.insert("__ringIdx", Value::int((ring_idx + 1) as i64));
    o.insert("__calls", Value::int(calls + 1));
    if is_failure {
        o.insert("__failures", Value::int(failures + 1));
    }

    if !allow_transition {
        // Panic path: ring updated, probe counter already decremented, no further transition
        return;
    }

    // ── State transitions ────────────────────────────────────────────────────
    match current_state.as_str() {
        "closed" => {
            // Re-read ring after update
            let total_calls = (calls + 1) as usize;
            if total_calls >= min_calls {
                let filled = total_calls.min(window);
                let new_ring_idx = ring_idx + 1;
                let fail_count: i64 = match o.get("__ring").unwrap_or(Value::array(vec![])).kind() {
                    ValueKind::Array(a) => {
                        let b = a.borrow();
                        let start = if filled == window { new_ring_idx % window } else { 0 };
                        (0..filled).map(|i| {
                            b.get((start + i) % window)
                                .and_then(|v| v.as_int())
                                .unwrap_or(0)
                        }).sum()
                    }
                    _ => 0,
                };
                let rate = fail_count as f64 / filled as f64;
                if rate >= failure_rate_threshold {
                    // → open
                    o.insert("__state", Value::str("open"));
                    o.insert("__openedAtMs", Value::float(now));
                    o.insert("__halfOpenInFlight", Value::int(0));
                    o.insert("__halfOpenSuccesses", Value::int(0));
                }
            }
        }
        "halfOpen" => {
            if is_probe {
                if is_failure {
                    // Any probe failure → re-open with fresh cooldown
                    o.insert("__state", Value::str("open"));
                    o.insert("__openedAtMs", Value::float(now));
                    o.insert("__halfOpenInFlight", Value::int(0));
                    o.insert("__halfOpenSuccesses", Value::int(0));
                } else {
                    // Probe success: increment successes; if ≥ halfOpenMax → closed
                    let succ = o.get("__halfOpenSuccesses").and_then(|v| v.as_int()).unwrap_or(0);
                    let new_succ = succ + 1;
                    let half_open_max = o.get("halfOpenMax").and_then(|v| v.as_int()).unwrap_or(3);
                    o.insert("__halfOpenSuccesses", Value::int(new_succ));
                    if new_succ >= half_open_max {
                        // → closed: reset ring
                        o.insert("__state", Value::str("closed"));
                        o.insert("__ring", Value::array(vec![]));
                        o.insert("__ringIdx", Value::int(0));
                        o.insert("__calls", Value::int(0));
                        o.insert("__failures", Value::int(0));
                        o.insert("__halfOpenInFlight", Value::int(0));
                        o.insert("__halfOpenSuccesses", Value::int(0));
                    }
                }
            }
        }
        _ => {}
    }
}

// ── limiter constructor ───────────────────────────────────────────────────────

/// Build a token-bucket limiter policy object from an options `Value::Object`.
///
/// Required (Tier-2 panic if wrong):
/// - `capacity`: positive integer (≥ 1)
/// - `refillPerSec`: non-negative finite number (≥ 0)
///
/// Optional:
/// - `name`: string label (default `"default"`)
///
/// State fields set on construction (§3.2.1):
/// - `__tokens: float` — initialized to `capacity` (full bucket)
/// - `__lastMs: float` — current monotonic time (via `real_monotonic_ms`)
fn make_limiter(opts: Value, span: Span) -> Result<Value, Control> {
    use crate::stdlib::time::real_monotonic_ms;

    let name = limiter_opt_name(&opts, span)?;
    let capacity = limiter_opt_capacity(&opts, span)?;
    let refill_per_sec = limiter_opt_refill_per_sec(&opts, span)?;

    // Initial state: full bucket, clock set to now
    let now = real_monotonic_ms();

    let mut m: IndexMap<String, Value> = IndexMap::new();
    // kind tag
    m.insert("__resil".to_string(), Value::str("limiter"));
    // config fields
    m.insert("name".to_string(), Value::str(name));
    m.insert("capacity".to_string(), Value::float(capacity));
    m.insert("refillPerSec".to_string(), Value::float(refill_per_sec));
    // mutable state — floats for precision
    m.insert("__tokens".to_string(), Value::float(capacity));
    m.insert("__lastMs".to_string(), Value::float(now));
    // non-sendable marker
    m.insert("__local".to_string(), local_marker());

    Ok(Value::object(m))
}

/// Extract `name` field (default `"default"`).
fn limiter_opt_name(opts: &Value, span: Span) -> Result<String, Control> {
    match opts.kind() {
        ValueKind::Object(o) => match o.get("name") {
            Some(v) => match v.kind() {
                ValueKind::Str(s) => Ok(s.to_string()),
                ValueKind::Nil => Ok("default".to_string()),
                _ => Err(AsError::at("limiter: 'name' must be a string", span).into()),
            },
            None => Ok("default".to_string()),
        },
        ValueKind::Nil => Ok("default".to_string()),
        _ => Err(AsError::at("limiter: expected an options object, got non-object", span).into()),
    }
}

/// Extract `capacity` as `f64` (must be ≥ 1).
fn limiter_opt_capacity(opts: &Value, span: Span) -> Result<f64, Control> {
    let v = match opts.kind() {
        ValueKind::Object(o) => o.get("capacity"),
        ValueKind::Nil => None,
        _ => return Err(AsError::at("limiter: expected an options object, got non-object", span).into()),
    };
    match v {
        None => Err(AsError::at("limiter: 'capacity' is required", span).into()),
        Some(v) => match v.as_f64() {
            Some(n) if n >= 1.0 => Ok(n),
            Some(_) => Err(AsError::at(
                "limiter: 'capacity' must be a positive integer (>= 1)",
                span,
            ).into()),
            None => Err(AsError::at(
                format!(
                    "limiter: 'capacity' must be a number, got {}",
                    crate::interp::type_name(&v)
                ),
                span,
            ).into()),
        },
    }
}

/// Extract `refillPerSec` as `f64` (must be ≥ 0 and finite).
fn limiter_opt_refill_per_sec(opts: &Value, span: Span) -> Result<f64, Control> {
    let v = match opts.kind() {
        ValueKind::Object(o) => o.get("refillPerSec"),
        ValueKind::Nil => None,
        _ => return Err(AsError::at("limiter: expected an options object, got non-object", span).into()),
    };
    match v {
        None => Err(AsError::at("limiter: 'refillPerSec' is required", span).into()),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && n >= 0.0 => Ok(n),
            Some(_) => Err(AsError::at(
                "limiter: 'refillPerSec' must be a non-negative finite number",
                span,
            ).into()),
            None => Err(AsError::at(
                format!(
                    "limiter: 'refillPerSec' must be a number, got {}",
                    crate::interp::type_name(&v)
                ),
                span,
            ).into()),
        },
    }
}

/// Refill `__tokens` based on elapsed time since `__lastMs` (§3.2.1 formula).
///
/// `now` must be from `clock_monotonic_ms` (det-routed).
/// Returns the post-refill token count.
/// MUST be called under a short, synchronous borrow — no `.await` inside.
fn limiter_refill(obj: &Value, now: f64) -> f64 {
    let o = match obj.kind() {
        ValueKind::Object(o) => o,
        _ => return 0.0,
    };
    let capacity = o.get("capacity").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let refill_per_sec = o.get("refillPerSec").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let last_ms = o.get("__lastMs").and_then(|v| v.as_f64()).unwrap_or(now);
    let tokens = o.get("__tokens").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let elapsed_ms = (now - last_ms).max(0.0);
    let replenish = elapsed_ms / 1000.0 * refill_per_sec;
    let new_tokens = (tokens + replenish).min(capacity);

    o.insert("__tokens", Value::float(new_tokens));
    o.insert("__lastMs", Value::float(now));

    new_tokens
}

// ── keyed limiter constructor + method dispatch ───────────────────────────────

impl Interp {
    /// Build a per-key token-bucket limiter policy object (§3.2.2).
    ///
    /// The bucket store is a real `std/lru` handle created via `new_lru_handle`,
    /// so recency and eviction are the shipped lru machinery.
    ///
    /// Fields:
    /// - `capacity`: positive integer (≥ 1), per-key bucket capacity
    /// - `refillPerSec`: non-negative finite number
    /// - `maxKeys`: optional positive int, default 10_000
    /// - `name`: optional string label, default "default"
    ///
    /// `__store` is the lru Native handle.
    fn make_keyed_limiter(&self, opts: Value, span: Span) -> Result<Value, Control> {
        let name = limiter_opt_name(&opts, span)?;
        let capacity = limiter_opt_capacity(&opts, span)?;
        let refill_per_sec = limiter_opt_refill_per_sec(&opts, span)?;
        let max_keys = keyed_opt_max_keys(&opts, span)?;

        // Create the real lru handle for the bucket store.
        let store = self.new_lru_handle(max_keys);

        let mut m: IndexMap<String, Value> = IndexMap::new();
        // kind tag (§2.2 — RESIL_KINDS already includes "keyedLimiter")
        m.insert("__resil".to_string(), Value::str("keyedLimiter"));
        // config fields
        m.insert("name".to_string(), Value::str(name));
        m.insert("capacity".to_string(), Value::float(capacity));
        m.insert("refillPerSec".to_string(), Value::float(refill_per_sec));
        m.insert("maxKeys".to_string(), Value::int(max_keys as i64));
        // the lru handle for per-key bucket storage
        m.insert("__store".to_string(), store);
        // non-sendable marker
        m.insert("__local".to_string(), local_marker());

        Ok(Value::object(m))
    }

    /// Dispatch a method on a `keyedLimiter` policy object.
    ///
    /// Supported methods: `tryAcquire(key, n=1)`, `acquire(key, n=1)`, `stats()`.
    ///
    /// The bucket store is the `__store` lru handle. Each bucket is an Object
    /// `{tokens: float, lastMs: float}`. Bucket refill/consume uses the SAME
    /// per-bucket formula as the plain limiter (`limiter_refill_bucket`).
    ///
    /// NO borrow is held across `.await` in `acquire`.
    async fn call_keyed_limiter_method(
        &self,
        recv: &Value,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        use crate::stdlib::time::real_monotonic_ms;

        match name {
            "tryAcquire" => {
                // args[0]=recv, args[1]=key, args[2]=n (optional)
                let key = args.get(1).cloned().unwrap_or(Value::nil());
                let key_str = keyed_validate_key(&key, span)?;
                let n = args.get(2).and_then(|v| v.as_f64()).unwrap_or(1.0);

                let now = self.clock_monotonic_ms(real_monotonic_ms());
                let (capacity, refill_per_sec, store_id) = keyed_read_config(recv);

                // Get or create bucket from lru store
                let bucket = keyed_get_bucket(self, store_id, &key_str, capacity, now, span)?;

                // Refill + try consume
                let new_tokens = limiter_refill_bucket(&bucket, capacity, refill_per_sec, now);
                if new_tokens >= n {
                    bucket_set_tokens(&bucket, new_tokens - n);
                    // Touch: set the updated bucket back into the lru (updates recency + tokens)
                    keyed_set_bucket(self, store_id, &key_str, &bucket, span)?;
                    Ok(Value::bool_(true))
                } else {
                    // Set back to persist the refill update even on rejection
                    keyed_set_bucket(self, store_id, &key_str, &bucket, span)?;
                    Ok(Value::bool_(false))
                }
            }

            "acquire" => {
                // args[0]=recv, args[1]=key, args[2]=n (optional)
                let key = args.get(1).cloned().unwrap_or(Value::nil());
                let key_str = keyed_validate_key(&key, span)?;
                let n = args.get(2).and_then(|v| v.as_f64()).unwrap_or(1.0);

                loop {
                    let now = self.clock_monotonic_ms(real_monotonic_ms());
                    let (capacity, refill_per_sec, store_id) = keyed_read_config(recv);

                    // Short synchronous section: refill + check
                    let bucket = keyed_get_bucket(self, store_id, &key_str, capacity, now, span)?;
                    let new_tokens = limiter_refill_bucket(&bucket, capacity, refill_per_sec, now);

                    if new_tokens >= n {
                        bucket_set_tokens(&bucket, new_tokens - n);
                        keyed_set_bucket(self, store_id, &key_str, &bucket, span)?;
                        return Ok(Value::nil());
                    }

                    // Compute deficit sleep, persist refill update, drop all borrows before await
                    bucket_set_tokens(&bucket, new_tokens);
                    keyed_set_bucket(self, store_id, &key_str, &bucket, span)?;

                    let deficit = n - new_tokens;
                    let sleep_ms = if refill_per_sec > 0.0 {
                        (deficit / refill_per_sec * 1000.0).max(1.0)
                    } else {
                        return Err(AsError::at(
                            "keyedLimiter.acquire: cannot acquire tokens from a zero-refill limiter",
                            span,
                        ).into());
                    };

                    // ── ALL borrows dropped here before .await ─────────────────
                    let duration = std::time::Duration::from_millis(sleep_ms as u64);
                    tokio::time::sleep(duration).await;
                    // Re-loop to re-check after sleep
                }
            }

            "stats" => {
                let (_, _, store_id) = keyed_read_config(recv);
                let keys = self.lru_len(store_id) as i64;
                let evictions = self.lru_eviction_count(store_id) as i64;

                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("keys".to_string(), Value::int(keys));
                m.insert("evictions".to_string(), Value::int(evictions));
                Ok(Value::object(m))
            }

            other => Err(AsError::at(
                format!("keyedLimiter policy has no method '{}'", other),
                span,
            ).into()),
        }
    }
}

// ── keyed-limiter helpers ─────────────────────────────────────────────────────

/// Extract `maxKeys` field (default 10_000; must be ≥ 1).
fn keyed_opt_max_keys(opts: &Value, span: Span) -> Result<usize, Control> {
    let v = match opts.kind() {
        ValueKind::Object(o) => o.get("maxKeys"),
        _ => return Ok(10_000),
    };
    match v {
        None => Ok(10_000),
        Some(v) => match v.as_f64() {
            Some(n) if n >= 1.0 => Ok(n as usize),
            Some(_) => Err(AsError::at(
                "keyedLimiter: 'maxKeys' must be a positive integer (>= 1)",
                span,
            ).into()),
            None => match v.kind() {
                ValueKind::Nil => Ok(10_000),
                _ => Err(AsError::at(
                    format!(
                        "keyedLimiter: 'maxKeys' must be a number, got {}",
                        crate::interp::type_name(&v)
                    ),
                    span,
                ).into()),
            },
        },
    }
}

/// Validate that a key is a `Value::Str`; return the string or Tier-2 panic.
fn keyed_validate_key(key: &Value, span: Span) -> Result<String, Control> {
    match key.kind() {
        ValueKind::Str(s) => Ok(s.to_string()),
        _ => Err(AsError::at(
            format!(
                "keyedLimiter: key must be a string, got {}",
                crate::interp::type_name(key)
            ),
            span,
        ).into()),
    }
}

/// Extract `(capacity, refillPerSec, store_id)` from the keyed limiter policy object.
/// Returns defaults on parse failures (should not happen with a valid policy object).
fn keyed_read_config(recv: &Value) -> (f64, f64, u64) {
    match recv.kind() {
        ValueKind::Object(o) => {
            let capacity = o.get("capacity").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let refill_per_sec = o.get("refillPerSec").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let store_id = o.get("__store")
                .and_then(|v| match v.kind() {
                    ValueKind::Native(n) => Some(n.id),
                    _ => None,
                })
                .unwrap_or(u64::MAX);
            (capacity, refill_per_sec, store_id)
        }
        _ => (1.0, 0.0, u64::MAX),
    }
}

/// Get or create a bucket `{tokens, lastMs}` for `key` from the lru store.
///
/// On a miss: returns a fresh full bucket (capacity tokens, `now` as lastMs).
/// On a hit: returns the stored bucket (after touching it for recency via the lru set path).
/// The bucket is returned as an owned `Value::Object`; callers must write it back via
/// `keyed_set_bucket` to persist any mutations.
fn keyed_get_bucket(
    interp: &Interp,
    store_id: u64,
    key: &str,
    capacity: f64,
    now: f64,
    _span: Span,
) -> Result<Value, Control> {
    use crate::interp::ResourceState;
    use crate::value::MapKey;

    let map_key = MapKey::Str(key.into());
    // Read from lru without touching recency (we'll touch on set after the refill)
    let existing = interp.with_resource(store_id, |r| match r {
        Some(ResourceState::Lru(s)) => {
            s.map.get(&map_key).cloned()
        }
        _ => None,
    });

    match existing {
        Some(bucket) => Ok(bucket),
        None => {
            // Fresh full bucket
            let mut b: IndexMap<String, Value> = IndexMap::new();
            b.insert("tokens".to_string(), Value::float(capacity));
            b.insert("lastMs".to_string(), Value::float(now));
            Ok(Value::object(b))
        }
    }
    .map_err(|e: Control| e) // infallible, but satisfies the signature
}

/// Write a bucket back to the lru store, updating recency via the lru set path.
///
/// This calls `call_lru_method` via the standard dispatch so the eviction machinery
/// and recency updates (touch-on-set) are handled by the shipped lru code.
fn keyed_set_bucket(
    interp: &Interp,
    store_id: u64,
    key: &str,
    bucket: &Value,
    _span: Span,
) -> Result<(), Control> {
    use crate::interp::ResourceState;
    use crate::value::MapKey;

    // Use the shipped lru `set` path directly (same as call_lru_method "set"):
    // This is inlined here to avoid constructing a NativeMethod just for one call.
    let map_key = MapKey::Str(key.into());
    interp.with_resource_mut(store_id, |r| {
        if let Some(ResourceState::Lru(s)) = r {
            if s.map.contains_key(&map_key) {
                // Update value + mark MRU (no eviction; size unchanged).
                s.map.insert(map_key.clone(), bucket.clone());
                s.touch(&map_key);
            } else {
                // Evict the LRU (front) entry if at capacity.
                while s.map.len() >= s.capacity && !s.map.is_empty() {
                    s.map.shift_remove_index(0);
                    s.eviction_count += 1;
                }
                s.map.insert(map_key, bucket.clone());
            }
        }
    });
    Ok(())
}

/// Refill a bucket object `{tokens, lastMs}` using the limiter formula.
///
/// Same formula as `limiter_refill` but operating on a standalone bucket Object
/// rather than the limiter policy Object directly.
/// Returns the post-refill token count AND mutates the bucket's `tokens`/`lastMs`.
fn limiter_refill_bucket(bucket: &Value, capacity: f64, refill_per_sec: f64, now: f64) -> f64 {
    let o = match bucket.kind() {
        ValueKind::Object(o) => o,
        _ => return 0.0,
    };
    let last_ms = o.get("lastMs").and_then(|v| v.as_f64()).unwrap_or(now);
    let tokens = o.get("tokens").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let elapsed_ms = (now - last_ms).max(0.0);
    let replenish = elapsed_ms / 1000.0 * refill_per_sec;
    let new_tokens = (tokens + replenish).min(capacity);

    o.insert("tokens", Value::float(new_tokens));
    o.insert("lastMs", Value::float(now));

    new_tokens
}

/// Update the `tokens` field of a bucket Object.
fn bucket_set_tokens(bucket: &Value, tokens: f64) {
    if let ValueKind::Object(o) = bucket.kind() {
        o.insert("tokens", Value::float(tokens));
    }
}

// ── bulkhead constructor + method dispatch ────────────────────────────────────

impl Interp {
    /// Build a bulkhead policy object from an options `Value::Object` (spec §3.3).
    ///
    /// Fields:
    /// - `limit`:  max concurrent executions (positive int ≥ 1; required).
    /// - `queue`:  max callers that may park waiting (non-negative int ≥ 0; default 0).
    /// - `name`:   optional string label (default `"default"`).
    ///
    /// The concurrency cap is backed by a real `sync.semaphore` handle (`__sem`)
    /// so the lost-wakeup-safe acquire loop is reused verbatim.
    /// `__waiting` tracks parked waiters synchronously (O(1) shed check).
    fn make_bulkhead(&self, opts: Value, span: Span) -> Result<Value, Control> {
        // ── extract & validate opts ───────────────────────────────────────────
        let name = match opts.kind() {
            ValueKind::Object(o) => match o.get("name") {
                Some(v) => match v.kind() {
                    ValueKind::Str(s) => s.to_string(),
                    ValueKind::Nil => "default".to_string(),
                    _ => return Err(AsError::at("bulkhead: 'name' must be a string", span).into()),
                },
                None => "default".to_string(),
            },
            ValueKind::Nil => "default".to_string(),
            _ => return Err(AsError::at("bulkhead: expected an options object", span).into()),
        };

        let limit: u64 = match opts.kind() {
            ValueKind::Object(o) => match o.get("limit") {
                None => {
                    return Err(AsError::at("bulkhead: 'limit' is required", span).into())
                }
                Some(v) => match v.as_f64() {
                    Some(n) if n >= 1.0 && n.fract() == 0.0 => n as u64,
                    _ => {
                        return Err(AsError::at(
                            "bulkhead: 'limit' must be a positive integer (>= 1)",
                            span,
                        )
                        .into())
                    }
                },
            },
            _ => return Err(AsError::at("bulkhead: 'limit' is required", span).into()),
        };

        let queue: u64 = match opts.kind() {
            ValueKind::Object(o) => match o.get("queue") {
                None => 0,
                Some(v) => match v.kind() {
                    ValueKind::Nil => 0,
                    _ => match v.as_f64() {
                        Some(n) if n >= 0.0 && n.fract() == 0.0 => n as u64,
                        _ => {
                            return Err(AsError::at(
                                "bulkhead: 'queue' must be a non-negative integer (>= 0)",
                                span,
                            )
                            .into())
                        }
                    },
                },
            },
            _ => 0,
        };

        // Create the semaphore handle (capacity = limit).
        let sem_val = self.sync_semaphore(
            &[Value::int(limit as i64)],
            span,
        )?;

        // ── build the policy object ───────────────────────────────────────────
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("__resil".to_string(), Value::str("bulkhead"));
        m.insert("name".to_string(), Value::str(name));
        m.insert("limit".to_string(), Value::int(limit as i64));
        m.insert("queue".to_string(), Value::int(queue as i64));
        // The real semaphore handle — concurrency cap
        m.insert("__sem".to_string(), sem_val);
        // Current parked waiters count (synchronous int counter, short borrows only)
        m.insert("__waiting".to_string(), Value::int(0));
        // Non-sendable marker (§2.2)
        m.insert("__local".to_string(), local_marker());

        Ok(Value::object(m))
    }

    /// `bulkhead.run(fn)` — the core policy method (spec §3.3).
    ///
    /// Three paths:
    /// 1. Permit immediately available (in-flight < limit): acquire (sync), run, release.
    /// 2. No permit AND `__waiting >= queue`: **immediate shed** — returns `[nil, {code:"bulkhead-full"}]`.
    /// 3. No permit AND `__waiting < queue`: `__waiting += 1`, park on semaphore, `__waiting -= 1`, run, release.
    ///
    /// Permit released on ALL exit paths (success, error pair, panic, propagate).
    /// `__waiting` decremented on ALL wait-path exits.
    /// No `RefCell`/ObjectCell borrow held across `.await`.
    async fn bulkhead_run(
        &self,
        recv: &Value,
        user_fn: Value,
        span: Span,
    ) -> Result<Value, Control> {
        use crate::interp::make_pair;
        use super::sync::get_semaphore;

        // ── Step 1: Read config + state under short synchronous borrow ────────
        let (sem_id, queue, waiting) = {
            match recv.kind() {
                ValueKind::Object(o) => {
                    let sem_id = o.get("__sem")
                        .and_then(|v| match v.kind() {
                            ValueKind::Native(n) => Some(n.id),
                            _ => None,
                        })
                        .unwrap_or(u64::MAX);
                    let queue = o.get("queue").and_then(|v| v.as_int()).unwrap_or(0);
                    let waiting = o.get("__waiting").and_then(|v| v.as_int()).unwrap_or(0);
                    (sem_id, queue, waiting)
                }
                _ => (u64::MAX, 0, 0),
            }
        };

        // ── Step 2: Check current in-flight by inspecting semaphore available ──
        // available == 0 means limit reached.
        let sem = match get_semaphore(self, sem_id) {
            Some(s) => s,
            None => {
                return Err(AsError::at("bulkhead: internal semaphore is invalid", span).into());
            }
        };
        let available = *sem.available.borrow();

        // ── Step 3: Admit or shed decision ────────────────────────────────────
        let needs_wait = available == 0;

        if needs_wait {
            if waiting >= queue {
                // O(1) immediate shed — no parking
                let name = match recv.kind() {
                    ValueKind::Object(o) => o
                        .get("name")
                        .and_then(|v| match v.kind() {
                            ValueKind::Str(s) => Some(s.to_string()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "default".to_string()),
                    _ => "default".to_string(),
                };
                let mut err: IndexMap<String, Value> = IndexMap::new();
                err.insert(
                    "message".to_string(),
                    Value::str(format!("bulkhead '{}' queue is full", name)),
                );
                err.insert("code".to_string(), Value::str("bulkhead-full"));
                return Ok(make_pair(Value::nil(), Value::object(err)));
            }

            // Park path: increment waiting (sync, no borrow across await below)
            if let ValueKind::Object(o) = recv.kind() {
                o.insert("__waiting", Value::int(waiting + 1));
            }
        }

        // ── Step 4: Acquire the permit (may park if needs_wait) ────────────────
        // Build a Value::Native(Semaphore) slice reference to pass to sync_acquire.
        // We already have `sem_id`; reconstitute the Value we need to pass.
        let sem_value = match recv.kind() {
            ValueKind::Object(o) => o.get("__sem").unwrap_or(Value::nil()),
            _ => Value::nil(),
        };

        let acquire_result = self
            .sync_acquire(std::slice::from_ref(&sem_value), span)
            .await;

        // If acquisition failed (shouldn't in normal operation), clean up waiting and propagate.
        if let Err(e) = acquire_result {
            if needs_wait {
                if let ValueKind::Object(o) = recv.kind() {
                    let w = o.get("__waiting").and_then(|v| v.as_int()).unwrap_or(1);
                    o.insert("__waiting", Value::int((w - 1).max(0)));
                }
            }
            return Err(e);
        }

        // Acquired the permit — decrement waiting counter (if we were waiting)
        if needs_wait {
            if let ValueKind::Object(o) = recv.kind() {
                let w = o.get("__waiting").and_then(|v| v.as_int()).unwrap_or(1);
                o.insert("__waiting", Value::int((w - 1).max(0)));
            }
        }

        // ── Step 5: Call the user fn (NO borrow held across .await) ──────────
        let call_result = self.call_value(user_fn, vec![], span).await;
        // Drive a returned future to completion (the task.retry / breaker pattern)
        let outcome = match call_result {
            Ok(v) => match v.kind() {
                ValueKind::Future(_) => {
                    let crate::value::OwnedKind::Future(f) = v.into_kind() else {
                        unreachable!()
                    };
                    f.get().await
                }
                _ => Ok(v),
            },
            other => other,
        };

        // ── Step 6: Release the permit on ALL exit paths ──────────────────────
        // sync_release never fails (it only increments the counter and notify_one).
        let _ = self.sync_release(&[sem_value], span);

        // Return the outcome:
        // - success: normalize plain value → [v, nil]; already-a-pair passes through.
        // - err pair: pass through.
        // - panic: re-raise (never swallowed — caller uses recover).
        // - propagate/exit: pass through.
        match outcome {
            Ok(v) => {
                // Normalize: plain non-pair value → [v, nil]; 2-element array passes through.
                let is_pair = match v.kind() {
                    ValueKind::Array(a) => a.borrow().len() == 2,
                    _ => false,
                };
                if is_pair {
                    Ok(v)
                } else {
                    Ok(crate::interp::make_pair(v, Value::nil()))
                }
            }
            other => other,
        }
    }

    /// Dispatch a method call on a `bulkhead` policy object.
    async fn call_bulkhead_method(
        &self,
        recv: &Value,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match name {
            "run" => {
                // args[0] = receiver, args[1] = the fn to run
                let user_fn = args.get(1).cloned().unwrap_or(Value::nil());
                self.bulkhead_run(recv, user_fn, span).await
            }
            other => Err(AsError::at(
                format!("bulkhead policy has no method '{}'", other),
                span,
            )
            .into()),
        }
    }
}

// ── retry policy constructor + method dispatch (§3.4) ─────────────────────────

/// Build a reusable retry policy object (§3.4).
///
/// The policy carries the parsed retry config (validated up front via the shared
/// `parse_retry_config`), a `budget` ratio in (0, 1], and the count-based budget
/// state (`__attemptsSeen`/`__retriesSpent`). `p.call(fn)` routes to the SAME
/// `Interp::retry_engine` `task.retry` uses, with this policy as the budget state.
///
/// `budget`, when present, must be a number in (0, 1] (Tier-2 otherwise). When
/// absent, the budget never blocks a retry (treated as 1.0 — `attempts` bounds it).
fn make_retry_policy(opts: Value, span: Span) -> Result<Value, Control> {
    // Validate the retry config up front (shared with task.retry; same messages).
    // `budget` is a policy-only key parse_retry_config ignores — validated below.
    let _cfg = crate::stdlib::task_mod::parse_retry_config(&opts, span)?;

    // Parse + validate the budget ratio (policy-only).
    let budget = match opts.kind() {
        ValueKind::Object(o) => match o.get("budget") {
            None => 1.0,
            Some(v) => match v.kind() {
                ValueKind::Nil => 1.0,
                _ => match v.as_f64() {
                    Some(n) if n > 0.0 && n <= 1.0 => n,
                    _ => {
                        return Err(AsError::at(
                            "resilience.retry: budget must be a number in (0, 1]",
                            span,
                        )
                        .into())
                    }
                },
            },
        },
        ValueKind::Nil => 1.0,
        _ => {
            return Err(AsError::at(
                "resilience.retry: expected an options object or nil",
                span,
            )
            .into())
        }
    };

    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert("__resil".to_string(), Value::str("retry"));
    // Stash the original opts so `call` re-derives the config via the shared parser
    // (keeps ONE source of truth for the retry config, incl. the `retryIf` callable).
    m.insert(
        "__opts".to_string(),
        match opts.kind() {
            ValueKind::Nil => Value::nil(),
            _ => opts.clone(),
        },
    );
    // Budget ratio + count-based state.
    m.insert("budget".to_string(), Value::float(budget));
    m.insert("__attemptsSeen".to_string(), Value::int(0));
    m.insert("__retriesSpent".to_string(), Value::int(0));
    // non-sendable marker (§2.2)
    m.insert("__local".to_string(), local_marker());

    Ok(Value::object(m))
}

impl Interp {
    /// Dispatch a method call on a `retry` policy object (§3.4).
    ///
    /// `p.call(fn)` re-derives the `RetryConfig` from the stashed `__opts` and drives
    /// `retry_engine` with the policy as the count-based budget state. `p.stats()`
    /// returns `{attemptsSeen, retriesSpent, budget}`. `p.reset()` zeroes the budget
    /// counters.
    async fn call_retry_method(
        &self,
        recv: &Value,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match name {
            "call" => {
                let user_fn = args.get(1).cloned().unwrap_or(Value::nil());
                let opts = match recv.kind() {
                    ValueKind::Object(o) => o.get("__opts").unwrap_or(Value::nil()),
                    _ => Value::nil(),
                };
                let cfg = crate::stdlib::task_mod::parse_retry_config(&opts, span)?;
                self.retry_engine(user_fn, &cfg, Some(recv), span).await
            }
            "stats" => {
                let (seen, spent, budget) = match recv.kind() {
                    ValueKind::Object(o) => (
                        o.get("__attemptsSeen").unwrap_or(Value::int(0)),
                        o.get("__retriesSpent").unwrap_or(Value::int(0)),
                        o.get("budget").unwrap_or(Value::float(1.0)),
                    ),
                    _ => (Value::int(0), Value::int(0), Value::float(1.0)),
                };
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("attemptsSeen".to_string(), seen);
                m.insert("retriesSpent".to_string(), spent);
                m.insert("budget".to_string(), budget);
                Ok(Value::object(m))
            }
            "reset" => {
                if let ValueKind::Object(o) = recv.kind() {
                    o.insert("__attemptsSeen", Value::int(0));
                    o.insert("__retriesSpent", Value::int(0));
                }
                Ok(Value::nil())
            }
            other => Err(AsError::at(
                format!("retry policy has no method '{}'", other),
                span,
            )
            .into()),
        }
    }
}

// ── Interp::call_fallback (§3.5) ─────────────────────────────────────────────

impl Interp {
    /// `resilience.fallback(fn, fb)` — runs `fn`; on success passes through as
    /// `[v, nil]`; on an err pair calls `fb(err)`; on a panic calls `fb({message})`
    /// (consuming the panic — this is the ONE documented place RESIL swallows a
    /// `Control::Panic`). `fb`'s result is normalized to a pair; if `fb` itself
    /// panics that re-raises. Async `fn`/`fb` are driven.
    async fn call_fallback(
        &self,
        user_fn: Value,
        fb: Value,
        span: Span,
    ) -> Result<Value, Control> {
        use crate::interp::{make_error, make_pair, result_pair_err};

        // ── Step 1: drive fn (await if it returns a future) ───────────────────
        let raw = self.call_value(user_fn, vec![], span).await;

        // Drive any returned future to completion (mirror of the breaker/retry pattern).
        let outcome: Result<Value, Control> = match raw {
            Ok(v) => {
                match v.kind() {
                    crate::value::ValueKind::Future(_) => {
                        let crate::value::OwnedKind::Future(f) = v.into_kind() else {
                            unreachable!()
                        };
                        f.get().await
                    }
                    _ => Ok(v),
                }
            }
            other => other,
        };

        // ── Step 2: classify outcome ──────────────────────────────────────────
        let fb_arg: Value = match outcome {
            // Propagate: pass through unchanged (not a fn-level panic/error).
            Err(Control::Propagate(pair)) => return Err(Control::Propagate(pair)),
            // Exit: pass through unchanged.
            Err(Control::Exit(code)) => return Err(Control::Exit(code)),

            // Panic: consume the panic, build {message: <msg>} error object for fb.
            // Use the CLEAN message (not Display, which appends "(at start..end)" byte
            // offsets) so fb receives the same shape recover()/retryIf get — §3.5.
            Err(Control::Panic(e)) => make_error(Value::str(e.message.clone())),

            // Ok value: check whether it is an err-pair.
            Ok(v) => {
                match result_pair_err(&v) {
                    // err-pair [nil, err]: call fb with the err object.
                    Some(err) => err,
                    // success (ok-pair or plain value): pass through as [v, nil].
                    None => return Ok(make_pair(v, Value::nil())),
                }
            }
        };

        // ── Step 3: call fb(err_arg) — fb panics propagate unchanged ──────────
        let fb_raw = self.call_value(fb, vec![fb_arg], span).await;

        // Drive any future returned by fb.
        let fb_outcome: Result<Value, Control> = match fb_raw {
            Ok(v) => {
                match v.kind() {
                    crate::value::ValueKind::Future(_) => {
                        let crate::value::OwnedKind::Future(f) = v.into_kind() else {
                            unreachable!()
                        };
                        f.get().await
                    }
                    _ => Ok(v),
                }
            }
            other => other,
        };

        // ── Step 4: normalize fb result to a pair ─────────────────────────────
        match fb_outcome {
            // fb panics re-raise.
            Err(e) => Err(e),
            Ok(v) => {
                // If fb returned an err-pair already, return as-is.
                // If fb returned a plain value (or ok-pair), wrap as [v, nil].
                if result_pair_err(&v).is_some() {
                    Ok(v)
                } else {
                    Ok(make_pair(v, Value::nil()))
                }
            }
        }
    }
}

// ── Interp::call_resilience ───────────────────────────────────────────────────

impl Interp {
    /// Dispatch a `resilience.*` stdlib call.
    ///
    /// Mirrors `Interp::call_schema` in structure.
    pub(crate) async fn call_resilience(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "breaker" => {
                let opts = arg(args, 0);
                make_breaker(opts, span)
            }
            "limiter" => {
                let opts = arg(args, 0);
                make_limiter(opts, span)
            }
            "keyedLimiter" => {
                let opts = arg(args, 0);
                self.make_keyed_limiter(opts, span)
            }
            "bulkhead" => {
                let opts = arg(args, 0);
                self.make_bulkhead(opts, span)
            }
            "retry" => {
                let opts = arg(args, 0);
                make_retry_policy(opts, span)
            }
            "fallback" => {
                let user_fn = arg(args, 0);
                let fb = arg(args, 1);
                self.call_fallback(user_fn, fb, span).await
            }
            other => Err(AsError::at(
                format!("resilience.{}: not implemented in this build", other),
                span,
            )
            .into()),
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    #[tokio::test]
    async fn module_imports_and_constructs_breaker() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({name: "t", failureRate: 0.5, window: 4, minCalls: 2, cooldownMs: 1000, halfOpenMax: 1})
print(b.__resil)
print(b.failureRate)
"#)
        .await;
        assert_eq!(out, "breaker\n0.5\n");
    }

    #[tokio::test]
    async fn breaker_defaults() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({})
print(b.name)
print(b.failureRate)
print(b.window)
print(b.minCalls)
print(b.cooldownMs)
print(b.halfOpenMax)
print(b.__state)
"#)
        .await;
        assert_eq!(out, "default\n0.5\n20\n10\n30000\n3\nclosed\n");
    }

    #[tokio::test]
    async fn breaker_nil_opts_uses_defaults() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker(nil)
print(b.name)
print(b.__state)
"#)
        .await;
        assert_eq!(out, "default\nclosed\n");
    }

    #[tokio::test]
    async fn breaker_invalid_failure_rate_panics() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
resilience.breaker({failureRate: 0.0})
"#)
        .await;
        assert!(
            res.is_err() || res.unwrap().contains("failureRate"),
            "expected a panic about failureRate"
        );
    }

    #[tokio::test]
    async fn breaker_zero_window_panics() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
resilience.breaker({window: 0})
"#)
        .await;
        assert!(
            res.is_err() || res.unwrap().contains("window"),
            "expected a panic about window"
        );
    }

    // ── hook tests (Task 1.2) ─────────────────────────────────────────────────

    /// Hook routes `.state()`, `.stats()`, `.reset()` calls on a breaker.
    #[tokio::test]
    async fn hook_routes_method_calls() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({window: 4, minCalls: 2})
print(b.state())
let s = b.stats()
print(s.calls)
print(s.failures)
print(s.rejected)
b.reset()
print(b.state())
"#)
        .await;
        assert_eq!(out, "closed\n0\n0\n0\nclosed\n");
    }

    /// A bare member read (not a call) still reads the stored field, not the hook.
    /// `b.state` (no parens) should return `nil` (the __state field is named `__state`,
    /// and `state` is not a config field — so it reads nil).
    #[tokio::test]
    async fn hook_call_position_only_bare_read_not_routed() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({window: 4, minCalls: 2})
let bare = b.state
print(type(bare))
"#)
        .await;
        // bare member read of a non-existent field returns nil
        assert_eq!(out, "nil\n");
    }

    /// A method in the union set but not valid for a breaker raises a Tier-2 panic.
    /// `acquire` is a limiter method — calling it on a breaker should panic.
    #[tokio::test]
    async fn hook_wrong_method_for_kind_is_tier2() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({window: 4, minCalls: 2})
b.acquire()
"#)
        .await;
        // run_source returns Err(AsError) for a Tier-2 panic.
        let msg = match res {
            Err(e) => format!("{e}"),
            Ok(s) => s,
        };
        assert!(
            msg.contains("breaker policy has no method 'acquire'"),
            "expected breaker method-mismatch message, got: {msg:?}"
        );
    }

    /// OptMember (`b?.state(...)`) does NOT route through the hook.
    /// It reads the stored `state` field (nil) and the call is never made.
    #[tokio::test]
    async fn hook_opt_member_does_not_route() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({window: 4, minCalls: 2})
print(b?.state)
"#)
        .await;
        // OptMember reads the object field `state` (absent → nil), not the method
        assert_eq!(out, "nil\n");
    }

    /// An object that carries `__resil` with a BOGUS kind is NOT hijacked.
    #[tokio::test]
    async fn hook_bogus_resil_not_hijacked() {
        let out = run(r#"
let fake = {__resil: "notAKind", state: "whatever"}
print(fake.state)
"#)
        .await;
        assert_eq!(out, "whatever\n");
    }

    /// An unimplemented kind raises a Tier-2 panic with the kind name.
    #[tokio::test]
    async fn hook_unimplemented_kind_error() {
        // Call a hook-dispatched method name (state) that bulkhead does not implement.
        // Verifies the call_bulkhead_method fallthrough path returns the right error.
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
let bh = resilience.bulkhead({limit: 1})
bh.state()
"#)
        .await;
        // run_source returns Err(AsError) for a Tier-2 panic.
        let msg = match res {
            Err(e) => format!("{e}"),
            Ok(s) => s,
        };
        assert!(
            msg.contains("bulkhead policy has no method 'state'"),
            "expected bulkhead kind error, got: {msg:?}"
        );
    }

    // ── Task 1.3: breaker.call(fn) state machine tests ───────────────────────

    /// 2/4 failures (0.5 threshold, window=4, minCalls=4) → breaker opens.
    /// After opening, a `call` returns `[nil, {code:"breaker-open"}]` and
    /// `rejected` increments; the original failures are not in the rejected counter.
    #[tokio::test]
    async fn breaker_opens_at_threshold_and_rejects_with_code() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({failureRate: 0.5, window: 4, minCalls: 4, cooldownMs: 999999, halfOpenMax: 1})
// 2 successes, 2 failures = exactly 50% failure rate → should open after 4th call
fn ok() { return 42 }
fn fail() { return [nil, {message: "boom", code: "err"}] }
let [v1, e1] = b.call(ok)
let [v2, e2] = b.call(ok)
let [v3, e3] = b.call(fail)
let [v4, e4] = b.call(fail)
print(b.state())
// next call should be rejected
let [rv, re] = b.call(ok)
print(re.code)
print(b.stats().rejected)
"#).await;
        assert_eq!(out, "open\nbreaker-open\n1\n");
    }

    /// Window=4, minCalls=4, only 3 calls (all failures) → stays closed
    /// because we haven't accumulated `minCalls` yet.
    #[tokio::test]
    async fn breaker_min_calls_not_reached_stays_closed() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({failureRate: 0.5, window: 4, minCalls: 4, cooldownMs: 999999, halfOpenMax: 1})
fn fail() { return [nil, {message: "boom", code: "err"}] }
b.call(fail)
b.call(fail)
b.call(fail)
print(b.state())
"#).await;
        assert_eq!(out, "closed\n");
    }

    /// 1 failure / 4 calls with 0.5 threshold → stays closed (below threshold).
    #[tokio::test]
    async fn breaker_rate_below_threshold_stays_closed() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({failureRate: 0.5, window: 4, minCalls: 4, cooldownMs: 999999, halfOpenMax: 1})
fn ok() { return 42 }
fn fail() { return [nil, {message: "boom", code: "err"}] }
b.call(ok)
b.call(ok)
b.call(ok)
b.call(fail)
print(b.state())
"#).await;
        assert_eq!(out, "closed\n");
    }

    /// A fn that panics: failure is counted AND panic re-raised.
    #[tokio::test]
    async fn breaker_panic_counts_as_failure_and_reraises() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({failureRate: 1.0, window: 2, minCalls: 1, cooldownMs: 999999, halfOpenMax: 1})
fn panic_fn() { error("boom") }
// recover sees the panic re-raised
let result = recover(() => b.call(panic_fn))
print(type(result))
print(b.stats().failures)
"#).await;
        assert_eq!(out, "array\n1\n");
    }

    /// A fn that returns a `?`-propagate pair: run_body converts the Propagate to
    /// Ok(pair) at the fn boundary, so b.call receives an error pair and records it
    /// as a failure — same as a direct `return [nil, err]`.
    #[tokio::test]
    async fn breaker_propagate_passes_through_unrecorded() {
        // `?` inside a called fn is converted to Ok([nil, err]) by run_body at the
        // fn call boundary (interp.rs: Propagate => Ok(v)).  So b.call counts it
        // as a failure just like a plain error-pair return.  This test verifies
        // that two such "propagate" calls open the breaker at the threshold.
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({failureRate: 0.5, window: 4, minCalls: 2, cooldownMs: 999999, halfOpenMax: 1})
fn prop_fn() {
    let pair = [nil, {message: "propagated", code: "prop"}]
    return pair?
}
b.call(prop_fn)
b.call(prop_fn)
print(b.stats().calls)
print(b.stats().failures)
print(b.state())
"#).await;
        // Both calls are recorded as failures (error pairs); breaker opens.
        assert_eq!(out, "2\n2\nopen\n");
    }

    /// Rejected calls are not entered in the ring window.
    #[tokio::test]
    async fn breaker_rejected_calls_not_in_window() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({failureRate: 0.5, window: 4, minCalls: 2, cooldownMs: 999999, halfOpenMax: 1})
// Force open by two failures in a 2-minCalls window
fn fail() { return [nil, {message: "boom", code: "err"}] }
b.call(fail)
b.call(fail)
print(b.state())
// Rejected calls should not increment __calls
let s1 = b.stats()
b.call(fail)
b.call(fail)
let s2 = b.stats()
print(s2.calls - s1.calls)
print(s2.rejected - s1.rejected)
"#).await;
        assert_eq!(out, "open\n0\n2\n");
    }

    /// After opening, reset() → closed, ring cleared.
    #[tokio::test]
    async fn breaker_reset_returns_to_closed() {
        let out = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({failureRate: 0.5, window: 4, minCalls: 2, cooldownMs: 999999, halfOpenMax: 1})
fn fail() { return [nil, {message: "boom", code: "err"}] }
b.call(fail)
b.call(fail)
print(b.state())
b.reset()
print(b.state())
print(b.stats().calls)
print(b.stats().failures)
"#).await;
        assert_eq!(out, "open\nclosed\n0\n0\n");
    }

    /// Cooldown: sleep past cooldownMs → next call is a probe → state = "halfOpen";
    /// probe success × halfOpenMax → closed.
    #[tokio::test]
    async fn breaker_cooldown_half_open() {
        // Use run_source_deterministic with a virtual clock so we don't actually sleep
        let out = crate::run_source_deterministic(r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
let b = resilience.breaker({failureRate: 0.5, window: 4, minCalls: 2, cooldownMs: 100, halfOpenMax: 2})
fn fail() { return [nil, {message: "boom", code: "err"}] }
fn ok() { return 42 }
// Force open
b.call(fail)
b.call(fail)
print(b.state())
// Advance clock past cooldown
await time.sleep(200)
// Next call should transition to halfOpen (probe)
b.call(ok)
print(b.state())
// Second successful probe → closed
b.call(ok)
print(b.state())
"#, 42).await.expect("deterministic run should succeed");
        assert_eq!(out, "open\nhalfOpen\nclosed\n");
    }

    // ── Task 2.1: limiter tests (TDD — add BEFORE implementation) ──────────────

    /// Capacity exhaustion determinism: capacity=2 → first two tryAcquire() true, third false.
    #[tokio::test]
    async fn limiter_capacity_exhaustion_deterministic() {
        let out = run(r#"
import * as resilience from "std/resilience"
let lim = resilience.limiter({capacity: 2, refillPerSec: 0.001})
print(lim.tryAcquire())
print(lim.tryAcquire())
print(lim.tryAcquire())
"#).await;
        assert_eq!(out, "true\ntrue\nfalse\n");
    }

    /// tryAcquire(n) atomicity: 5 tokens → tryAcquire(5) true (all 5 consumed);
    /// then 4 remaining tokens → tryAcquire(5) false AND none consumed.
    #[tokio::test]
    async fn limiter_try_acquire_n_atomicity() {
        let out = run(r#"
import * as resilience from "std/resilience"
let lim = resilience.limiter({capacity: 5, refillPerSec: 0.0})
let r1 = lim.tryAcquire(5)
print(r1)
// Re-fill 4 tokens manually by creating a fresh limiter with 4 capacity
let lim2 = resilience.limiter({capacity: 4, refillPerSec: 0.0})
let r2 = lim2.tryAcquire(5)
print(r2)
// Verify 4 tokens still present (can still take 4)
let r3 = lim2.tryAcquire(4)
print(r3)
"#).await;
        assert_eq!(out, "true\nfalse\ntrue\n");
    }

    /// Refill under virtual clock: capacity=1, consume it, advance clock 2s → tryAcquire() true.
    #[tokio::test]
    async fn limiter_refill_virtual_clock() {
        let out = crate::run_source_deterministic(r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
let lim = resilience.limiter({capacity: 1, refillPerSec: 1000})
print(lim.tryAcquire())
print(lim.tryAcquire())
await time.sleep(2)
print(lim.tryAcquire())
"#, 42).await.expect("deterministic run should succeed");
        assert_eq!(out, "true\nfalse\ntrue\n");
    }

    /// acquire integration: capacity=1, high refillPerSec → two sequential await lim.acquire()
    /// both complete in real time (the sleep is timing-only per §3.2.1).
    /// Uses a high refillPerSec (10000/sec = 0.1ms/token) so the test completes quickly.
    #[tokio::test]
    async fn limiter_acquire_integration() {
        let out = crate::run_source(r#"
import * as resilience from "std/resilience"
let lim = resilience.limiter({capacity: 1, refillPerSec: 10000})
await lim.acquire()
print("first")
await lim.acquire()
print("second")
"#).await.expect("acquire integration should succeed");
        assert_eq!(out, "first\nsecond\n");
    }

    /// Validation panics (Tier-2): capacity=0, negative refillPerSec, non-number args.
    #[tokio::test]
    async fn limiter_validation_panics() {
        // capacity: 0
        let r1 = crate::run_source(r#"
import * as resilience from "std/resilience"
resilience.limiter({capacity: 0, refillPerSec: 10})
"#).await;
        let msg1 = match r1 { Err(e) => format!("{e}"), Ok(s) => s };
        assert!(msg1.contains("capacity"), "expected capacity panic, got: {msg1:?}");

        // negative refillPerSec
        let r2 = crate::run_source(r#"
import * as resilience from "std/resilience"
resilience.limiter({capacity: 10, refillPerSec: -1})
"#).await;
        let msg2 = match r2 { Err(e) => format!("{e}"), Ok(s) => s };
        assert!(msg2.contains("refillPerSec"), "expected refillPerSec panic, got: {msg2:?}");

        // non-number capacity
        let r3 = crate::run_source(r#"
import * as resilience from "std/resilience"
resilience.limiter({capacity: "ten", refillPerSec: 10})
"#).await;
        let msg3 = match r3 { Err(e) => format!("{e}"), Ok(s) => s };
        assert!(msg3.contains("capacity"), "expected capacity type panic, got: {msg3:?}");
    }

    /// halfOpenMax=2: two sequential probes both succeed → breaker re-closes.
    /// halfOpenMax=1: first probe fails → re-opens; second call is rejected.
    #[tokio::test]
    async fn breaker_half_open_probe_budget() {
        // Part A: halfOpenMax=2, both probes succeed → closed
        let out_a = crate::run_source_deterministic(r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
let b = resilience.breaker({failureRate: 0.5, window: 4, minCalls: 2, cooldownMs: 100, halfOpenMax: 2})
fn fail() { return [nil, {message: "boom", code: "err"}] }
fn ok() { return 42 }
b.call(fail)
b.call(fail)
print(b.state())
await time.sleep(200)
b.call(ok)
print(b.state())
b.call(ok)
print(b.state())
"#, 42).await.expect("deterministic run should succeed");
        assert_eq!(out_a, "open\nhalfOpen\nclosed\n");

        // Part B: halfOpenMax=1; first probe fails → re-opens; next call rejected.
        let out_b = crate::run_source_deterministic(r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
let b = resilience.breaker({failureRate: 0.5, window: 4, minCalls: 2, cooldownMs: 100, halfOpenMax: 1})
fn fail() { return [nil, {message: "boom", code: "err"}] }
b.call(fail)
b.call(fail)
print(b.state())
await time.sleep(200)
// First call: open→halfOpen, admitted as probe, fails → re-opens
let r1 = b.call(fail)
print(b.state())
// Second call: open again, cooldown NOT elapsed → rejected
let r2 = b.call(fail)
print(r2[1].code)
"#, 42).await.expect("deterministic run should succeed");
        assert_eq!(out_b, "open\nopen\nbreaker-open\n");
    }

    // ── Task 2.2: keyedLimiter tests ──────────────────────────────────────────

    /// Per-key isolation: exhausting key "A" does not affect key "B".
    #[tokio::test]
    async fn keyed_limiter_per_key_isolation() {
        let out = run(r#"
import * as resilience from "std/resilience"
let kl = resilience.keyedLimiter({capacity: 2, refillPerSec: 0.001, maxKeys: 100})
// Exhaust key "A"
print(kl.tryAcquire("A"))   // true
print(kl.tryAcquire("A"))   // true
print(kl.tryAcquire("A"))   // false (exhausted)
// Key "B" should have its own FULL bucket
print(kl.tryAcquire("B"))   // true
print(kl.tryAcquire("B"))   // true
print(kl.tryAcquire("B"))   // false
"#).await;
        assert_eq!(out, "true\ntrue\nfalse\ntrue\ntrue\nfalse\n");
    }

    /// Documented eviction: maxKeys=2; exhaust A, touch B, touch C (→ A evicted).
    /// A's next tryAcquire returns true on a FULL bucket (re-created fresh).
    /// stats().evictions incremented.
    #[tokio::test]
    async fn keyed_limiter_eviction_resets_bucket_and_counts() {
        let out = run(r#"
import * as resilience from "std/resilience"
let kl = resilience.keyedLimiter({capacity: 3, refillPerSec: 0.0, maxKeys: 2})
// Exhaust key "A"
kl.tryAcquire("A")
kl.tryAcquire("A")
kl.tryAcquire("A")
// A is exhausted; now touch B and C to evict A (maxKeys=2)
kl.tryAcquire("B")
kl.tryAcquire("C")   // C admission evicts LRU ("A")
let s1 = kl.stats()
print(s1.evictions >= 1)   // at least 1 eviction
// A should now have a fresh full bucket (capacity=3)
print(kl.tryAcquire("A"))  // true (fresh bucket)
print(kl.tryAcquire("A"))  // true
print(kl.tryAcquire("A"))  // true
print(kl.tryAcquire("A"))  // false (exhausted again)
"#).await;
        assert_eq!(out, "true\ntrue\ntrue\ntrue\nfalse\n");
    }

    /// stats() returns {keys, evictions} reflecting current lru state.
    #[tokio::test]
    async fn keyed_limiter_stats() {
        let out = run(r#"
import * as resilience from "std/resilience"
let kl = resilience.keyedLimiter({capacity: 5, refillPerSec: 0.0, maxKeys: 10})
kl.tryAcquire("X")
kl.tryAcquire("Y")
let s = kl.stats()
print(s.keys)      // 2 (two distinct buckets stored)
print(s.evictions) // 0 (no evictions yet)
"#).await;
        assert_eq!(out, "2\n0\n");
    }

    /// Non-string key → Tier-2 panic.
    #[tokio::test]
    async fn keyed_limiter_non_string_key_panics() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
let kl = resilience.keyedLimiter({capacity: 5, refillPerSec: 0.0, maxKeys: 10})
kl.tryAcquire(42)
"#).await;
        let msg = match res {
            Err(e) => format!("{e}"),
            Ok(s) => s,
        };
        assert!(
            msg.contains("key must be a string"),
            "expected string-key panic, got: {msg:?}"
        );
    }

    // ── Task 2.3: bulkhead tests ──────────────────────────────────────────────

    /// Cap honored: limit=2, three concurrent bh.run() of a parking async fn →
    /// at most 2 in-flight at once (third parks then proceeds when one finishes).
    #[tokio::test]
    async fn bulkhead_cap_honored() {
        let out = run(r#"
import * as resilience from "std/resilience"
import { channel, send, recv } from "std/sync"
import { spawn, gather } from "std/task"

let bh = resilience.bulkhead({limit: 2, queue: 10})
let in_flight = channel()   // signals entry
let release = channel()     // gate to control exit

async fn worker(id) {
    await send(in_flight, id)       // signal entry
    await recv(release)             // park until released
}

// Wrap in async closures so spawn gets a Future (bh.run is a hook call, not an async fn literal)
let t1 = spawn(async () => bh.run(() => worker(1)))
let t2 = spawn(async () => bh.run(() => worker(2)))
let t3 = spawn(async () => bh.run(() => worker(3)))

// Drain 2 in-flight signals — exactly 2 should have started (limit=2)
let id1 = await recv(in_flight)
let id2 = await recv(in_flight)
// Release one so the third can enter
await send(release, "go")
// Third should start now (got a permit)
let id3 = await recv(in_flight)
// Release remaining two
await send(release, "go")
await send(release, "go")

await gather([t1, t2, t3])
// If we got here without deadlock, at most 2 were in-flight simultaneously
print("ok")
"#).await;
        assert_eq!(out, "ok\n");
    }

    /// Queue boundary: limit=1, queue=1 → first parks, second parks (queued),
    /// third gets immediate [nil, {code:"bulkhead-full"}] (shed before first finishes).
    #[tokio::test]
    async fn bulkhead_queue_boundary_shed() {
        let out = run(r#"
import * as resilience from "std/resilience"
import { channel, send, recv } from "std/sync"
import { spawn, gather } from "std/task"
import * as time from "std/time"

let bh = resilience.bulkhead({limit: 1, queue: 1, name: "test"})
let gate = channel()
let entered = channel()

async fn slow_fn() {
    await send(entered, "in")
    await recv(gate)
    return "done"
}

// First: acquires the permit, parks on gate
let t1 = spawn(async () => bh.run(slow_fn))
let _ = await recv(entered)   // wait for t1 to be in-flight

// Second: queued (limit=1 full, queue=1 empty) — spawn and give it a tick
let t2 = spawn(async () => bh.run(slow_fn))
// Yield to let t2 enter the waiting state (increment __waiting)
await time.sleep(5)

// Third: immediate shed — queue is full (queue=1, 1 already waiting)
// Call synchronously (no spawn) — the shed path returns immediately without parking
let [v3, e3] = bh.run(slow_fn)
print(e3.code)   // bulkhead-full

// Release the gate so t1 and t2 can finish
await send(gate, "go")
await send(gate, "go")
await t1
await t2
print("done")
"#).await;
        assert_eq!(out, "bulkhead-full\ndone\n");
    }

    /// All-paths release: panicking fn → permit released → subsequent run succeeds.
    #[tokio::test]
    async fn bulkhead_all_paths_release_on_panic() {
        let out = run(r#"
import * as resilience from "std/resilience"

let bh = resilience.bulkhead({limit: 1, queue: 0})

fn panicking_fn() { error("kaboom") }
fn ok_fn() { return 42 }

// First run panics — but must release the permit
let [v1, e1] = recover(() => bh.run(panicking_fn))
print(e1 != nil)   // true: panic propagated (wrapped by recover)

// If permit was released, this should succeed
let [v2, e2] = bh.run(ok_fn)
print(v2)          // 42
print(e2)          // nil
"#).await;
        assert_eq!(out, "true\n42\nnil\n");
    }

    /// Validation: limit=0 rejected (Tier-2); queue=0 valid (sheds immediately when full).
    #[tokio::test]
    async fn bulkhead_validation() {
        // limit=0 → Tier-2 panic
        let r1 = crate::run_source(r#"
import * as resilience from "std/resilience"
resilience.bulkhead({limit: 0, queue: 0})
"#).await;
        let msg1 = match r1 { Err(e) => format!("{e}"), Ok(s) => s };
        assert!(msg1.contains("limit"), "expected limit panic, got: {msg1:?}");

        // queue=0 valid — sheds immediately when limit reached
        let out = run(r#"
import * as resilience from "std/resilience"
import { channel, send, recv } from "std/sync"
import { spawn } from "std/task"

let bh = resilience.bulkhead({limit: 1, queue: 0})
let gate = channel()
let entered = channel()

async fn slow_fn() {
    await send(entered, "in")
    await recv(gate)
    return "done"
}

let t1 = spawn(async () => bh.run(slow_fn))
let _ = await recv(entered)   // t1 is in-flight

// queue=0: second run sheds immediately (synchronous call, no spawn needed)
let [v2, e2] = bh.run(slow_fn)
print(e2.code)   // bulkhead-full

await send(gate, "go")
await t1
print("done")
"#).await;
        assert_eq!(out, "bulkhead-full\ndone\n");
    }

    // deadline-while-parked test added in Task 4.4

    // ── Task 2.4: resilience.retry stateful budget policy (§3.4) ──────────────

    /// A retry policy with no budget behaves like task.retry (retries panics, succeeds).
    #[tokio::test]
    async fn retry_policy_basic_succeeds() {
        let out = run(r#"
import * as resilience from "std/resilience"
let p = resilience.retry({attempts: 5, baseMs: 1})
let c = [0]
async fn flaky() { c[0] = c[0] + 1; if (c[0] < 3) { assert(false, "x") }; return "ok" }
print(await p.call(flaky))
print(c[0])
"#)
        .await;
        assert_eq!(out, "ok\n3\n");
    }

    /// Budget exhaustion is COUNT-based: with budget=0.5 over an err-retry policy,
    /// retries stop once `__retriesSpent >= budget * __attemptsSeen`. Driving many
    /// always-failing calls, the budget caps retries → later calls exhaust immediately
    /// (one attempt only). NO clocks involved.
    #[tokio::test]
    async fn retry_policy_budget_caps_retries() {
        let out = run(r#"
import * as resilience from "std/resilience"
// budget 0.5: at most half as many retries as attempts seen.
let p = resilience.retry({attempts: 10, baseMs: 1, retryOn: "error", budget: 0.5})
let total = [0]
async fn errs() { total[0] = total[0] + 1; return [nil, {message: "bad"}] }
// Run several failing calls; each consumes from the shared budget.
let i = [0]
while (i[0] < 5) {
    let [v, e] = await p.call(errs)
    i[0] = i[0] + 1
}
let s = p.stats()
// Budget invariant: retriesSpent <= budget * attemptsSeen (count-based, no clocks).
print(s.retriesSpent <= 0.5 * s.attemptsSeen)
// The budget genuinely throttled retries (fewer attempts than the unbounded 5*10=50).
print(s.attemptsSeen < 50)
print(s.retriesSpent > 0)
"#)
        .await;
        assert_eq!(out, "true\ntrue\ntrue\n");
    }

    /// A tiny budget throttles HARD after the first retry. The budget invariant is
    /// `spent < budget * seen`; with budget=0.1 the first call (seen=1, spent=0)
    /// permits exactly one retry (0 < 0.1), then the second attempt (seen=2, spent=1)
    /// is blocked (1 < 0.2 is false) → exactly 2 attempts. Count-based, NO clocks.
    #[tokio::test]
    async fn retry_policy_tiny_budget_throttles_after_first_retry() {
        let out = run(r#"
import * as resilience from "std/resilience"
let p = resilience.retry({attempts: 5, baseMs: 1, retryOn: "error", budget: 0.1})
let c = [0]
async fn errs() { c[0] = c[0] + 1; return [nil, {message: "bad"}] }
let [v, e] = await p.call(errs)
print(c[0])   // 2 — one retry permitted, then budget blocks further attempts
"#)
        .await;
        assert_eq!(out, "2\n");
    }

    /// budget out of range (> 1) on resilience.retry → Tier-2.
    #[tokio::test]
    async fn retry_policy_budget_range_validated() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
resilience.retry({attempts: 3, budget: 2.0})
"#)
        .await;
        let msg = match res { Err(e) => format!("{e}"), Ok(s) => s };
        assert!(msg.contains("budget"), "expected budget range panic, got: {msg:?}");
    }

    /// reset() zeroes the budget counters.
    #[tokio::test]
    async fn retry_policy_reset_clears_counters() {
        let out = run(r#"
import * as resilience from "std/resilience"
let p = resilience.retry({attempts: 3, baseMs: 1, retryOn: "error", budget: 1.0})
async fn errs() { return [nil, {message: "bad"}] }
await p.call(errs)
let before = p.stats()
print(before.attemptsSeen > 0)
p.reset()
let after = p.stats()
print(after.attemptsSeen)
print(after.retriesSpent)
"#)
        .await;
        assert_eq!(out, "true\n0\n0\n");
    }

    // ── Task 2.5: resilience.fallback (§3.5) ────────────────────────────────

    /// ok value passes through as [v, nil]; fb is NOT called.
    #[tokio::test]
    async fn fallback_ok_value_passes_through() {
        let out = run(r#"
import * as resilience from "std/resilience"
let called = [false]
fn fb(e) { called[0] = true; return "fallback" }
let [v, err] = resilience.fallback(() => 42, fb)
print(v)
print(err)
print(called[0])
"#)
        .await;
        assert_eq!(out, "42\nnil\nfalse\n");
    }

    /// err pair → fb(err) called with the err object; fb result returned as pair.
    #[tokio::test]
    async fn fallback_err_pair_calls_fb() {
        let out = run(r#"
import * as resilience from "std/resilience"
fn primary() { return [nil, {message: "x"}] }
fn fb(e) { return e.message }
let [v, err] = resilience.fallback(primary, fb)
print(v)
print(err)
"#)
        .await;
        assert_eq!(out, "x\nnil\n");
    }

    /// panic → fb called with {message: <panic msg>}; panic is consumed (NOT re-raised).
    #[tokio::test]
    async fn fallback_panic_calls_fb_and_is_consumed() {
        let out = run(r#"
import * as resilience from "std/resilience"
import { contains } from "std/string"
fn primary() { assert(false, "boom") }
let got_msg = [""]
fn fb(e) { got_msg[0] = e.message; return "recovered" }
let [v, err] = resilience.fallback(primary, fb)
print(v)
print(err)
print(contains(got_msg[0], "boom"))
"#)
        .await;
        assert_eq!(out, "recovered\nnil\ntrue\n");
    }

    /// fb panic re-raises (NOT consumed).
    #[tokio::test]
    async fn fallback_fb_panic_reraises() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
fn primary() { return [nil, {message: "x"}] }
fn fb(e) { assert(false, "fb-boom") }
let [v, err] = resilience.fallback(primary, fb)
print(v)
"#)
        .await;
        let msg = match res {
            Err(e) => format!("{e}"),
            Ok(s) => s,
        };
        assert!(msg.contains("fb-boom"), "expected fb panic to re-raise, got: {msg:?}");
    }

    /// fb result normalized: a plain value returned by fb → [value, nil].
    #[tokio::test]
    async fn fallback_fb_result_normalized_to_pair() {
        let out = run(r#"
import * as resilience from "std/resilience"
fn primary() { return [nil, {message: "x"}] }
fn fb(e) { return 99 }
let [v, err] = resilience.fallback(primary, fb)
print(v)
print(err)
"#)
        .await;
        assert_eq!(out, "99\nnil\n");
    }

    /// async fn and async fb are driven correctly.
    #[tokio::test]
    async fn fallback_async_fn_and_fb_driven() {
        let out = run(r#"
import * as resilience from "std/resilience"
async fn primary() { return [nil, {message: "async-err"}] }
async fn fb(e) { return e.message }
let [v, err] = await resilience.fallback(primary, fb)
print(v)
print(err)
"#)
        .await;
        assert_eq!(out, "async-err\nnil\n");
    }
}
