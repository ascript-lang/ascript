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
}
