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

// ── public exports ────────────────────────────────────────────────────────────

/// The export list (binding name → value) for `import * from "std/resilience"`.
///
/// Task 1.1 ships only `breaker`; subsequent tasks will extend this list.
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("breaker", super::bi("resilience.breaker"))]
}

// ── tagged-object helpers ─────────────────────────────────────────────────────

/// Known `__resil` kind tags — kept narrow so an unrelated user object that
/// happens to carry a `__resil` field is never hijacked by the call-site hook.
// Task 1.2 (call-site hook) uses these; pre-declared here.
#[allow(dead_code)]
pub(crate) const RESIL_KINDS: &[&str] =
    &["breaker", "limiter", "keyedLimiter", "bulkhead", "retry", "memoize"];

/// True iff `v` is a resilience policy object: a `Value::Object` whose `__resil`
/// field is a string in `RESIL_KINDS`.
// Task 1.2 uses this in the call-site hook wiring; dead until then.
#[allow(dead_code)]
pub(crate) fn is_resilience_value(v: &Value) -> bool {
    resil_kind(v).map(|k| RESIL_KINDS.contains(&&*k)).unwrap_or(false)
}

/// The method names that route through the call-site hook (Task 1.2).
// Task 1.2 uses this in the call-site hook wiring; dead until then.
#[allow(dead_code)]
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

    /// An unimplemented kind (not "breaker") raises a Tier-2 panic with the kind name.
    #[tokio::test]
    async fn hook_unimplemented_kind_error() {
        // We can't construct a non-breaker policy yet, but we can craft a tagged object
        // with a known kind and call a method on it to check the error message.
        let res = crate::run_source(r#"
let fake = {__resil: "limiter"}
fake.acquire()
"#)
        .await;
        // run_source returns Err(AsError) for a Tier-2 panic.
        let msg = match res {
            Err(e) => format!("{e}"),
            Ok(s) => s,
        };
        assert!(
            msg.contains("limiter policy has no method 'acquire'"),
            "expected limiter kind error, got: {msg:?}"
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
}
