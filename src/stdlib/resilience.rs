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
pub(crate) struct ResilState {
    /// Active singleflight flights keyed by the user-supplied string key.
    /// Each value is the `SharedFuture` for the ONE in-progress execution;
    /// concurrent callers with the same key clone-and-await it instead of
    /// launching a second invocation. Entries are removed when the flight
    /// resolves (Task 3.2).
    pub(crate) flights: IndexMap<String, crate::task::SharedFuture>,
    /// Monotonic counter minting a UNIQUE `__sfPrefix` per `memoize()` cache
    /// (Task 3.3 §3.7), so two distinct caches never collide on the global
    /// singleflight `flights` table when memoizing the same user key.
    pub(crate) sf_prefix_next: u64,
    /// Minimal per-isolate metrics registry (§6.1) — the always-on source of
    /// truth for policy counters/gauges (telemetry is a mirror, log a
    /// breadcrumb). Filled by Phase 5's policy instrumentation.
    pub(crate) registry: ResilRegistry,
    /// Instrument-id cache for the SP12 telemetry soft-hook mirror: ONE
    /// registration per metric NAME across the isolate, keyed by metric name.
    /// `telemetry_register_instrument` is itself idempotent, but caching the id
    /// avoids a `telemetry.borrow()` scan on every bump. Behind the telemetry
    /// feature — the registry itself is always-on and feature-free.
    #[cfg(feature = "telemetry")]
    pub(crate) instrument_ids: std::collections::HashMap<String, u64>,
}

/// A metric's type — Prometheus needs this for the `# TYPE` line (Task 5.2) and
/// it selects the aggregation (counter = monotonic add, gauge = set).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ResilMetricKind {
    Counter,
    Gauge,
}

/// One label series of a metric: the (insertion-ordered) labels and its value.
struct ResilSeries {
    /// The label pairs in declaration order (so a deterministic Prometheus
    /// render can emit `{name="t",result="failure"}` in a stable order).
    labels: Vec<(String, String)>,
    value: f64,
}

/// One metric: its kind plus the per-label-key series, both insertion-ordered.
struct ResilMetric {
    kind: ResilMetricKind,
    /// `sorted-label-key → series`. The key is the `attr_key`-style canonical
    /// form (sorted `k=v` joined by `;`) so the same labels always map to ONE
    /// series; iteration order is first-seen (deterministic Task-5.2 render).
    series: IndexMap<String, ResilSeries>,
}

/// Per-isolate minimal metrics registry (§6.1) — counters and gauges only (NO
/// histograms in v1). Shape: `metric-name → (label-key → series)`, both
/// `IndexMap`s so a deterministic walk for Prometheus rendering (Task 5.2) is
/// trivial (metric-name insertion order × per-metric label-key insertion order).
#[derive(Default)]
pub(crate) struct ResilRegistry {
    metrics: IndexMap<String, ResilMetric>,
}

/// Canonical sorted-label key for the registry — mirrors `telemetry::model`'s
/// `attr_key` (sorted `k=v` joined by `;`) so a registry series and its
/// telemetry-mirror point share the same identity for string-valued labels.
fn label_key(labels: &[(&str, &str)]) -> String {
    let mut parts: Vec<String> = labels.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
    parts.sort();
    parts.join(";")
}

impl ResilRegistry {
    /// Add `by` to a counter `metric_name` for the given label set (monotonic).
    /// Registers the metric (as a Counter) on first sight; a later use of a
    /// known name keeps its kind.
    pub(crate) fn incr(&mut self, metric_name: &str, labels: &[(&str, &str)], by: f64) {
        let metric = self
            .metrics
            .entry(metric_name.to_string())
            .or_insert_with(|| ResilMetric {
                kind: ResilMetricKind::Counter,
                series: IndexMap::new(),
            });
        let key = label_key(labels);
        let series = metric.series.entry(key).or_insert_with(|| ResilSeries {
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            value: 0.0,
        });
        series.value += by;
    }

    /// Set a gauge `metric_name` for the given label set to `value`.
    /// Registers the metric (as a Gauge) on first sight.
    pub(crate) fn set_gauge(&mut self, metric_name: &str, labels: &[(&str, &str)], value: f64) {
        let metric = self
            .metrics
            .entry(metric_name.to_string())
            .or_insert_with(|| ResilMetric {
                kind: ResilMetricKind::Gauge,
                series: IndexMap::new(),
            });
        let key = label_key(labels);
        let series = metric.series.entry(key).or_insert_with(|| ResilSeries {
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            value: 0.0,
        });
        series.value = value;
    }

    /// Read a counter/gauge series value (test/Task-5.2 accessor). Returns
    /// `0.0` if the metric or label set has never been touched.
    #[allow(dead_code)]
    pub(crate) fn value(&self, metric_name: &str, labels: &[(&str, &str)]) -> f64 {
        let Some(metric) = self.metrics.get(metric_name) else {
            return 0.0;
        };
        metric
            .series
            .get(&label_key(labels))
            .map(|s| s.value)
            .unwrap_or(0.0)
    }

    /// Deterministic walk for Task-5.2 Prometheus rendering: yields
    /// `(metric_name, kind, &[(label_pairs)], value)` in metric-name insertion
    /// order × per-metric label-key insertion order.
    #[allow(dead_code)]
    pub(crate) fn walk(&self) -> impl Iterator<Item = (&str, ResilMetricKind, &[(String, String)], f64)> {
        self.metrics.iter().flat_map(|(name, metric)| {
            metric.series.values().map(move |s| {
                (name.as_str(), metric.kind, s.labels.as_slice(), s.value)
            })
        })
    }
}

/// Escape a Prometheus label VALUE per the text exposition format 0.0.4 rules:
/// `\` → `\\`, `"` → `\"`, newline → `\n`. (Label NAMES are never escaped — they
/// come from our closed vocabulary of `[a-z_]` keys.)
fn escape_label_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the registry as Prometheus text exposition format 0.0.4. Deterministic:
/// metric-name insertion order × per-metric label-key (already-sorted) order; one
/// `# TYPE <metric> <counter|gauge>` line per metric NAME (emitted before its first
/// series, never duplicated). Metric names already carry the `ascript_resilience_`
/// prefix (stored at bump time), so no prefix is added here.
pub(crate) fn render_prometheus(reg: &ResilRegistry) -> String {
    let mut out = String::new();
    let mut last_name: Option<&str> = None;
    for (name, kind, labels, value) in reg.walk() {
        // `# TYPE` once per metric name, before its first series.
        if last_name != Some(name) {
            let kind_str = match kind {
                ResilMetricKind::Counter => "counter",
                ResilMetricKind::Gauge => "gauge",
            };
            out.push_str("# TYPE ");
            out.push_str(name);
            out.push(' ');
            out.push_str(kind_str);
            out.push('\n');
            last_name = Some(name);
        }
        out.push_str(name);
        if !labels.is_empty() {
            out.push('{');
            for (i, (k, v)) in labels.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(k);
                out.push_str("=\"");
                out.push_str(&escape_label_value(v));
                out.push('"');
            }
            out.push('}');
        }
        out.push(' ');
        // Prometheus values are plain decimals; an integral f64 prints without a
        // trailing `.0` to match the conventional counter rendering (`5`, not `5.0`).
        if value.fract() == 0.0 && value.is_finite() {
            out.push_str(&format!("{}", value as i64));
        } else {
            out.push_str(&format!("{}", value));
        }
        out.push('\n');
    }
    out
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
        ("singleflight", super::bi("resilience.singleflight")),
        ("memoize", super::bi("resilience.memoize")),
        ("deadline", super::bi("resilience.deadline")),
        ("deadlineRemaining", super::bi("resilience.deadlineRemaining")),
        ("withTrace", super::bi("resilience.withTrace")),
        ("traceId", super::bi("resilience.traceId")),
        ("metricsHandler", super::bi("resilience.metricsHandler")),
        ("health", super::bi("resilience.health")),
        ("handler", super::bi("resilience.handler")),
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
            | "delete" | "clear" | "len"
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

// ── §6.3/§6.4 HTTP wrapper helpers (free fns) ────────────────────────────────

/// Append a JSON-escaped string literal (with surrounding quotes) to `out`.
/// A minimal, feature-INDEPENDENT escaper (the `resilience` feature does not pull
/// in `data`/`serde_json`) covering the RFC 8259 mandatory escapes + control chars.
fn json_push_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Serialize the §6.3 health body: `{"status":"ok|degraded","checks":{<name>:{ok:bool,
/// error?:string}, ...}}`. Hand-built (no `serde_json` dependency) so it works under
/// `--no-default-features --features resilience`.
fn health_json(all_ok: bool, results: &[(String, bool, Option<String>)]) -> String {
    let mut out = String::from("{\"status\":");
    json_push_str(&mut out, if all_ok { "ok" } else { "degraded" });
    out.push_str(",\"checks\":{");
    for (i, (name, ok, err)) in results.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_push_str(&mut out, name);
        out.push_str(":{\"ok\":");
        out.push_str(if *ok { "true" } else { "false" });
        if let Some(msg) = err {
            out.push_str(",\"error\":");
            json_push_str(&mut out, msg);
        }
        out.push('}');
    }
    out.push_str("}}");
    out
}

/// Extract a human-readable message from a check's error value (`{message}` object
/// or a bare string), for the health body `error` field.
fn health_err_message(err: &Value) -> String {
    match err.kind() {
        ValueKind::Object(o) => match o.get("message").as_ref().map(|v| v.kind()) {
            Some(ValueKind::Str(s)) => s.to_string(),
            _ => err.to_string(),
        },
        ValueKind::Str(s) => s.to_string(),
        _ => err.to_string(),
    }
}

/// Build a §6.4 status-only HTTP response object `{status, headers?}`. When
/// `retry_after` is `Some(secs)`, a `retry-after` header (whole seconds, rounded up,
/// ≥ 1) is attached — the standard HTTP `Retry-After` delta-seconds form.
fn handler_status_response(status: i64, retry_after: Option<f64>) -> Value {
    let mut fields: IndexMap<String, Value> = IndexMap::new();
    fields.insert("status".to_string(), Value::int(status));
    if let Some(secs) = retry_after {
        let whole = (secs.ceil() as i64).max(1);
        let headers = Value::object(
            [("retry-after".to_string(), Value::str(whole.to_string()))]
                .into_iter()
                .collect(),
        );
        fields.insert("headers".to_string(), headers);
    }
    Value::object(fields)
}

/// Read an optional `retryAfter` (seconds) off a policy-rejection error object.
fn retry_after_of(err: &Value) -> Option<f64> {
    match err.kind() {
        ValueKind::Object(o) => o.get("retryAfter").and_then(|v| v.as_f64()),
        _ => None,
    }
}

/// Compute a limiter's retry-after (seconds until ≥ 1 token refills) from its live
/// `__tokens`/`refillPerSec`. Returns `None` for a zero-refill bucket (never refills
/// → omit the header) or a non-limiter receiver. Used to populate the 429 header.
fn limiter_retry_after_secs(lim: &Value) -> Option<f64> {
    match lim.kind() {
        ValueKind::Object(o) => {
            let refill = o.get("refillPerSec").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if refill <= 0.0 {
                return None;
            }
            let tokens = o.get("__tokens").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let deficit = (1.0 - tokens).max(0.0);
            Some(deficit / refill)
        }
        _ => None,
    }
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

// ── deadline-exceeded err pair (RESIL §5.2/§5.4) ─────────────────────────────

/// The `[nil, {code:"deadline-exceeded", message}]` err pair returned when a
/// deadline is hit — by `deadline` (the body race / already-expired entry) and by
/// the budget-aware park points (`limiter.acquire`, `keyedLimiter.acquire`,
/// `bulkhead.run`). Delegates to the CORE `interp::deadline_exceeded_pair` so the
/// shape is byte-identical with the §5.4 I/O consult sites (http/pg/redis/sqlite).
fn deadline_exceeded_pair() -> Value {
    crate::interp::deadline_exceeded_pair()
}

impl Interp {
    /// §6.1: bump `deadline_exceeded_total` (no `name` label) and return the
    /// shared `[nil, {code:"deadline-exceeded"}]` pair. Used at EVERY site that
    /// returns a deadline-exceeded result (the `deadline` body race + already-
    /// expired entry, and the budget-aware park exits in limiter/keyedLimiter/
    /// bulkhead acquire). The returned pair is byte-identical to
    /// `deadline_exceeded_pair()` — only the registry side effect is added.
    fn resil_deadline_exceeded_pair(&self) -> Value {
        self.resil_metric_incr("ascript_resilience_deadline_exceeded_total", &[], 1.0);
        deadline_exceeded_pair()
    }
}

// ── §6.1 metric bump helpers ─────────────────────────────────────────────────
//
// EVERY policy outcome routes a counter/gauge bump through ONE of these two
// `&self` helpers, so the always-on internal registry (the source of truth) and
// the SP12 telemetry soft-hook mirror stay in lock-step BY CONSTRUCTION:
//   - `resil_metric_incr` — a monotonic counter add
//   - `resil_metric_gauge` — a gauge set
// The telemetry mirror is `#[cfg(feature="telemetry")]` and no-ops until
// `telemetry.init` (the hook handles that); the registry works regardless. The
// `self.resilience` borrow is a SHORT synchronous borrow (never across `.await`).

impl Interp {
    /// Add `by` to the registry counter `name` for `labels`, then mirror the add
    /// through the SP12 telemetry soft hook (lazy one-per-name instrument).
    pub(crate) fn resil_metric_incr(&self, name: &str, labels: &[(&str, &str)], by: f64) {
        // Always-on registry (short sync borrow).
        self.resilience.borrow_mut().registry.incr(name, labels, by);
        #[cfg(feature = "telemetry")]
        self.resil_telemetry_mirror(
            name,
            crate::stdlib::telemetry::model::MetricKind::Counter,
            "add",
            by,
            labels,
        );
    }

    /// Set the registry gauge `name` for `labels` to `value`, then mirror.
    pub(crate) fn resil_metric_gauge(&self, name: &str, labels: &[(&str, &str)], value: f64) {
        self.resilience.borrow_mut().registry.set_gauge(name, labels, value);
        #[cfg(feature = "telemetry")]
        self.resil_telemetry_mirror(
            name,
            crate::stdlib::telemetry::model::MetricKind::Gauge,
            "set",
            value,
            labels,
        );
    }

    /// Mirror one metric sample through the SP12 telemetry soft hook. ONE
    /// instrument registration per metric NAME across the isolate (id cached in
    /// `ResilState.instrument_ids`); `telemetry_record_metric` no-ops until
    /// `telemetry.init`. Behind the telemetry feature; never holds the
    /// `resilience` borrow across the `telemetry` borrow.
    #[cfg(feature = "telemetry")]
    fn resil_telemetry_mirror(
        &self,
        name: &str,
        kind: crate::stdlib::telemetry::model::MetricKind,
        method: &str,
        amount: f64,
        labels: &[(&str, &str)],
    ) {
        // Resolve (and cache) the instrument id for this metric name. The cache
        // borrow is dropped before the registration / record calls.
        let cached = self.resilience.borrow().instrument_ids.get(name).copied();
        let id = match cached {
            Some(id) => id,
            None => {
                // `u64::MAX` means telemetry is uninitialized — register lazily but
                // do NOT cache the sentinel, so a later `telemetry.init` re-registers
                // (and caches the real id) on the next bump.
                let id = self.telemetry_register_instrument(name, kind, None, None);
                if id != u64::MAX {
                    self.resilience.borrow_mut().instrument_ids.insert(name.to_string(), id);
                }
                id
            }
        };
        if id == u64::MAX {
            // Telemetry uninitialized — nothing to record (the registry already
            // captured the sample).
            return;
        }
        let attrs: Vec<(String, Value)> = labels
            .iter()
            .map(|(k, v)| (k.to_string(), Value::str(*v)))
            .collect();
        self.telemetry_record_metric(id, method, amount, attrs);
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
            "memoize" => self.call_memoize_method(&recv, name, args, span).await,
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

        // The policy `name` label, captured once for all §6.1 breaker bumps.
        let bname = breaker_name(recv);

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
                    // §6.1: cooldown-driven open→halfOpen transition.
                    self.breaker_note_transition(&bname, "open", "halfOpen");
                    is_probe = true;
                } else {
                    // Still open → reject
                    if let ValueKind::Object(o) = recv.kind() {
                        let rej = o.get("__rejected").and_then(|v| v.as_int()).unwrap_or(0);
                        o.insert("__rejected", Value::int(rej + 1));
                    }
                    // §6.1: a rejected call (breaker open, cooldown not elapsed).
                    self.resil_metric_incr(
                        "ascript_resilience_breaker_calls_total",
                        &[("name", &bname), ("result", "rejected")],
                        1.0,
                    );
                    let mut err: IndexMap<String, Value> = IndexMap::new();
                    err.insert("message".to_string(), Value::str(format!("circuit breaker '{}' is open", bname)));
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
                    // §6.1: a rejected call (halfOpen probe budget exhausted).
                    self.resil_metric_incr(
                        "ascript_resilience_breaker_calls_total",
                        &[("name", &bname), ("result", "rejected")],
                        1.0,
                    );
                    let mut err: IndexMap<String, Value> = IndexMap::new();
                    err.insert("message".to_string(), Value::str(format!("circuit breaker '{}' is open", bname)));
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
                let pre = breaker_state_str(recv);
                breaker_record_outcome(recv, true /* is_failure */, is_probe,
                                       false /* transition already handled via halfOpen fail below */,
                                       window, min_calls, failure_rate, now, true /* panic path */);
                // §6.1: a panic is a failure outcome (the recorder skips state
                // transitions on the panic path, so post == pre — no edge bump).
                self.breaker_after_outcome(recv, &bname, true, &pre);
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
                let pre = breaker_state_str(recv);
                breaker_record_outcome(recv, is_failure, is_probe, true,
                                       window, min_calls, failure_rate, now, false);
                // §6.1: count the success/failure outcome + reflect any state edge
                // the recorder made (closed→open, halfOpen→open, halfOpen→closed).
                self.breaker_after_outcome(recv, &bname, is_failure, &pre);
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
                let lname = policy_name(recv);
                if new_tokens >= n {
                    // Consume n tokens (atomic: check+consume under one sync borrow)
                    if let ValueKind::Object(o) = recv.kind() {
                        o.insert("__tokens", Value::float(new_tokens - n));
                    }
                    // §6.1: a granted acquisition.
                    self.resil_metric_incr(
                        "ascript_resilience_limiter_acquired_total",
                        &[("name", &lname)],
                        1.0,
                    );
                    Ok(Value::bool_(true))
                } else {
                    // §6.1: a rejected (would-block) tryAcquire.
                    self.resil_metric_incr(
                        "ascript_resilience_limiter_rejected_total",
                        &[("name", &lname)],
                        1.0,
                    );
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
                        // §6.1: a granted acquisition (possibly after parking).
                        self.resil_metric_incr(
                            "ascript_resilience_limiter_acquired_total",
                            &[("name", &policy_name(recv))],
                            1.0,
                        );
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
                    // §5.4 budget-aware park: if a deadline is set and the refill
                    // sleep would carry us past the budget (or it is already
                    // exhausted), return `deadline-exceeded` at ~the budget rather
                    // than waiting for the full refill. NO deadline → the `None`
                    // fast path is byte-identical to before (plain sleep, no race).
                    let duration = std::time::Duration::from_millis(sleep_ms as u64);
                    match self.deadline_remaining_ms() {
                        Some(r) if r <= 0.0 => return Ok(self.resil_deadline_exceeded_pair()),
                        Some(r) => {
                            tokio::select! {
                                _ = tokio::time::sleep(duration) => {}
                                _ = tokio::time::sleep(
                                    std::time::Duration::from_millis(r as u64),
                                ) => return Ok(self.resil_deadline_exceeded_pair()),
                            }
                        }
                        None => tokio::time::sleep(duration).await,
                    }
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

// ── §6.1 breaker metric helpers ──────────────────────────────────────────────

/// Read a policy's `name` field (default `"default"`) as an owned `String` —
/// the `{name}` label for every §6.1 policy metric (breaker/limiter/bulkhead/
/// keyedLimiter/memoize/retry all carry a `name`).
fn policy_name(recv: &Value) -> String {
    breaker_name(recv)
}

/// Read the breaker policy's `name` field (default `"default"`) as an owned
/// `String` — the `{name}` label for every §6.1 breaker metric.
fn breaker_name(recv: &Value) -> String {
    match recv.kind() {
        ValueKind::Object(o) => o
            .get("name")
            .and_then(|v| match v.kind() {
                ValueKind::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "default".to_string()),
        _ => "default".to_string(),
    }
}

/// Read the breaker's current `__state` string (default `"closed"`).
fn breaker_state_str(recv: &Value) -> String {
    match recv.kind() {
        ValueKind::Object(o) => o
            .get("__state")
            .and_then(|v| match v.kind() {
                ValueKind::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "closed".to_string()),
        _ => "closed".to_string(),
    }
}

/// Map a breaker state string to the §6.1 `breaker_state` gauge value
/// (0 = closed, 1 = open, 2 = halfOpen).
fn breaker_state_gauge(state: &str) -> f64 {
    match state {
        "open" => 1.0,
        "halfOpen" => 2.0,
        _ => 0.0,
    }
}

impl Interp {
    /// Record a breaker STATE TRANSITION (`from`→`to`): the §6.1
    /// `breaker_transitions_total{name,to}` counter, the `breaker_state{name}`
    /// gauge, and a `log.debug` transition breadcrumb (behind the `log` feature —
    /// a transition event, never per-call spam).
    fn breaker_note_transition(&self, name: &str, from: &str, to: &str) {
        self.resil_metric_incr(
            "ascript_resilience_breaker_transitions_total",
            &[("name", name), ("to", to)],
            1.0,
        );
        self.resil_metric_gauge(
            "ascript_resilience_breaker_state",
            &[("name", name)],
            breaker_state_gauge(to),
        );
        #[cfg(feature = "log")]
        self.breaker_log_transition(name, from, to);
        #[cfg(not(feature = "log"))]
        let _ = from;
    }

    /// `log.debug` breadcrumb for a breaker transition (best-effort, behind the
    /// `log` feature). Routed through the SYNCHRONOUS `log_debug_breadcrumb`
    /// emitter so it can be called from the sync metric helpers (the regular
    /// `call_log` is async only because of its thunk-first-arg path, which this
    /// breadcrumb never uses).
    #[cfg(feature = "log")]
    fn breaker_log_transition(&self, name: &str, from: &str, to: &str) {
        self.log_debug_breadcrumb(
            "breaker state transition",
            &[("breaker", name), ("from", from), ("to", to)],
        );
    }

    /// After `breaker_record_outcome`, emit the §6.1 outcome metrics: the
    /// `breaker_calls_total{name,result=success|failure}` counter, the
    /// `breaker_state{name}` gauge, and a transition counter/breadcrumb when the
    /// recorder moved the breaker between states (`pre_state` != post-state).
    fn breaker_after_outcome(&self, recv: &Value, name: &str, is_failure: bool, pre_state: &str) {
        let result = if is_failure { "failure" } else { "success" };
        self.resil_metric_incr(
            "ascript_resilience_breaker_calls_total",
            &[("name", name), ("result", result)],
            1.0,
        );
        let post = breaker_state_str(recv);
        if post != pre_state {
            // A real edge — bumps the transition counter + sets the gauge.
            self.breaker_note_transition(name, pre_state, &post);
        } else {
            // No edge: keep the gauge current (idempotent set; cheap).
            self.resil_metric_gauge(
                "ascript_resilience_breaker_state",
                &[("name", name)],
                breaker_state_gauge(&post),
            );
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
                let kname = policy_name(recv);
                // §6.1: evictions are counted as the lru store's eviction-count
                // delta across this operation (the bucket store evicts the LRU key
                // when at `maxKeys` inside `keyed_set_bucket`).
                let ev_before = self.lru_eviction_count(store_id);

                // Get or create bucket from lru store
                let bucket = keyed_get_bucket(self, store_id, &key_str, capacity, now, span)?;

                // Refill + try consume
                let new_tokens = limiter_refill_bucket(&bucket, capacity, refill_per_sec, now);
                let granted = new_tokens >= n;
                if granted {
                    bucket_set_tokens(&bucket, new_tokens - n);
                    // Touch: set the updated bucket back into the lru (updates recency + tokens)
                    keyed_set_bucket(self, store_id, &key_str, &bucket, span)?;
                } else {
                    // Set back to persist the refill update even on rejection
                    keyed_set_bucket(self, store_id, &key_str, &bucket, span)?;
                }
                self.keyed_emit_metrics(&kname, store_id, ev_before, Some(granted));
                Ok(Value::bool_(granted))
            }

            "acquire" => {
                // args[0]=recv, args[1]=key, args[2]=n (optional)
                let key = args.get(1).cloned().unwrap_or(Value::nil());
                let key_str = keyed_validate_key(&key, span)?;
                let n = args.get(2).and_then(|v| v.as_f64()).unwrap_or(1.0);

                loop {
                    let now = self.clock_monotonic_ms(real_monotonic_ms());
                    let (capacity, refill_per_sec, store_id) = keyed_read_config(recv);
                    let ev_before = self.lru_eviction_count(store_id);

                    // Short synchronous section: refill + check
                    let bucket = keyed_get_bucket(self, store_id, &key_str, capacity, now, span)?;
                    let new_tokens = limiter_refill_bucket(&bucket, capacity, refill_per_sec, now);

                    if new_tokens >= n {
                        bucket_set_tokens(&bucket, new_tokens - n);
                        keyed_set_bucket(self, store_id, &key_str, &bucket, span)?;
                        // §6.1: a granted acquisition (+ any eviction delta).
                        self.keyed_emit_metrics(&policy_name(recv), store_id, ev_before, Some(true));
                        return Ok(Value::nil());
                    }

                    // Compute deficit sleep, persist refill update, drop all borrows before await
                    bucket_set_tokens(&bucket, new_tokens);
                    keyed_set_bucket(self, store_id, &key_str, &bucket, span)?;
                    // §6.1: count evictions caused by this park-iteration's bucket
                    // touch (acquire never "rejects" — it parks — so pass `None`:
                    // evictions only, no acquired/rejected bump on the wait path).
                    self.keyed_emit_metrics(&policy_name(recv), store_id, ev_before, None);

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
                    // §5.4 budget-aware park (same shape as `limiter.acquire`): a
                    // set deadline caps the refill wait at the remaining budget.
                    // NO deadline → byte-identical plain sleep.
                    let duration = std::time::Duration::from_millis(sleep_ms as u64);
                    match self.deadline_remaining_ms() {
                        Some(r) if r <= 0.0 => return Ok(self.resil_deadline_exceeded_pair()),
                        Some(r) => {
                            tokio::select! {
                                _ = tokio::time::sleep(duration) => {}
                                _ = tokio::time::sleep(
                                    std::time::Duration::from_millis(r as u64),
                                ) => return Ok(self.resil_deadline_exceeded_pair()),
                            }
                        }
                        None => tokio::time::sleep(duration).await,
                    }
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

impl Interp {
    /// Emit the §6.1 keyedLimiter metrics for one acquire/tryAcquire op:
    /// `limiter_evictions_total{name}` for the lru eviction-count delta since
    /// `ev_before`, plus (when `granted` is `Some`) `limiter_acquired_total{name}`
    /// on grant or `limiter_rejected_total{name}` on a `tryAcquire` rejection.
    /// `granted = None` is the `acquire` park path — evictions only.
    fn keyed_emit_metrics(
        &self,
        name: &str,
        store_id: u64,
        ev_before: u64,
        granted: Option<bool>,
    ) {
        let ev_after = self.lru_eviction_count(store_id);
        if ev_after > ev_before {
            self.resil_metric_incr(
                "ascript_resilience_limiter_evictions_total",
                &[("name", name)],
                (ev_after - ev_before) as f64,
            );
        }
        match granted {
            Some(true) => self.resil_metric_incr(
                "ascript_resilience_limiter_acquired_total",
                &[("name", name)],
                1.0,
            ),
            Some(false) => self.resil_metric_incr(
                "ascript_resilience_limiter_rejected_total",
                &[("name", name)],
                1.0,
            ),
            None => {}
        }
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

        // The policy `name` + `limit` for the §6.1 bulkhead gauge/shed metrics.
        let bhname = policy_name(recv);
        let limit = match recv.kind() {
            ValueKind::Object(o) => o.get("limit").and_then(|v| v.as_int()).unwrap_or(0),
            _ => 0,
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
                // §6.1: an immediately-shed call (queue full).
                self.resil_metric_incr(
                    "ascript_resilience_bulkhead_shed_total",
                    &[("name", &bhname)],
                    1.0,
                );
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

        // §5.4 budget-aware park: race the permit acquire against the remaining
        // deadline budget. The acquire future is `&mut`-driven through the select
        // so the WINNER on the deadline branch only ABANDONS the wait (the future
        // is dropped → no permit leaked, `sync_acquire` had not yet incremented an
        // in-flight count). NO deadline set → the `None` fast path is byte-identical
        // to the previous plain `.await` (no select, no behavior change).
        //
        // We reach Step 6's unconditional release ONLY when we hold a permit (this
        // block returns early on a deadline-expiry park exit, before any permit).
        let acquire_result: Result<Value, Control> = {
            let acquire = self.sync_acquire(std::slice::from_ref(&sem_value), span);
            match self.deadline_remaining_ms() {
                Some(r) if r <= 0.0 => {
                    drop(acquire);
                    // Already past the budget — never park. Cleanup `__waiting`.
                    if needs_wait {
                        if let ValueKind::Object(o) = recv.kind() {
                            let w =
                                o.get("__waiting").and_then(|v| v.as_int()).unwrap_or(1);
                            o.insert("__waiting", Value::int((w - 1).max(0)));
                        }
                    }
                    return Ok(self.resil_deadline_exceeded_pair());
                }
                Some(r) => {
                    tokio::select! {
                        res = acquire => res,
                        _ = tokio::time::sleep(
                            std::time::Duration::from_millis(r as u64),
                        ) => {
                            // Deadline expired while parked: decrement `__waiting`
                            // (exactly once — we are on a `needs_wait` park exit).
                            if needs_wait {
                                if let ValueKind::Object(o) = recv.kind() {
                                    let w = o
                                        .get("__waiting")
                                        .and_then(|v| v.as_int())
                                        .unwrap_or(1);
                                    o.insert("__waiting", Value::int((w - 1).max(0)));
                                }
                            }
                            return Ok(self.resil_deadline_exceeded_pair());
                        }
                    }
                }
                None => acquire.await,
            }
        };

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

        // §6.1: reflect the now-incremented in-flight count (limit − available),
        // computed from the live semaphore (short borrow, no await held).
        {
            let avail = *sem.available.borrow();
            self.resil_metric_gauge(
                "ascript_resilience_bulkhead_in_flight",
                &[("name", &bhname)],
                (limit - avail as i64).max(0) as f64,
            );
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

        // §6.1: reflect the now-decremented in-flight count after release.
        {
            let avail = *sem.available.borrow();
            self.resil_metric_gauge(
                "ascript_resilience_bulkhead_in_flight",
                &[("name", &bhname)],
                (limit - avail as i64).max(0) as f64,
            );
        }

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

    // Policy `name` label (default "default") for the §6.1 retry metrics.
    let name = match opts.kind() {
        ValueKind::Object(o) => match o.get("name") {
            Some(v) => match v.kind() {
                ValueKind::Str(s) => s.to_string(),
                ValueKind::Nil => "default".to_string(),
                _ => return Err(AsError::at("resilience.retry: 'name' must be a string", span).into()),
            },
            None => "default".to_string(),
        },
        _ => "default".to_string(),
    };

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
    m.insert("name".to_string(), Value::str(name));
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

// ── Interp::call_singleflight (§3.6) ─────────────────────────────────────────

impl Interp {
    /// `resilience.singleflight(key, fn)` — collapse concurrent same-key calls
    /// to ONE execution of `fn`; all awaiters observe the same value (or the
    /// same panic, via `SharedFuture`'s panic fan-out). NO result caching — the
    /// table entry is removed on resolve, so the key is re-flyable the moment
    /// the result is delivered (caching is memoize's job, §3.7).
    ///
    /// - `key` must be a string (Tier-2 panic otherwise).
    /// - key present → clone the live flight handle out (under a SHORT borrow,
    ///   dropped before returning) and return it; the caller awaits the shared
    ///   handle (N awaiters → one execution).
    /// - key absent → create a taskless `SharedFuture`, insert it, then
    ///   `spawn_local` a driver that invokes `fn` (driving a returned future),
    ///   resolves the cell, and removes the table entry in ALL paths
    ///   (success / panic / propagate).
    ///
    /// Lifecycle (the `task.rs` split): the driver holds only the `ResultCell`,
    /// the table holds the handle — so callers dropping their futures mid-flight
    /// do NOT cancel the flight (it completes for whoever joins next; herd-safe).
    /// The `flights` borrow is NEVER held across an `.await`.
    fn call_singleflight(
        &self,
        key: Value,
        user_fn: Value,
        span: Span,
    ) -> Result<Value, Control> {
        // ── Validate the key (Tier-2 on non-string) ───────────────────────────
        let key_str = match key.kind() {
            ValueKind::Str(s) => s.to_string(),
            _ => {
                return Err(AsError::at(
                    format!(
                        "singleflight: key must be a string, got {}",
                        crate::interp::type_name(&key)
                    ),
                    span,
                )
                .into())
            }
        };

        // ── Join an existing flight (SHORT borrow, dropped before return) ──────
        {
            let flights = self.resilience.borrow();
            let joined = flights.flights.get(&key_str).cloned();
            // Drop the registry borrow BEFORE the §6.1 join bump (which takes a
            // `borrow_mut` of the SAME `self.resilience` cell).
            drop(flights);
            if let Some(handle) = joined {
                // §6.1: a singleflight join (no `name` label — flights are global).
                self.resil_metric_incr(
                    "ascript_resilience_singleflight_joins_total",
                    &[],
                    1.0,
                );
                return Ok(Value::future(handle));
            }
        }

        // ── Start a new flight ────────────────────────────────────────────────
        // Taskless `SharedFuture`: the table owns this handle, so the flight is
        // not cancelled when callers drop their futures (the driver holds only
        // the cell — see the lifecycle note).
        let fut = crate::task::SharedFuture::new();
        let cell = fut.cell();

        // Insert the handle into the table under a SHORT borrow (dropped before
        // we spawn / return).
        {
            let mut flights = self.resilience.borrow_mut();
            flights.flights.insert(key_str.clone(), fut.clone());
        }

        // Drive `fn` on its own task; resolve the cell + remove the table entry
        // on ALL exit paths. Held alive: an owned `Rc<Interp>` so the driver can
        // touch `self.resilience` after the await.
        let interp = self.rc();
        let driver = crate::exec::spawn_local(async move {
            // Invoke `fn`, driving a returned future to completion.
            let raw = interp.call_value(user_fn, vec![], span).await;
            let outcome: Result<Value, Control> = match raw {
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

            // Remove the table entry FIRST (so the key is re-flyable immediately;
            // no caching), then resolve so all awaiters wake with the result.
            // Short borrow, no `.await` inside.
            {
                let mut flights = interp.resilience.borrow_mut();
                flights.flights.shift_remove(&key_str);
            }
            cell.resolve(outcome);
        });
        // Park the driver's abort handle on the table handle so the short-lived
        // driver is reaped at isolate teardown. The table holds a handle clone
        // until resolve, so caller-drops never trip this abort mid-flight.
        fut.set_abort(driver.abort_handle());

        Ok(Value::future(fut))
    }
}

// ── memoize constructor + method dispatch (§3.7) ──────────────────────────────

impl Interp {
    /// Build a stampede-protected `memoize` cache policy object (§3.7).
    ///
    /// Fields:
    /// - `max`: positive int (≥ 1), the lru entry cap; default 1024.
    /// - `ttlMs`: optional positive number; absent/nil means entries never expire.
    ///
    /// State:
    /// - `__store`: a `std/lru` Native handle (recency + eviction shipped). Entries
    ///   are `{value, atMs}` Objects.
    /// - `__sfPrefix`: a per-isolate-unique string minted from `ResilState.sf_prefix_next`,
    ///   so two caches never collide on the GLOBAL singleflight `flights` table.
    /// - `__hits` / `__misses`: stats counters (mutated on the recv Object, like breaker).
    fn make_memoize(&self, opts: Value, span: Span) -> Result<Value, Control> {
        // ── name (optional string label, default "default") ───────────────────
        let mname = match opts.kind() {
            ValueKind::Object(o) => match o.get("name") {
                Some(v) => match v.kind() {
                    ValueKind::Str(s) => s.to_string(),
                    ValueKind::Nil => "default".to_string(),
                    _ => return Err(AsError::at("memoize: 'name' must be a string", span).into()),
                },
                None => "default".to_string(),
            },
            _ => "default".to_string(),
        };

        // ── max (positive int, default 1024) ──────────────────────────────────
        let max = memoize_opt_pos_int(&opts, "max", 1024, span)?;

        // ── ttlMs (optional positive number; nil/absent = no TTL) ──────────────
        let ttl_ms = memoize_opt_ttl_ms(&opts, span)?;

        // Real lru handle for the entry store.
        let store = self.new_lru_handle(max as usize);

        // Mint a unique singleflight prefix for this cache.
        let sf_prefix = {
            let mut st = self.resilience.borrow_mut();
            let n = st.sf_prefix_next;
            st.sf_prefix_next = st.sf_prefix_next.wrapping_add(1);
            format!("__memo{}:", n)
        };

        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("__resil".to_string(), Value::str("memoize"));
        m.insert("name".to_string(), Value::str(mname));
        m.insert("max".to_string(), Value::int(max as i64));
        m.insert(
            "ttlMs".to_string(),
            match ttl_ms {
                Some(t) => Value::float(t),
                None => Value::nil(),
            },
        );
        m.insert("__store".to_string(), store);
        m.insert("__sfPrefix".to_string(), Value::str(sf_prefix));
        m.insert("__hits".to_string(), Value::int(0));
        m.insert("__misses".to_string(), Value::int(0));
        // non-sendable marker
        m.insert("__local".to_string(), local_marker());

        Ok(Value::object(m))
    }

    /// Dispatch a method on a `memoize` cache policy object (§3.7).
    ///
    /// Supported methods: `get(key, fn)`, `delete(key)`, `clear()`, `len()`, `stats()`.
    /// NO `RefCell`/resources borrow is held across `.await`.
    async fn call_memoize_method(
        &self,
        recv: &Value,
        name: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        use crate::interp::{make_pair, result_pair_err, ResourceState};
        use crate::stdlib::time::real_monotonic_ms;
        use crate::value::MapKey;

        // Read config off the recv Object (sync, no borrow held across await).
        let (store_id, sf_prefix, ttl_ms) = memoize_read_config(recv);

        match name {
            "get" => {
                // args[0]=recv, args[1]=key, args[2]=fn
                let key = args.get(1).cloned().unwrap_or(Value::nil());
                let key_str = match key.kind() {
                    ValueKind::Str(s) => s.to_string(),
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "memoize: key must be a string, got {}",
                                crate::interp::type_name(&key)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                let user_fn = args.get(2).cloned().unwrap_or(Value::nil());

                let now = self.clock_monotonic_ms(real_monotonic_ms());

                // ── Look up the lru entry (touch recency on hit via lru `get`) ──
                let map_key = MapKey::Str(key_str.as_str().into());
                let entry = self.with_resource_mut(store_id, |r| match r {
                    Some(ResourceState::Lru(s)) => {
                        if s.map.contains_key(&map_key) {
                            s.touch(&map_key);
                            s.map.get(&map_key).cloned()
                        } else {
                            None
                        }
                    }
                    _ => None,
                });

                // ── HIT test: entry present AND (no TTL OR within TTL) ──────────
                if let Some(ref e) = entry {
                    let (value, at_ms) = match e.kind() {
                        ValueKind::Object(o) => (
                            o.get("value").unwrap_or(Value::nil()),
                            o.get("atMs").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        ),
                        _ => (Value::nil(), 0.0),
                    };
                    let fresh = match ttl_ms {
                        None => true,
                        Some(ttl) => (now - at_ms) < ttl,
                    };
                    if fresh {
                        // Hit: bump __hits, return the ok-pair [value, nil].
                        memoize_bump(recv, "__hits");
                        // §6.1: a cross-cache aggregate hit (separate from the
                        // per-cache `__hits`/`stats()` counter).
                        self.resil_metric_incr(
                            "ascript_resilience_memoize_hits_total",
                            &[("name", &policy_name(recv))],
                            1.0,
                        );
                        return Ok(make_pair(value, Value::nil()));
                    }
                    // Expired → fall through to a miss (lazy TTL).
                }

                // ── MISS: singleflight on a cache-scoped key, await it ─────────
                memoize_bump(recv, "__misses");
                // §6.1: a cross-cache aggregate miss.
                self.resil_metric_incr(
                    "ascript_resilience_memoize_misses_total",
                    &[("name", &policy_name(recv))],
                    1.0,
                );
                let flight_key = Value::str(format!("{}{}", sf_prefix, key_str));
                let fut = self.call_singleflight(flight_key, user_fn, span)?;
                // Await the flight (all concurrent awaiters observe the same result).
                let result = match fut.kind() {
                    ValueKind::Future(_) => {
                        let crate::value::OwnedKind::Future(f) = fut.into_kind() else {
                            unreachable!()
                        };
                        f.get().await?
                    }
                    _ => fut,
                };

                // Classify the flight result. SUCCESS = a plain value OR an
                // ok-pair `[v, nil]`; FAILURE = an err-pair `[nil, e]`. A panic
                // propagated via `?` above (never reaches here, never stored).
                if result_pair_err(&result).is_some() {
                    // err-pair: NOT cached (negative caching parked, §3.7).
                    // Pass the err-pair through unchanged.
                    return Ok(result);
                }

                // Success. Normalize the cached/returned value to the underlying
                // value: an ok-pair `[v, nil]` stores `v`; a plain value stores
                // itself. `get` always returns the `[value, nil]` ok-pair shape.
                let value = match result_pair_value(&result) {
                    Some(v) => v,
                    None => result.clone(),
                };
                let mut e: IndexMap<String, Value> = IndexMap::new();
                e.insert("value".to_string(), value.clone());
                e.insert("atMs".to_string(), Value::float(now));
                let entry_obj = Value::object(e);
                // lru set path: eviction + recency via the shipped machinery.
                let map_key = MapKey::Str(key_str.as_str().into());
                self.with_resource_mut(store_id, |r| {
                    if let Some(ResourceState::Lru(s)) = r {
                        if s.map.contains_key(&map_key) {
                            s.map.insert(map_key.clone(), entry_obj);
                            s.touch(&map_key);
                        } else {
                            while s.map.len() >= s.capacity && !s.map.is_empty() {
                                s.map.shift_remove_index(0);
                                s.eviction_count += 1;
                            }
                            s.map.insert(map_key, entry_obj);
                        }
                    }
                });

                Ok(make_pair(value, Value::nil()))
            }

            "delete" => {
                let key = args.get(1).cloned().unwrap_or(Value::nil());
                let key_str = match key.kind() {
                    ValueKind::Str(s) => s.to_string(),
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "memoize: key must be a string, got {}",
                                crate::interp::type_name(&key)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                let map_key = MapKey::Str(key_str.as_str().into());
                self.with_resource_mut(store_id, |r| {
                    if let Some(ResourceState::Lru(s)) = r {
                        s.map.shift_remove(&map_key);
                    }
                });
                Ok(Value::nil())
            }

            "clear" => {
                self.with_resource_mut(store_id, |r| {
                    if let Some(ResourceState::Lru(s)) = r {
                        s.map.clear();
                    }
                });
                Ok(Value::nil())
            }

            "len" => Ok(Value::int(self.lru_len(store_id) as i64)),

            "stats" => {
                let (hits, misses) = match recv.kind() {
                    ValueKind::Object(o) => (
                        o.get("__hits").unwrap_or(Value::int(0)),
                        o.get("__misses").unwrap_or(Value::int(0)),
                    ),
                    _ => (Value::int(0), Value::int(0)),
                };
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("hits".to_string(), hits);
                m.insert("misses".to_string(), misses);
                Ok(Value::object(m))
            }

            other => Err(AsError::at(
                format!("memoize policy has no method '{}'", other),
                span,
            )
            .into()),
        }
    }
}

// ── memoize helpers ───────────────────────────────────────────────────────────

/// Read a positive-integer option field; Tier-2 panic on a non-number or a
/// non-positive value. Mirrors the breaker `opt_pos_int`+`check_pos_int` pair
/// but with `memoize:`-prefixed messages.
fn memoize_opt_pos_int(opts: &Value, key: &str, default: u64, span: Span) -> Result<u64, Control> {
    let v = match opts.kind() {
        ValueKind::Object(o) => o.get(key),
        ValueKind::Nil => return Ok(default),
        _ => {
            return Err(AsError::at(
                "memoize: expected an options object, got non-object",
                span,
            )
            .into())
        }
    };
    match v {
        None => Ok(default),
        Some(v) => match v.kind() {
            ValueKind::Nil => Ok(default),
            _ => match v.as_f64() {
                Some(n) if n >= 1.0 && n.is_finite() => Ok(n as u64),
                Some(_) => Err(AsError::at(
                    format!("memoize: '{}' must be a positive integer (>= 1)", key),
                    span,
                )
                .into()),
                None => Err(AsError::at(
                    format!(
                        "memoize: '{}' must be a number, got {}",
                        key,
                        crate::interp::type_name(&v)
                    ),
                    span,
                )
                .into()),
            },
        },
    }
}

/// Read the optional `ttlMs` field: absent/nil → `None` (no TTL); a positive
/// finite number → `Some(n)`; anything else → Tier-2 panic.
fn memoize_opt_ttl_ms(opts: &Value, span: Span) -> Result<Option<f64>, Control> {
    let v = match opts.kind() {
        ValueKind::Object(o) => o.get("ttlMs"),
        _ => return Ok(None),
    };
    match v {
        None => Ok(None),
        Some(v) => match v.kind() {
            ValueKind::Nil => Ok(None),
            _ => match v.as_f64() {
                Some(n) if n > 0.0 && n.is_finite() => Ok(Some(n)),
                Some(_) => Err(AsError::at(
                    "memoize: 'ttlMs' must be a positive number",
                    span,
                )
                .into()),
                None => Err(AsError::at(
                    format!(
                        "memoize: 'ttlMs' must be a number, got {}",
                        crate::interp::type_name(&v)
                    ),
                    span,
                )
                .into()),
            },
        },
    }
}

/// Extract `(store_id, sfPrefix, ttlMs)` from a memoize policy Object.
fn memoize_read_config(recv: &Value) -> (u64, String, Option<f64>) {
    match recv.kind() {
        ValueKind::Object(o) => {
            let store_id = o
                .get("__store")
                .and_then(|v| match v.kind() {
                    ValueKind::Native(n) => Some(n.id),
                    _ => None,
                })
                .unwrap_or(u64::MAX);
            let sf_prefix = o
                .get("__sfPrefix")
                .and_then(|v| match v.kind() {
                    ValueKind::Str(s) => Some(s.to_string()),
                    _ => None,
                })
                .unwrap_or_default();
            let ttl_ms = o.get("ttlMs").and_then(|v| v.as_f64());
            (store_id, sf_prefix, ttl_ms)
        }
        _ => (u64::MAX, String::new(), None),
    }
}

/// If `v` is an ok-pair `[value, nil]` (2-element array, nil at `[1]`), return
/// `Some(value)`; otherwise `None`. Used to normalize a flight's success result
/// (the user fn may return a plain value OR an ok-pair) into the stored value.
fn result_pair_value(v: &Value) -> Option<Value> {
    match v.kind() {
        ValueKind::Array(a) => {
            let b = a.borrow();
            if b.len() == 2 && b[1] == Value::nil() {
                Some(b[0].clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Increment an integer counter field (`__hits` / `__misses`) on a policy Object.
fn memoize_bump(recv: &Value, field: &str) {
    if let ValueKind::Object(o) = recv.kind() {
        let cur = o.get(field).and_then(|v| v.as_int()).unwrap_or(0);
        o.insert(field, Value::int(cur + 1));
    }
}

// ── Interp::call_deadline (§5.2) ─────────────────────────────────────────────

impl Interp {
    /// `resilience.deadline(ms, fn)` — establish a deadline budget on the current
    /// task's ambient locals ([`crate::interp::TASK_LOCALS`]) for the duration of
    /// `fn`, then restore the previous locals.
    ///
    /// **Nesting only SHRINKS** (§5.2): the effective deadline is
    /// `min(existing, now + ms)` — a callee never extends its caller's budget. The
    /// new locals are a COW [`crate::interp::TaskLocals`] `Rc` (the existing
    /// `trace_id` is carried forward).
    ///
    /// **save → set → run → restore**, restore on ALL exits (incl. a panic): the
    /// callback runs via `call_value`, which returns a `Result` (a Tier-2 panic is
    /// `Err(Control::Panic)`, never a host unwind), so the restore line always
    /// executes before the outcome is inspected.
    ///
    /// **Task 4.1 scope (set/restore only):** this task establishes the budget and
    /// inheritance; it does NOT race a timer against the body (the enforcement race —
    /// cancelling the body when the deadline elapses — and the immediate
    /// expired-on-entry refusal are **Task 4.2**, deliberately deferred here). `ms`
    /// must be a finite number (Tier-2 panic otherwise).
    async fn call_deadline(
        &self,
        ms: Value,
        user_fn: Value,
        span: Span,
    ) -> Result<Value, Control> {
        use crate::interp::{make_pair, result_pair_err, TaskLocals, TASK_LOCALS};
        use crate::stdlib::time::real_monotonic_ms;
        use std::rc::Rc;

        // ── Validate `ms` (Tier-2 on non-number / non-finite) ─────────────────
        let ms = match ms.as_f64() {
            Some(n) if n.is_finite() => n,
            Some(_) => {
                return Err(AsError::at(
                    "resilience.deadline: 'ms' must be a finite number",
                    span,
                )
                .into())
            }
            None => {
                return Err(AsError::at(
                    format!(
                        "resilience.deadline: 'ms' must be a number, got {}",
                        crate::interp::type_name(&ms)
                    ),
                    span,
                )
                .into())
            }
        };

        // ── Compute the effective (shrunk) deadline + build the COW locals ────
        let now = self.clock_monotonic_ms(real_monotonic_ms());
        let new_at = now + ms;
        // Snapshot the current locals to compute the shrink + carry the trace id.
        let prev = crate::interp::task_locals_current();
        let effective_at = match prev.as_ref().and_then(|l| l.deadline_at_ms) {
            // Nested: a callee's deadline can only SHRINK the caller's budget.
            Some(existing) => existing.min(new_at),
            None => new_at,
        };
        let carried_trace = prev.as_ref().and_then(|l| l.trace_id.clone());
        let new_locals = Rc::new(TaskLocals {
            deadline_at_ms: Some(effective_at),
            trace_id: carried_trace,
        });

        // ── save → set (no borrow held across .await) ────────────────────────
        // `replace` returns the previous cell value so we restore it after. If the
        // task-local is somehow not in scope (`try_with` errs), fall back to running
        // the body without a deadline — never panic the host.
        let restore: Option<Option<Rc<TaskLocals>>> = TASK_LOCALS
            .try_with(|c| c.replace(Some(new_locals)))
            .ok();

        // A local closure so EVERY exit (already-expired, normal, expiry, panic,
        // try_with-failure) restores the previous locals exactly once — the cell
        // op is sync (no borrow held across `.await`).
        let do_restore = |restore: Option<Option<Rc<TaskLocals>>>| {
            if let Some(old) = restore {
                let _ = TASK_LOCALS.try_with(|c| c.set(old));
            }
        };

        // ── §5.2 already-expired on ENTRY: refuse, NEVER run the body ─────────
        // `remaining` reflects the EFFECTIVE (shrunk) deadline we just set. A
        // freshly-zero `deadline(0, fn)` or entering under an already-past outer
        // deadline returns the err pair immediately — `user_fn` is never called.
        let remaining = self.deadline_remaining_ms();
        if matches!(remaining, Some(r) if r <= 0.0) {
            do_restore(restore);
            return Ok(self.resil_deadline_exceeded_pair());
        }

        // ── §5.2 the race: body vs. the remaining budget ─────────────────────
        // Defensively: if no deadline is set (`remaining == None` — impossible
        // inside `deadline`, which always sets one), just await the body with no
        // race. Otherwise race the body future against a sleep of `remaining` ms;
        // on the sleep branch `tokio::select!` DROPS the body future →
        // cancel-on-drop cancels eagerly-spawned async work. A synchronous body
        // cannot be preempted (same truth as `task.timeout`): the deadline fires
        // after the body's last `.await` yields control.
        let outcome: Result<Value, Control> = match remaining {
            Some(r) => {
                let body = async {
                    let raw = self.call_value(user_fn, vec![], span).await;
                    match raw {
                        Ok(v) => match v.kind() {
                            crate::value::ValueKind::Future(_) => {
                                let crate::value::OwnedKind::Future(f) = v.into_kind() else {
                                    unreachable!()
                                };
                                f.get().await
                            }
                            _ => Ok(v),
                        },
                        other => other,
                    }
                };
                tokio::select! {
                    r = body => r,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(r as u64)) => {
                        do_restore(restore);
                        return Ok(self.resil_deadline_exceeded_pair());
                    }
                }
            }
            None => {
                let raw = self.call_value(user_fn, vec![], span).await;
                match raw {
                    Ok(v) => match v.kind() {
                        crate::value::ValueKind::Future(_) => {
                            let crate::value::OwnedKind::Future(f) = v.into_kind() else {
                                unreachable!()
                            };
                            f.get().await
                        }
                        _ => Ok(v),
                    },
                    other => other,
                }
            }
        };

        // ── restore on ALL exits (incl. panic, captured as Err above) ─────────
        do_restore(restore);

        // ── normalize to a `[value, err]` pair (matches `fallback`/the breaker) ─
        match outcome {
            Err(e) => Err(e),
            Ok(v) => {
                if result_pair_err(&v).is_some() {
                    // Already an err-pair (or a normalized result pair) — pass through.
                    Ok(v)
                } else {
                    Ok(make_pair(v, Value::nil()))
                }
            }
        }
    }

    /// `resilience.withTrace(id, fn)` — establish an ambient trace/request id on the
    /// current task's locals ([`crate::interp::TASK_LOCALS`]) for the duration of
    /// `fn`, then restore the previous locals.
    ///
    /// Same **save → set → run → restore** cell discipline as [`Self::call_deadline`]:
    /// the new locals are a COW [`crate::interp::TaskLocals`] `Rc` (the existing
    /// `deadline_at_ms` is carried forward, so a `withTrace` nested inside a
    /// `deadline` — or vice-versa — preserves BOTH fields). Restore runs on ALL exits
    /// (normal return / `?`-propagate / panic captured as `Err`). NO deadline
    /// enforcement race — `withTrace` only sets an id.
    ///
    /// Return convention MIRRORS `deadline`/`fallback`: a plain body value becomes a
    /// `[value, nil]` ok-pair; an err-pair passes through unchanged. (The §5.5 gateway
    /// idiom `withTrace(id, () => next(req))` returns the body's result; pair-normalizing
    /// keeps it consistent with the rest of the module.)
    async fn call_with_trace(
        &self,
        id: Value,
        user_fn: Value,
        span: Span,
    ) -> Result<Value, Control> {
        use crate::interp::{make_pair, result_pair_err, TaskLocals, TASK_LOCALS};
        use std::rc::Rc;

        // ── Validate `id` (Tier-2 on non-string) ──────────────────────────────
        let id: Rc<str> = match id.kind() {
            ValueKind::Str(s) => s.clone(),
            _ => {
                return Err(AsError::at(
                    format!(
                        "resilience.withTrace: 'id' must be a string, got {}",
                        crate::interp::type_name(&id)
                    ),
                    span,
                )
                .into())
            }
        };

        // ── Build the COW locals (carry the existing deadline forward) ────────
        let prev = crate::interp::task_locals_current();
        let carried_deadline = prev.as_ref().and_then(|l| l.deadline_at_ms);
        let new_locals = Rc::new(TaskLocals {
            deadline_at_ms: carried_deadline,
            trace_id: Some(id),
        });

        // ── save → set (no borrow held across .await) ────────────────────────
        let restore: Option<Option<Rc<TaskLocals>>> =
            TASK_LOCALS.try_with(|c| c.replace(Some(new_locals))).ok();
        let do_restore = |restore: Option<Option<Rc<TaskLocals>>>| {
            if let Some(old) = restore {
                let _ = TASK_LOCALS.try_with(|c| c.set(old));
            }
        };

        // ── run the body (drive a returned future, like `call_deadline`) ──────
        let raw = self.call_value(user_fn, vec![], span).await;
        let outcome: Result<Value, Control> = match raw {
            Ok(v) => match v.kind() {
                crate::value::ValueKind::Future(_) => {
                    let crate::value::OwnedKind::Future(f) = v.into_kind() else {
                        unreachable!()
                    };
                    f.get().await
                }
                _ => Ok(v),
            },
            other => other,
        };

        // ── restore on ALL exits (incl. panic, captured as Err above) ─────────
        do_restore(restore);

        // ── normalize to a `[value, err]` pair (matches `deadline`/`fallback`) ─
        match outcome {
            Err(e) => Err(e),
            Ok(v) => {
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
            "singleflight" => {
                let key = arg(args, 0);
                let user_fn = arg(args, 1);
                self.call_singleflight(key, user_fn, span)
            }
            "memoize" => {
                let opts = arg(args, 0);
                self.make_memoize(opts, span)
            }
            "deadline" => {
                let ms = arg(args, 0);
                let user_fn = arg(args, 1);
                self.call_deadline(ms, user_fn, span).await
            }
            "deadlineRemaining" => Ok(match self.deadline_remaining_ms() {
                Some(ms) => Value::float(ms),
                None => Value::nil(),
            }),
            "withTrace" => {
                let id = arg(args, 0);
                let user_fn = arg(args, 1);
                self.call_with_trace(id, user_fn, span).await
            }
            "traceId" => Ok(
                match crate::interp::task_locals_current().and_then(|l| l.trace_id.clone()) {
                    Some(id) => Value::str(&*id),
                    None => Value::nil(),
                },
            ),
            // §6.2: a callable handler the server mounts directly
            // (`server.get("/metrics", resilience.metricsHandler())`). It carries NO
            // per-instance state — the registry lives on `self.resilience`, read at
            // call time in `call_resilience_native_method`. Receiver is a fields-only
            // `NativeKind::Resilience` marker (id = u64::MAX, like `__local`).
            "metricsHandler" => Ok(Value::native_method(Rc::new(crate::value::NativeMethod {
                receiver: Rc::new(NativeObject {
                    id: u64::MAX,
                    kind: NativeKind::Resilience,
                    fields: IndexMap::new(),
                }),
                method: "__metrics".to_string(),
            }))),
            // §6.3: a callable readiness/liveness handler the server mounts directly
            // (`server.get("/readyz", resilience.health({checks, timeoutMs}))`). The
            // `{checks, timeoutMs}` config is stashed verbatim in the handle's
            // `NativeObject.fields` under `__config` and read at call time in
            // `call_resilience_native_method` (the request arg is ignored — readiness
            // carries no meaningful body).
            "health" => {
                let cfg = arg(args, 0);
                let mut fields: IndexMap<String, Value> = IndexMap::new();
                fields.insert("__config".to_string(), cfg);
                Ok(Value::native_method(Rc::new(crate::value::NativeMethod {
                    receiver: Rc::new(NativeObject {
                        id: u64::MAX,
                        kind: NativeKind::Resilience,
                        fields,
                    }),
                    method: "__health".to_string(),
                })))
            }
            // §6.4: a callable policy-wrapped HTTP handler the server mounts directly
            // (`server.get("/quote", resilience.handler(policies, fn))`). Both the
            // `policies` object and the wrapped `fn` are stashed in the handle's
            // `NativeObject.fields` (`__policies`, `__fn`); composition + code→status
            // mapping happens at call time in `call_resilience_native_method`.
            "handler" => {
                let policies = arg(args, 0);
                let user_fn = arg(args, 1);
                let mut fields: IndexMap<String, Value> = IndexMap::new();
                fields.insert("__policies".to_string(), policies);
                fields.insert("__fn".to_string(), user_fn);
                Ok(Value::native_method(Rc::new(crate::value::NativeMethod {
                    receiver: Rc::new(NativeObject {
                        id: u64::MAX,
                        kind: NativeKind::Resilience,
                        fields,
                    }),
                    method: "__handler".to_string(),
                })))
            }
            other => Err(AsError::at(
                format!("resilience.{}: not implemented in this build", other),
                span,
            )
            .into()),
        }
    }

    /// Dispatch a `NativeKind::Resilience` `NativeMethod` (§6.2). Today the only
    /// method is `__metrics` (the handler minted by `resilience.metricsHandler()`):
    /// render the per-isolate registry as Prometheus text and return an HTTP
    /// response object `{status, headers, body}` the server understands — the
    /// `text/plain; version=0.0.4` content-type set explicitly so `value_to_response`
    /// does not override it with `text/plain; charset=utf-8`. The request arg is
    /// ignored (a Prometheus scrape carries no meaningful body).
    pub(crate) async fn call_resilience_native_method(
        &self,
        m: &Rc<crate::value::NativeMethod>,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match m.method.as_str() {
            "__metrics" => {
                // Prometheus scrape carries no meaningful body; the request arg is
                // ignored (preserved as a binding so all arms share the signature).
                let _ = &args;
                let body = render_prometheus(&self.resilience.borrow().registry);
                let headers = Value::object(
                    [(
                        "content-type".to_string(),
                        Value::str("text/plain; version=0.0.4"),
                    )]
                    .into_iter()
                    .collect(),
                );
                Ok(Value::object(
                    [
                        ("status".to_string(), Value::int(200)),
                        ("headers".to_string(), headers),
                        ("body".to_string(), Value::str(body.as_str())),
                    ]
                    .into_iter()
                    .collect(),
                ))
            }
            // §6.3: readiness/liveness. Run every check (sequential, registration
            // order), each contained (`recover`-style — a panicking check never 500s
            // the endpoint) under a per-check `timeoutMs` deadline, and report.
            "__health" => {
                let cfg = m.receiver.fields.get("__config").cloned().unwrap_or(Value::nil());
                self.run_health(&cfg, span).await
            }
            // §6.4: the policy-wrapped handler. Read `policies`+`fn`+the request arg,
            // compose the fixed policy stack, and map a policy-rejection code to its
            // HTTP status (429/503/504); a real handler error/success passes through.
            "__handler" => {
                let policies = m.receiver.fields.get("__policies").cloned().unwrap_or(Value::nil());
                let user_fn = m.receiver.fields.get("__fn").cloned().unwrap_or(Value::nil());
                let req = args.into_iter().next().unwrap_or(Value::nil());
                self.run_handler(&policies, &user_fn, req, span).await
            }
            // §6.4: an internal composition-step callable — the `user_fn` a policy
            // method (bulkhead/breaker/deadline) invokes with no args. It re-enters
            // the policy stack at the stored `__layer`, threading the stored request.
            "__handler_step" => {
                let policies = m.receiver.fields.get("__policies").cloned().unwrap_or(Value::nil());
                let user_fn = m.receiver.fields.get("__fn").cloned().unwrap_or(Value::nil());
                let req = m.receiver.fields.get("__req").cloned().unwrap_or(Value::nil());
                let layer = m
                    .receiver
                    .fields
                    .get("__layer")
                    .and_then(|v| v.as_int())
                    .unwrap_or(0) as u8;
                self.handler_layer(&policies, &user_fn, req, layer, span).await
            }
            other => Err(AsError::at(
                format!("resilience handler has no method '{}'", other),
                span,
            )
            .into()),
        }
    }

    // ── §6.3 health composition ───────────────────────────────────────────────

    /// Run all health checks (sequential, registration order) under per-check
    /// containment + a per-check `timeoutMs` deadline, and assemble the HTTP
    /// response object. All pass → 200 `{"status":"ok",...}`; any fail → 503
    /// `{"status":"degraded",...}`. Body is a hand-built JSON string + an explicit
    /// `application/json` content-type (the server stringifies an Object body via
    /// display, not JSON — so we serialize here, like `__metrics`).
    async fn run_health(&self, cfg: &Value, span: Span) -> Result<Value, Control> {
        // Pull `checks` (key → fn) + optional `timeoutMs` out of the config object.
        let (checks, timeout_ms): (Vec<(String, Value)>, Option<f64>) = match cfg.kind() {
            ValueKind::Object(o) => {
                let checks = match o.get("checks").as_ref().map(|v| v.kind()) {
                    Some(ValueKind::Object(c)) => c
                        .entries()
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v.clone()))
                        .collect(),
                    _ => Vec::new(),
                };
                let timeout = o.get("timeoutMs").and_then(|v| v.as_f64());
                (checks, timeout)
            }
            _ => (Vec::new(), None),
        };

        // Run each check; collect (name, ok, optional error message). Containment is
        // total: a panic / propagate / falsy / err-pair / timeout all read as a fail
        // and NEVER escape (`recover`-style — one bad check cannot 500 the endpoint).
        let mut results: Vec<(String, bool, Option<String>)> = Vec::with_capacity(checks.len());
        let mut all_ok = true;
        for (name, check_fn) in checks {
            let (ok, err) = self.run_one_check(check_fn, timeout_ms, span).await;
            if !ok {
                all_ok = false;
            }
            results.push((name, ok, err));
        }

        let status = if all_ok { 200 } else { 503 };
        let body = health_json(all_ok, &results);
        let headers = Value::object(
            [("content-type".to_string(), Value::str("application/json"))]
                .into_iter()
                .collect(),
        );
        Ok(Value::object(
            [
                ("status".to_string(), Value::int(status)),
                ("headers".to_string(), headers),
                ("body".to_string(), Value::str(body.as_str())),
            ]
            .into_iter()
            .collect(),
        ))
    }

    /// Run ONE health check fn (driving a returned future), under an optional
    /// per-check `timeoutMs` deadline, fully contained. Returns `(ok, Some(error))`.
    /// Pass: truthy plain value OR ok-pair `[v, nil]`. Fail: falsy, err-pair,
    /// panic/propagate (the message captured), or deadline-exceeded.
    async fn run_one_check(
        &self,
        check_fn: Value,
        timeout_ms: Option<f64>,
        span: Span,
    ) -> (bool, Option<String>) {
        // A timeout wraps the check in `deadline(ms, () => check())`; the deadline
        // helper already drives a returned future + contains the body in a pair.
        let raw = match timeout_ms {
            Some(ms) => self.call_deadline(Value::float(ms), check_fn, span).await,
            None => {
                let r = self.call_value(check_fn, vec![], span).await;
                // Drive a returned future to completion (like deadline/breaker do).
                match r {
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
                }
            }
        };

        match raw {
            // A panic/propagate is contained → reported as a failed check.
            Err(Control::Panic(e)) => (false, Some(e.message)),
            Err(Control::Propagate(p)) => {
                let msg = crate::interp::result_pair_err(&p)
                    .map(|e| health_err_message(&e))
                    .unwrap_or_else(|| "check propagated".to_string());
                (false, Some(msg))
            }
            // Exit must not be contained — let it surface (process-level intent).
            Err(other) => {
                // Defensive: treat any other Control as a fail without escaping the
                // endpoint, EXCEPT Exit which we re-raise via the caller path. Since
                // run_one_check has no Result return, fold Exit into a fail too — an
                // exit() inside a health check is pathological; containment wins.
                let _ = other;
                (false, Some("check did not complete".to_string()))
            }
            Ok(v) => {
                // `deadline`/no-timeout both may hand back a pair; classify it.
                if let Some(e) = crate::interp::result_pair_err(&v) {
                    return (false, Some(health_err_message(&e)));
                }
                // An ok-pair `[v, nil]` — unwrap to the inner value for truthiness.
                let inner = match v.kind() {
                    ValueKind::Array(a) => {
                        let b = a.borrow();
                        if b.len() == 2 && b[1] == Value::nil() {
                            b[0].clone()
                        } else {
                            v.clone()
                        }
                    }
                    _ => v.clone(),
                };
                if inner.is_truthy() {
                    (true, None)
                } else {
                    (false, Some("check returned a falsy value".to_string()))
                }
            }
        }
    }

    // ── §6.4 handler composition ──────────────────────────────────────────────

    /// Entry: run the policy-wrapped handler for one request and map the outcome
    /// to an HTTP response. Composes the fixed stack (limiter → bulkhead → breaker
    /// → deadline → fn), then inspects the resulting pair: a known policy-rejection
    /// `code` → the mapped status object (429/503/504); an ok-pair → the handler's
    /// own value (unwrapped — the server serializes it); any other error pair → the
    /// pair UNCHANGED (the server's existing 500 path).
    async fn run_handler(
        &self,
        policies: &Value,
        user_fn: &Value,
        req: Value,
        span: Span,
    ) -> Result<Value, Control> {
        let outcome = self.handler_layer(policies, user_fn, req, 0, span).await?;
        // The composed stack always returns a pair (every policy normalizes; the
        // identity `{}` path normalizes too). Classify the error half.
        if let Some(err) = crate::interp::result_pair_err(&outcome) {
            let code = match err.kind() {
                ValueKind::Object(o) => match o.get("code").as_ref().map(|v| v.kind()) {
                    Some(ValueKind::Str(s)) => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            };
            match code.as_deref() {
                Some("rate-limited") => {
                    return Ok(handler_status_response(429, retry_after_of(&err)));
                }
                Some("bulkhead-full") => return Ok(handler_status_response(503, None)),
                Some("breaker-open") => {
                    return Ok(handler_status_response(503, retry_after_of(&err)));
                }
                Some("deadline-exceeded") => return Ok(handler_status_response(504, None)),
                // Any other error pair from the handler itself: pass through unchanged
                // so the server's existing semantics (value_to_response → 500) apply.
                _ => return Ok(outcome),
            }
        }
        // Ok-pair: hand back the handler's value (unwrapped). `value_to_response`
        // also unwraps an ok-pair, but returning the bare value keeps a unit-tested
        // `resp.status` working without the server in the loop.
        match outcome.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                if b.len() == 2 && b[1] == Value::nil() {
                    return Ok(b[0].clone());
                }
                drop(b);
                Ok(outcome)
            }
            _ => Ok(outcome),
        }
    }

    /// Apply ONE policy layer and recurse. Fixed order (outermost first):
    /// 0 = limiter (sync `tryAcquire`; on reject → `rate-limited` pair),
    /// 1 = bulkhead (`run`), 2 = breaker (`call`), 3 = deadline, 4 = the handler fn.
    /// A layer with no configured policy falls through to the next. Each delegating
    /// layer passes a `__handler_step` NativeMethod (carrying policies/fn/req/next
    /// layer) as the policy method's `user_fn`, so the policy invokes our recursion.
    #[async_recursion::async_recursion(?Send)]
    async fn handler_layer(
        &self,
        policies: &Value,
        user_fn: &Value,
        req: Value,
        layer: u8,
        span: Span,
    ) -> Result<Value, Control> {
        use crate::interp::make_pair;

        let policy = |name: &str| -> Option<Value> {
            match policies.kind() {
                ValueKind::Object(o) => o.get(name).filter(|v| !matches!(v.kind(), ValueKind::Nil)),
                _ => None,
            }
        };

        match layer {
            // ── Layer 0: limiter / keyedLimiter (sync, non-blocking tryAcquire) ──
            0 => {
                if let Some(lim) = policy("limiter") {
                    let kind = resil_kind(&lim).map(|k| k.to_string());
                    let granted = match kind.as_deref() {
                        Some("keyedLimiter") => {
                            // Resolve the partition key via the optional `key` fn.
                            let key = match policy("key") {
                                Some(kf) => self.call_value(kf, vec![req.clone()], span).await?,
                                None => Value::str("anon"),
                            };
                            let r = self
                                .call_keyed_limiter_method(
                                    &lim,
                                    "tryAcquire",
                                    &[lim.clone(), key],
                                    span,
                                )
                                .await?;
                            matches!(r.kind(), ValueKind::Bool(true))
                        }
                        Some("limiter") => {
                            let r = self
                                .call_limiter_method(
                                    &lim,
                                    "tryAcquire",
                                    std::slice::from_ref(&lim),
                                    span,
                                )
                                .await?;
                            matches!(r.kind(), ValueKind::Bool(true))
                        }
                        // A non-limiter under the `limiter` key: ignore (no shed).
                        _ => true,
                    };
                    if !granted {
                        let mut err: IndexMap<String, Value> = IndexMap::new();
                        err.insert("message".to_string(), Value::str("rate limit exceeded"));
                        err.insert("code".to_string(), Value::str("rate-limited"));
                        if let Some(ra) = limiter_retry_after_secs(&lim) {
                            err.insert("retryAfter".to_string(), Value::float(ra));
                        }
                        return Ok(make_pair(Value::nil(), Value::object(err)));
                    }
                }
                self.handler_layer(policies, user_fn, req, 1, span).await
            }
            // ── Layer 1: bulkhead.run(step@2) ───────────────────────────────────
            1 => {
                if let Some(bh) = policy("bulkhead") {
                    let inner = self.handler_step(policies, user_fn, &req, 2);
                    return self.bulkhead_run(&bh, inner, span).await;
                }
                self.handler_layer(policies, user_fn, req, 2, span).await
            }
            // ── Layer 2: breaker.call(step@3) ───────────────────────────────────
            2 => {
                if let Some(b) = policy("breaker") {
                    let inner = self.handler_step(policies, user_fn, &req, 3);
                    return self.breaker_call(&b, inner, span).await;
                }
                self.handler_layer(policies, user_fn, req, 3, span).await
            }
            // ── Layer 3: deadline(ms, step@4) ───────────────────────────────────
            3 => {
                if let Some(ms) = policy("deadlineMs") {
                    let inner = self.handler_step(policies, user_fn, &req, 4);
                    return self.call_deadline(ms, inner, span).await;
                }
                self.handler_layer(policies, user_fn, req, 4, span).await
            }
            // ── Layer 4: the handler fn itself; normalize to a pair ─────────────
            _ => {
                let r = self.call_value(user_fn.clone(), vec![req], span).await?;
                // Drive a returned future to completion (async handler). Return the
                // handler's value RAW (no `[v,nil]` wrap): each wrapping policy
                // (bulkhead/breaker/deadline) normalizes a bare value to a pair
                // exactly once, and `run_handler` normalizes the no-policy path. A
                // premature wrap here would be double-wrapped by `deadline` (which
                // re-wraps an OK-pair, unlike breaker/bulkhead's is-pair guard).
                match r.kind() {
                    ValueKind::Future(_) => {
                        let crate::value::OwnedKind::Future(f) = r.into_kind() else {
                            unreachable!()
                        };
                        f.get().await
                    }
                    _ => Ok(r),
                }
            }
        }
    }

    /// Build a `__handler_step` callable for `next_layer`, carrying the full handler
    /// state (policies/fn/req) in its `NativeObject.fields` so the policy method that
    /// invokes it (with no args) re-enters `handler_layer` at the right point.
    fn handler_step(&self, policies: &Value, user_fn: &Value, req: &Value, next_layer: u8) -> Value {
        let mut fields: IndexMap<String, Value> = IndexMap::new();
        fields.insert("__policies".to_string(), policies.clone());
        fields.insert("__fn".to_string(), user_fn.clone());
        fields.insert("__req".to_string(), req.clone());
        fields.insert("__layer".to_string(), Value::int(next_layer as i64));
        Value::native_method(Rc::new(crate::value::NativeMethod {
            receiver: Rc::new(NativeObject {
                id: u64::MAX,
                kind: NativeKind::Resilience,
                fields,
            }),
            method: "__handler_step".to_string(),
        }))
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

    // ── Task 3.2: resilience.singleflight (§3.6) ────────────────────────────

    /// Concurrent same-key calls COLLAPSE to one execution; both awaiters get
    /// the same value, `fn` runs exactly once.
    #[tokio::test]
    async fn singleflight_collapses_concurrent_calls() {
        let out = run(r#"
import * as resilience from "std/resilience"
let calls = [0]
async fn fetchIt() { calls[0] = calls[0] + 1; return 42 }
let f1 = resilience.singleflight("k", fetchIt)
let f2 = resilience.singleflight("k", fetchIt)
print(await f1); print(await f2); print(calls[0])
"#)
        .await;
        assert_eq!(out, "42\n42\n1\n");
    }

    /// PANIC FAN-OUT: a panicking flight delivers the SAME panic to BOTH
    /// awaiters (two `recover`s, both err messages equal — the §3.6 SharedFuture
    /// argument).
    #[tokio::test]
    async fn singleflight_panic_fans_out_to_all_awaiters() {
        let out = run(r#"
import * as resilience from "std/resilience"
async fn boom() { assert(false, "kaboom") }
let f1 = resilience.singleflight("k", boom)
let f2 = resilience.singleflight("k", boom)
let [v1, e1] = recover(() => await f1)
let [v2, e2] = recover(() => await f2)
print(e1.message)
print(e2.message)
print(e1.message == e2.message)
"#)
        .await;
        assert_eq!(out, "kaboom\nkaboom\ntrue\n");
    }

    /// NO result caching: after a flight settles, the key is reusable — a fresh
    /// same-key call re-runs `fn` (sequential singleflight→await→singleflight).
    #[tokio::test]
    async fn singleflight_key_reusable_after_settle() {
        let out = run(r#"
import * as resilience from "std/resilience"
let calls = [0]
async fn fetchIt() { calls[0] = calls[0] + 1; return calls[0] }
let a = await resilience.singleflight("k", fetchIt)
let b = await resilience.singleflight("k", fetchIt)
print(a); print(b); print(calls[0])
"#)
        .await;
        // Two SEQUENTIAL flights → fn ran TWICE.
        assert_eq!(out, "1\n2\n2\n");
    }

    /// The flights table is emptied after success AND after failure
    /// (Rust-level assertion over `interp.resilience.borrow().flights`).
    #[tokio::test]
    async fn singleflight_table_emptied_after_settle() {
        // Success path.
        let (out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
async fn ok() { return 1 }
print(await resilience.singleflight("k", ok))
"#)
        .await
        .expect("program should run");
        assert_eq!(out, "1\n");
        assert!(
            interp.resilience.borrow().flights.is_empty(),
            "flights table must be empty after a successful flight settles"
        );

        // Failure (panic) path.
        let (out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
async fn boom() { assert(false, "x") }
let [v, e] = recover(() => await resilience.singleflight("k", boom))
print(e.message)
"#)
        .await
        .expect("program should run");
        assert_eq!(out, "x\n");
        assert!(
            interp.resilience.borrow().flights.is_empty(),
            "flights table must be empty after a panicking flight settles"
        );
    }

    /// A non-string key is a Tier-2 panic.
    #[tokio::test]
    async fn singleflight_non_string_key_is_tier2() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
async fn f() { return 1 }
let _ = await resilience.singleflight(42, f)
"#)
        .await;
        let msg = match res {
            Err(e) => format!("{e}"),
            Ok(s) => s,
        };
        assert!(
            msg.contains("key must be a string"),
            "expected non-string key Tier-2 panic, got: {msg:?}"
        );
    }

    /// Driver-leak check: a singleflight whose awaiters are all dropped
    /// mid-flight must NOT leak — the table holds the handle (so the flight is
    /// not cancelled), the short-lived driver completes + removes the table
    /// entry, and the isolate exits cleanly. We start a flight, then DROP the
    /// returned future without awaiting it (reassign to nil), and confirm the
    /// program runs to completion + the table is empty afterwards.
    #[tokio::test]
    async fn singleflight_dropped_awaiters_exit_cleanly() {
        let (out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
let ran = [0]
async fn slow() { ran[0] = ran[0] + 1; return 7 }
let f = resilience.singleflight("k", slow)
f = nil   // drop the only handle the caller holds, mid-flight, without awaiting
print("done")
"#)
        .await
        .expect("program should run");
        assert_eq!(out, "done\n");
        // The flight completes for whoever joins next; the driver removes the
        // entry on resolve, so the table is empty and nothing leaks.
        assert!(
            interp.resilience.borrow().flights.is_empty(),
            "flights table must be empty — a dropped-awaiter flight must not leak"
        );
    }

    /// Adversarial driver-leak hunt (Task 3.4 review): start N flights on
    /// DISTINCT keys, drop ALL awaiter handles mid-flight (never await), and
    /// confirm the isolate exits cleanly AND the flights table empties (every
    /// short-lived driver runs its `shift_remove`). Exercises the multi-entry
    /// path the single-key drop test does not.
    #[tokio::test]
    async fn singleflight_many_dropped_flights_leave_no_leak() {
        let (out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
let ran = [0]
async fn slow(n) { ran[0] = ran[0] + 1; return n }
// Start 16 flights on distinct keys; drop every handle without awaiting.
let i = 0
while (i < 16) {
    let f = resilience.singleflight(`key-${i}`, () => slow(i))
    f = nil
    i = i + 1
}
print("done")
"#)
        .await
        .expect("program should run");
        assert_eq!(out, "done\n");
        assert!(
            interp.resilience.borrow().flights.is_empty(),
            "flights table must be empty — N dropped-awaiter flights must not leak"
        );
    }

    // ── Task 3.3: resilience.memoize (§3.7) ─────────────────────────────────

    /// Stampede protection: N concurrent `cache.get("k", fn)` collapse to ONE
    /// `fn` execution (singleflight), and every caller observes the same value.
    #[tokio::test]
    async fn memoize_stampede_collapses_concurrent_misses() {
        let out = run(r#"
import * as resilience from "std/resilience"
let calls = [0]
async fn fetchIt() { calls[0] = calls[0] + 1; return 42 }
let cache = resilience.memoize({ max: 100 })
let f1 = cache.get("k", fetchIt)
let f2 = cache.get("k", fetchIt)
let f3 = cache.get("k", fetchIt)
let [v1, e1] = await f1
let [v2, e2] = await f2
let [v3, e3] = await f3
print(v1); print(v2); print(v3); print(calls[0])
"#)
        .await;
        assert_eq!(out, "42\n42\n42\n1\n");
    }

    /// A second `get` on a cached key is a HIT: `fn` does NOT re-run.
    #[tokio::test]
    async fn memoize_hit_does_not_rerun() {
        let out = run(r#"
import * as resilience from "std/resilience"
let calls = [0]
async fn fetchIt() { calls[0] = calls[0] + 1; return 7 }
let cache = resilience.memoize({ max: 100 })
let [a, ea] = await cache.get("k", fetchIt)
let [b, eb] = await cache.get("k", fetchIt)
print(a); print(b); print(calls[0])
"#)
        .await;
        assert_eq!(out, "7\n7\n1\n");
    }

    /// `stats()` reports hits/misses: one miss (first get) + one hit (second).
    #[tokio::test]
    async fn memoize_stats_hits_and_misses() {
        let out = run(r#"
import * as resilience from "std/resilience"
async fn fetchIt() { return 1 }
let cache = resilience.memoize({ max: 100 })
await cache.get("k", fetchIt)
await cache.get("k", fetchIt)
await cache.get("k", fetchIt)
let s = cache.stats()
print(s.hits); print(s.misses)
"#)
        .await;
        assert_eq!(out, "2\n1\n");
    }

    /// TTL boundary under the virtual clock: ttlMs=100, hit at +99, miss at +101.
    #[tokio::test]
    async fn memoize_ttl_boundary_virtual_clock() {
        let out = crate::run_source_deterministic(r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
let calls = [0]
async fn fetchIt() { calls[0] = calls[0] + 1; return calls[0] }
let cache = resilience.memoize({ max: 100, ttlMs: 100 })
let [a, ea] = await cache.get("k", fetchIt)   // miss → fn ran (calls=1)
print(a)
await time.sleep(99)
let [b, eb] = await cache.get("k", fetchIt)   // +99 < 100 → HIT (fn not run)
print(b)
await time.sleep(2)                            // total +101 >= 100 → expired
let [c, ec] = await cache.get("k", fetchIt)   // miss → fn ran (calls=2)
print(c)
print(calls[0])
"#, 42).await.expect("deterministic run should succeed");
        assert_eq!(out, "1\n1\n2\n2\n");
    }

    /// Eviction via `max` (lru semantics): max=2 → inserting a third key evicts
    /// the least-recently-used; a re-get on the evicted key re-runs `fn`.
    #[tokio::test]
    async fn memoize_eviction_via_max() {
        let out = run(r#"
import * as resilience from "std/resilience"
let calls = [0]
async fn f() { calls[0] = calls[0] + 1; return calls[0] }
let cache = resilience.memoize({ max: 2 })
await cache.get("a", f)   // calls=1, store {a}
await cache.get("b", f)   // calls=2, store {a,b}
await cache.get("c", f)   // calls=3, evicts "a", store {b,c}
print(cache.len())        // 2
let [v, e] = await cache.get("a", f)   // "a" evicted → miss → calls=4
print(v)
print(calls[0])           // 4
"#)
        .await;
        assert_eq!(out, "2\n4\n4\n");
    }

    /// Errors are NOT cached: an err-pair-returning fn that later succeeds is
    /// re-run on the next get.
    #[tokio::test]
    async fn memoize_err_pair_not_cached() {
        let out = run(r#"
import * as resilience from "std/resilience"
let calls = [0]
async fn f() {
  calls[0] = calls[0] + 1
  if (calls[0] == 1) { return [nil, {message: "boom"}] }
  return 99
}
let cache = resilience.memoize({ max: 100 })
let [v1, e1] = await cache.get("k", f)   // err-pair → NOT cached
print(e1.message)
let [v2, e2] = await cache.get("k", f)   // re-runs → success
print(v2)
print(calls[0])                          // 2
"#)
        .await;
        assert_eq!(out, "boom\n99\n2\n");
    }

    /// Panics are NOT cached: a panicking fn that later succeeds is re-run.
    #[tokio::test]
    async fn memoize_panic_not_cached() {
        let out = run(r#"
import * as resilience from "std/resilience"
let calls = [0]
async fn f() {
  calls[0] = calls[0] + 1
  if (calls[0] == 1) { assert(false, "kaboom") }
  return 5
}
let cache = resilience.memoize({ max: 100 })
let [v1, e1] = recover(() => await cache.get("k", f))
print(e1.message)
let [v2, e2] = await cache.get("k", f)   // re-runs → success
print(v2)
print(calls[0])                          // 2
"#)
        .await;
        assert_eq!(out, "kaboom\n5\n2\n");
    }

    /// `delete`/`clear`/`len` behave as expected.
    #[tokio::test]
    async fn memoize_delete_clear_len() {
        let out = run(r#"
import * as resilience from "std/resilience"
async fn f() { return 1 }
let cache = resilience.memoize({ max: 100 })
await cache.get("a", f)
await cache.get("b", f)
print(cache.len())     // 2
cache.delete("a")
print(cache.len())     // 1
cache.clear()
print(cache.len())     // 0
"#)
        .await;
        assert_eq!(out, "2\n1\n0\n");
    }

    /// Two caches never collide on the global singleflight table: a key "k" in
    /// cache1 and "k" in cache2 are independent flights (distinct __sfPrefix).
    #[tokio::test]
    async fn memoize_distinct_caches_dont_collide() {
        let out = run(r#"
import * as resilience from "std/resilience"
let calls = [0]
async fn f() { calls[0] = calls[0] + 1; return calls[0] }
let c1 = resilience.memoize({ max: 100 })
let c2 = resilience.memoize({ max: 100 })
let [a, ea] = await c1.get("k", f)   // calls=1
let [b, eb] = await c2.get("k", f)   // distinct cache → calls=2 (no collision)
print(a); print(b); print(calls[0])
"#)
        .await;
        assert_eq!(out, "1\n2\n2\n");
    }

    /// A non-string key is a Tier-2 panic.
    #[tokio::test]
    async fn memoize_non_string_key_is_tier2() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
async fn f() { return 1 }
let cache = resilience.memoize({ max: 100 })
let _ = await cache.get(42, f)
"#)
        .await;
        let msg = match res {
            Err(e) => format!("{e}"),
            Ok(s) => s,
        };
        assert!(
            msg.contains("key must be a string"),
            "expected non-string key Tier-2 panic, got: {msg:?}"
        );
    }

    /// Invalid `max` (non-positive) is a Tier-2 panic at construction.
    #[tokio::test]
    async fn memoize_invalid_max_is_tier2() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
let cache = resilience.memoize({ max: 0 })
"#)
        .await;
        let msg = match res {
            Err(e) => format!("{e}"),
            Ok(s) => s,
        };
        assert!(
            msg.contains("memoize") && msg.contains("max"),
            "expected memoize 'max' validation Tier-2 panic, got: {msg:?}"
        );
    }

    // ── Task 3.4 review: kind × method dispatch-hole sweep ────────────────────
    //
    // For EVERY kind in RESIL_KINDS × the full `is_resilience_method` allowlist,
    // a method NOT in that kind's own valid set MUST raise EXACTLY
    //   "<kind> policy has no method '<name>'"
    // — never silently accept a foreign method (a dispatch-table hole), never a
    // different message. This catches both a missing `other =>` arm and a kind
    // that leaks another kind's method.
    #[tokio::test]
    async fn kind_method_dispatch_sweep() {
        // The COMPLETE method allowlist routed through the call-site hook.
        // Kept in sync with `is_resilience_method`.
        const ALL_METHODS: &[&str] = &[
            "call", "state", "stats", "reset", "acquire", "tryAcquire", "run", "get", "delete",
            "clear", "len",
        ];
        // Assert the allowlist constant matches `is_resilience_method` exactly
        // (so this sweep can never silently fall out of date).
        for m in ALL_METHODS {
            assert!(
                super::is_resilience_method(m),
                "ALL_METHODS lists '{m}' but is_resilience_method rejects it"
            );
        }

        // (kind, constructor-expr, valid-method-set)
        let kinds: &[(&str, &str, &[&str])] = &[
            ("breaker", "resilience.breaker({})", &["state", "stats", "reset", "call"]),
            (
                "limiter",
                "resilience.limiter({capacity: 2, refillPerSec: 1.0})",
                &["tryAcquire", "acquire"],
            ),
            (
                "keyedLimiter",
                "resilience.keyedLimiter({capacity: 5, refillPerSec: 0.0, maxKeys: 10})",
                &["tryAcquire", "acquire", "stats"],
            ),
            ("bulkhead", "resilience.bulkhead({limit: 1})", &["run"]),
            ("retry", "resilience.retry({attempts: 3, baseMs: 1})", &["call", "stats", "reset"]),
            (
                "memoize",
                "resilience.memoize({max: 10})",
                &["get", "delete", "clear", "len", "stats"],
            ),
        ];

        for (kind, ctor, valid) in kinds {
            for method in ALL_METHODS {
                if valid.contains(method) {
                    // Positive pairs are exercised by the per-kind tests; here we
                    // only prove the negative pairs do NOT silently work.
                    continue;
                }
                // Call the foreign method with no args (the dispatch arm runs
                // BEFORE any arg handling, so arity is irrelevant — a present
                // arm would either succeed or raise an arity/type panic, both of
                // which would FAIL this assertion and expose the hole).
                let src = format!(
                    "import * as resilience from \"std/resilience\"\n\
                     let p = {ctor}\n\
                     let _ = p.{method}()\n"
                );
                let res = crate::run_source(&src).await;
                let msg = match res {
                    Err(e) => format!("{e}"),
                    Ok(s) => s,
                };
                let expected = format!("{kind} policy has no method '{method}'");
                assert!(
                    msg.contains(&expected),
                    "kind '{kind}' method '{method}': expected {expected:?}, got: {msg:?}"
                );
            }
        }
    }

    // ── Task 4.1: resilience.deadline / deadlineRemaining (§5.1–§5.3) ──────────

    /// The deadline local is captured at the spawn site so a child async fn spawned
    /// WHILE the deadline is set inherits it; restored to nil at top level after.
    /// Exercises the spawn-site capture wrap (the critical seam).
    #[tokio::test]
    async fn deadline_local_inherited_by_spawned_async_fn() {
        let out = run(r#"
import * as resilience from "std/resilience"
async fn child() { return resilience.deadlineRemaining() != nil }
let [v, err] = resilience.deadline(60000, async () => {
    let f = child()              // spawned WHILE the deadline local is set
    return await f
})
print(v)
print(resilience.deadlineRemaining())   // restored: nil at top level
"#)
        .await;
        assert_eq!(out, "true\nnil\n");
    }

    /// deadlineRemaining() is nil when no deadline is set.
    #[tokio::test]
    async fn deadline_remaining_nil_outside_deadline() {
        let out = run(r#"
import * as resilience from "std/resilience"
print(resilience.deadlineRemaining())
"#)
        .await;
        assert_eq!(out, "nil\n");
    }

    /// A panicking deadline body still restores the previous locals: after a
    /// recover()-caught panic, deadlineRemaining() is nil at top level.
    #[tokio::test]
    async fn deadline_restores_on_panic() {
        let out = run(r#"
import * as resilience from "std/resilience"
let [v, err] = recover(() => resilience.deadline(60000, () => { assert(false, "boom") }))
print(resilience.deadlineRemaining())   // restored even though the body panicked
"#)
        .await;
        assert_eq!(out, "nil\n");
    }

    /// Nested deadlines only SHRINK: an inner deadline(120000) inside an outer
    /// deadline(60000) sees the SHRUNK budget (≤ outer), never the larger 120000.
    #[tokio::test]
    async fn deadline_nesting_only_shrinks() {
        let out = run(r#"
import * as resilience from "std/resilience"
let observed = [0.0]
resilience.deadline(60000, () => {
    resilience.deadline(120000, () => {
        // inner budget must be the SHRUNK one (<= 60000), not 120000
        observed[0] = resilience.deadlineRemaining()
        return nil
    })
    return nil
})
print(observed[0] <= 60000)
"#)
        .await;
        assert_eq!(out, "true\n");
    }

    /// deadline returns a `[value, nil]` ok-pair on a plain-value body.
    #[tokio::test]
    async fn deadline_wraps_plain_value_as_ok_pair() {
        let out = run(r#"
import * as resilience from "std/resilience"
let [v, err] = resilience.deadline(1000, () => 42)
print(v)
print(err)
"#)
        .await;
        assert_eq!(out, "42\nnil\n");
    }

    /// §5.2 the race: a body that sleeps far past the deadline is cancelled and the
    /// caller gets `[nil, {code:"deadline-exceeded"}]` — and the body's side effect
    /// (a flag set AFTER the long sleep) never happens (the body future is dropped).
    #[tokio::test]
    async fn deadline_body_exceeds_returns_err_and_cancels() {
        let out = run(r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
let ran = [false]
let [v, err] = resilience.deadline(50, async () => {
    await time.sleep(500)
    ran[0] = true          // must NEVER run — body cancelled at ~50ms
    return "done"
})
print(v)
print(err.code)
print(ran[0])
"#)
        .await;
        assert_eq!(out, "nil\ndeadline-exceeded\nfalse\n");
    }

    /// §5.2 already-expired on entry: `deadline(0, fn)` returns the err pair
    /// immediately and NEVER calls `fn` (a counter the body bumps stays 0).
    #[tokio::test]
    async fn deadline_zero_never_runs_body() {
        let out = run(r#"
import * as resilience from "std/resilience"
let ran = [0]
let [v, err] = resilience.deadline(0, () => { ran[0] = ran[0] + 1; return 7 })
print(v)
print(err.code)
print(ran[0])
"#)
        .await;
        assert_eq!(out, "nil\ndeadline-exceeded\n0\n");
    }

    /// §5.2 nested shrink enforcement: an inner `deadline(120000)` inside an outer
    /// `deadline(60000)` has an effective remaining ≤ the outer's remaining (relative
    /// comparison — not absolute values, to avoid clock flakiness).
    #[tokio::test]
    async fn deadline_nested_inner_remaining_le_outer() {
        let out = run(r#"
import * as resilience from "std/resilience"
let outer = [0.0]
let inner = [0.0]
resilience.deadline(60000, () => {
    outer[0] = resilience.deadlineRemaining()
    resilience.deadline(120000, () => {
        inner[0] = resilience.deadlineRemaining()
        return nil
    })
    return nil
})
print(inner[0] <= outer[0])
"#)
        .await;
        assert_eq!(out, "true\n");
    }

    /// §5.2 restore-after-expiry: after a `deadline` that EXPIRED (body raced out),
    /// `deadlineRemaining()` at top level is nil — the locals are restored on the
    /// expiry branch just like on normal return / panic.
    #[tokio::test]
    async fn deadline_restores_after_expiry() {
        let out = run(r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
let [v, err] = resilience.deadline(40, async () => {
    await time.sleep(400)
    return "done"
})
print(err.code)
print(resilience.deadlineRemaining())   // restored to nil after the expired deadline
"#)
        .await;
        assert_eq!(out, "deadline-exceeded\nnil\n");
    }

    /// §5.4 bulkhead park-with-budget: limit=1, queue=1. Hold the one permit with a
    /// long task, then a second `run` under a 50ms deadline → the caller gets
    /// `deadline-exceeded` (NOT bulkhead-full — it was admitted to the queue) and
    /// `__waiting` returns to 0 after the parked caller bails on the budget.
    #[tokio::test]
    async fn bulkhead_park_respects_deadline_budget() {
        let out = run(r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
let b = resilience.bulkhead({limit: 1, queue: 4})
// Holder occupies the single permit for 1s.
let holder = b.run(async () => { await time.sleep(1000); return "held" })
await time.sleep(20)   // let the holder acquire the permit first
// Second caller parks (queue has room), then bails at its 50ms deadline.
let [v, err] = resilience.deadline(50, async () => {
    return await b.run(async () => { await time.sleep(1000); return "second" })
})
print(err.code)
print(b.__waiting)
"#)
        .await;
        assert_eq!(out, "deadline-exceeded\n0\n");
    }

    /// §5.4 limiter park-with-budget: a 1-token/sec limiter, drained, then an
    /// `acquire(1)` under a tight 30ms deadline → `deadline-exceeded` rather than the
    /// ~1000ms refill wait. (Acquire returns the err PAIR under a deadline; the
    /// no-deadline success path still returns nil — see other limiter tests.)
    #[tokio::test]
    async fn limiter_acquire_respects_deadline_budget() {
        let out = run(r#"
import * as resilience from "std/resilience"
let l = resilience.limiter({capacity: 1, refillPerSec: 1})
l.acquire()    // drain the single token (no deadline → returns nil, instant)
let [v, err] = resilience.deadline(30, async () => {
    let r = l.acquire()    // would need ~1000ms to refill — budget caps it
    return r
})
print(err.code)
"#)
        .await;
        assert_eq!(out, "deadline-exceeded\n");
    }

    /// A non-number `ms` is a Tier-2 panic.
    #[tokio::test]
    async fn deadline_non_number_ms_panics() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
resilience.deadline("nope", () => 1)
"#)
        .await;
        assert!(
            res.is_err() || res.unwrap().contains("ms"),
            "expected a panic about ms"
        );
    }

    // ── Task 4.3: resilience.withTrace / traceId (§5.5) ────────────────────────

    /// `traceId()` inside `withTrace` returns the set id; after the scope it is nil
    /// again (save → set → restore on the cell, like `deadline`).
    #[tokio::test]
    async fn with_trace_sets_and_restores() {
        let out = run(r#"
import * as resilience from "std/resilience"
resilience.withTrace("t-1", () => {
    print(resilience.traceId())
    return nil
})
print(resilience.traceId())   // restored: nil at top level
"#)
        .await;
        assert_eq!(out, "t-1\nnil\n");
    }

    /// The trace id is captured at the spawn site, so a child async fn spawned WHILE
    /// the trace is set inherits it (mirrors the deadline inheritance probe).
    #[tokio::test]
    async fn with_trace_inherited_by_spawned_async_fn() {
        let out = run(r#"
import * as resilience from "std/resilience"
async fn child() { return resilience.traceId() }
let [v, err] = resilience.withTrace("t-1", async () => {
    let f = child()              // spawned WHILE the trace local is set
    return await f
})
print(v)
print(resilience.traceId())   // restored: nil at top level
"#)
        .await;
        assert_eq!(out, "t-1\nnil\n");
    }

    /// `withTrace` returns a `[value, nil]` ok-pair on a plain-value body (matches
    /// `deadline`/`fallback`).
    #[tokio::test]
    async fn with_trace_wraps_plain_value_as_ok_pair() {
        let out = run(r#"
import * as resilience from "std/resilience"
let [v, err] = resilience.withTrace("t-1", () => 42)
print(v)
print(err)
"#)
        .await;
        assert_eq!(out, "42\nnil\n");
    }

    /// Nested `withTrace`: the inner sets a new id, restored to the outer's on exit.
    #[tokio::test]
    async fn with_trace_nesting_restores_outer() {
        let out = run(r#"
import * as resilience from "std/resilience"
resilience.withTrace("outer", () => {
    print(resilience.traceId())
    resilience.withTrace("inner", () => {
        print(resilience.traceId())
        return nil
    })
    print(resilience.traceId())   // restored to the outer id
    return nil
})
"#)
        .await;
        assert_eq!(out, "outer\ninner\nouter\n");
    }

    /// A panicking `withTrace` body still restores the previous trace id.
    #[tokio::test]
    async fn with_trace_restores_on_panic() {
        let out = run(r#"
import * as resilience from "std/resilience"
let [v, err] = recover(() => resilience.withTrace("t-1", () => { assert(false, "boom") }))
print(resilience.traceId())   // restored even though the body panicked
"#)
        .await;
        assert_eq!(out, "nil\n");
    }

    /// `withTrace` inside `deadline` (and the reverse) preserves the OTHER field via
    /// the COW copy: both the deadline budget AND the trace id are visible inside.
    #[tokio::test]
    async fn with_trace_inside_deadline_preserves_both() {
        let out = run(r#"
import * as resilience from "std/resilience"
resilience.deadline(60000, () => {
    resilience.withTrace("t", () => {
        print(resilience.deadlineRemaining() != nil)
        print(resilience.traceId())
        return nil
    })
    return nil
})
"#)
        .await;
        assert_eq!(out, "true\nt\n");
    }

    /// `deadline` inside `withTrace`: the trace id survives the deadline scope (the
    /// deadline COW carries the existing trace_id forward).
    #[tokio::test]
    async fn deadline_inside_with_trace_preserves_trace() {
        let out = run(r#"
import * as resilience from "std/resilience"
resilience.withTrace("t", () => {
    resilience.deadline(60000, () => {
        print(resilience.traceId())
        print(resilience.deadlineRemaining() != nil)
        return nil
    })
    return nil
})
"#)
        .await;
        assert_eq!(out, "t\ntrue\n");
    }

    /// `traceId()` is nil when no trace is set.
    #[tokio::test]
    async fn trace_id_nil_outside_with_trace() {
        let out = run(r#"
import * as resilience from "std/resilience"
print(resilience.traceId())
"#)
        .await;
        assert_eq!(out, "nil\n");
    }

    /// A non-string `id` is a Tier-2 panic.
    #[tokio::test]
    async fn with_trace_non_string_id_panics() {
        let res = crate::run_source(r#"
import * as resilience from "std/resilience"
resilience.withTrace(123, () => 1)
"#)
        .await;
        assert!(
            res.is_err() || res.unwrap().contains("must be a string"),
            "expected a panic about id being a string"
        );
    }

    // ── Task 5.1: §6.1 per-isolate metrics registry ─────────────────────────

    /// A breaker trip scenario fills `breaker_calls_total` (success/failure/
    /// rejected), sets `breaker_state` to 1.0 (open), and counts the closed→open
    /// transition — all in the always-on registry.
    #[tokio::test]
    async fn registry_breaker_outcomes_state_and_transition() {
        let (_out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({name: "t", failureRate: 0.5, window: 4, minCalls: 4, cooldownMs: 999999, halfOpenMax: 1})
fn ok() { return 42 }
fn fail() { return [nil, {message: "boom", code: "err"}] }
b.call(ok)    // success #1
b.call(ok)    // success #2
b.call(fail)  // failure #1
b.call(fail)  // failure #2 → opens (50% over 4 >= 0.5)
b.call(ok)    // rejected (open)
print(b.state())
"#).await.expect("run");
        let reg = &interp.resilience.borrow().registry;
        assert_eq!(reg.value("ascript_resilience_breaker_state", &[("name", "t")]), 1.0, "open gauge");
        assert_eq!(
            reg.value("ascript_resilience_breaker_calls_total", &[("name", "t"), ("result", "success")]),
            2.0, "2 successes"
        );
        assert_eq!(
            reg.value("ascript_resilience_breaker_calls_total", &[("name", "t"), ("result", "failure")]),
            2.0, "2 failures"
        );
        assert_eq!(
            reg.value("ascript_resilience_breaker_calls_total", &[("name", "t"), ("result", "rejected")]),
            1.0, "1 rejected"
        );
        assert_eq!(
            reg.value("ascript_resilience_breaker_transitions_total", &[("name", "t"), ("to", "open")]),
            1.0, "1 closed→open transition"
        );
    }

    /// limiter: a granted `tryAcquire` and a rejected one bump the §6.1 counters.
    #[tokio::test]
    async fn registry_limiter_acquired_and_rejected() {
        let (_out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
let l = resilience.limiter({name: "L", capacity: 1, refillPerSec: 0})
print(l.tryAcquire())   // true  → acquired
print(l.tryAcquire())   // false → rejected (bucket empty, no refill)
"#).await.expect("run");
        let reg = &interp.resilience.borrow().registry;
        assert_eq!(reg.value("ascript_resilience_limiter_acquired_total", &[("name", "L")]), 1.0);
        assert_eq!(reg.value("ascript_resilience_limiter_rejected_total", &[("name", "L")]), 1.0);
    }

    /// keyedLimiter: an eviction bumps `limiter_evictions_total` (maxKeys=1, two
    /// distinct keys → the first bucket is evicted).
    #[tokio::test]
    async fn registry_keyed_limiter_evictions() {
        let (_out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
let k = resilience.keyedLimiter({name: "K", capacity: 5, refillPerSec: 0, maxKeys: 1})
k.tryAcquire("a")   // creates bucket a → acquired
k.tryAcquire("b")   // creates bucket b → evicts a → acquired
"#).await.expect("run");
        let reg = &interp.resilience.borrow().registry;
        assert_eq!(reg.value("ascript_resilience_limiter_acquired_total", &[("name", "K")]), 2.0);
        assert_eq!(reg.value("ascript_resilience_limiter_evictions_total", &[("name", "K")]), 1.0);
    }

    /// bulkhead: an immediate shed (limit=0-capacity reached, queue=0) bumps
    /// `bulkhead_shed_total`; in-flight gauge returns to 0 after the run.
    #[tokio::test]
    async fn registry_bulkhead_shed_and_in_flight() {
        // limit=1, queue=0: a synchronous run completes + releases, so in_flight
        // returns to 0; a SECOND concurrent run would shed, but to keep this test
        // deterministic we assert the in_flight gauge after a completed run.
        let (_out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
let bh = resilience.bulkhead({name: "B", limit: 1, queue: 0})
fn work() { return 1 }
let [v, e] = bh.run(work)
"#).await.expect("run");
        let reg = &interp.resilience.borrow().registry;
        // The permit was acquired (in_flight→1) then released (in_flight→0).
        assert_eq!(reg.value("ascript_resilience_bulkhead_in_flight", &[("name", "B")]), 0.0);
    }

    /// retry: a resilience.retry policy bumps `retry_attempts_total{outcome}`
    /// (one failure then a success).
    #[tokio::test]
    async fn registry_retry_attempts() {
        let (_out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
let r = resilience.retry({name: "R", attempts: 3, baseMs: 0, retryOn: "error"})
let tries = [0]
fn flaky() {
  tries[0] = tries[0] + 1
  if (tries[0] < 2) { return [nil, {message: "x"}] }
  return 99
}
let v = r.call(flaky)
print(v)
"#).await.expect("run");
        let reg = &interp.resilience.borrow().registry;
        assert_eq!(
            reg.value("ascript_resilience_retry_attempts_total", &[("name", "R"), ("outcome", "failure")]),
            1.0, "1 failed attempt"
        );
        assert_eq!(
            reg.value("ascript_resilience_retry_attempts_total", &[("name", "R"), ("outcome", "success")]),
            1.0, "1 successful attempt"
        );
    }

    /// memoize: one miss (first get) then one hit (second get) bump the §6.1
    /// cross-cache aggregate counters.
    #[tokio::test]
    async fn registry_memoize_hits_and_misses() {
        let (_out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
let c = resilience.memoize({name: "M", max: 10})
async fn f() { return 1 }
await c.get("k", f)   // miss
await c.get("k", f)   // hit
"#).await.expect("run");
        let reg = &interp.resilience.borrow().registry;
        assert_eq!(reg.value("ascript_resilience_memoize_misses_total", &[("name", "M")]), 1.0);
        assert_eq!(reg.value("ascript_resilience_memoize_hits_total", &[("name", "M")]), 1.0);
    }

    /// singleflight: N concurrent same-key calls collapse to one execution; the
    /// joiners bump `singleflight_joins_total` (no name label).
    #[tokio::test]
    async fn registry_singleflight_joins() {
        let (_out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
async fn slow() { return 7 }
let f1 = resilience.singleflight("k", slow)   // starts the flight
let f2 = resilience.singleflight("k", slow)   // join #1
let f3 = resilience.singleflight("k", slow)   // join #2
await f1
await f2
await f3
"#).await.expect("run");
        let reg = &interp.resilience.borrow().registry;
        assert_eq!(reg.value("ascript_resilience_singleflight_joins_total", &[]), 2.0);
    }

    /// deadline: an already-expired (0ms) deadline bumps `deadline_exceeded_total`
    /// (no name label).
    #[tokio::test]
    async fn registry_deadline_exceeded() {
        let (_out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
fn body() { return 1 }
let [v, e] = resilience.deadline(0, body)   // already-expired entry → exceeded
"#).await.expect("run");
        let reg = &interp.resilience.borrow().registry;
        assert_eq!(reg.value("ascript_resilience_deadline_exceeded_total", &[]), 1.0);
    }

    /// cfg-matrix proof: WITHOUT telemetry/log features the registry still works
    /// (this test runs in every config that includes `resilience`).
    #[tokio::test]
    async fn registry_works_without_telemetry_init() {
        // No telemetry.init anywhere — the soft-hook mirror no-ops, the registry
        // still records the breaker outcome.
        let (_out, interp) = crate::run_source_with_interp(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({name: "z"})
fn ok() { return 1 }
b.call(ok)
"#).await.expect("run");
        let reg = &interp.resilience.borrow().registry;
        assert_eq!(
            reg.value("ascript_resilience_breaker_calls_total", &[("name", "z"), ("result", "success")]),
            1.0
        );
    }

    // ── Task 5.2: §6.2 Prometheus text render ────────────────────────────────

    /// `render_prometheus` over a hand-built registry: `# TYPE` once per metric
    /// (gauge + counter), a labeled series, an unlabeled series (no `{}`), and a
    /// label VALUE that exercises all three escape rules (`\`, `"`, newline).
    #[test]
    fn render_prometheus_format_and_escaping() {
        let mut reg = super::ResilRegistry::default();
        // gauge with a single label
        reg.set_gauge("ascript_resilience_breaker_state", &[("name", "t")], 1.0);
        // counter, two series under ONE metric name (must share one # TYPE line)
        reg.incr(
            "ascript_resilience_breaker_calls_total",
            &[("name", "t"), ("result", "success")],
            2.0,
        );
        reg.incr(
            "ascript_resilience_breaker_calls_total",
            &[("name", "t"), ("result", "failure")],
            1.0,
        );
        // counter with NO labels → no `{}`
        reg.incr("ascript_resilience_singleflight_joins_total", &[], 3.0);
        // counter whose label value needs escaping: \ , " , newline
        reg.incr(
            "ascript_resilience_breaker_calls_total",
            &[("name", "a\"b\\c\nd"), ("result", "success")],
            1.0,
        );

        let text = super::render_prometheus(&reg);

        // # TYPE lines, one per metric name, correct kind.
        assert!(
            text.contains("# TYPE ascript_resilience_breaker_state gauge\n"),
            "gauge TYPE line:\n{text}"
        );
        assert!(
            text.contains("# TYPE ascript_resilience_breaker_calls_total counter\n"),
            "counter TYPE line:\n{text}"
        );
        assert!(
            text.contains("# TYPE ascript_resilience_singleflight_joins_total counter\n"),
            "joins TYPE line:\n{text}"
        );

        // The gauge series line (integral value prints with no trailing .0).
        assert!(
            text.contains("ascript_resilience_breaker_state{name=\"t\"} 1\n"),
            "gauge series:\n{text}"
        );
        // Labeled counter series.
        assert!(
            text.contains(
                "ascript_resilience_breaker_calls_total{name=\"t\",result=\"success\"} 2\n"
            ),
            "labeled counter series:\n{text}"
        );
        // Unlabeled series → no `{}`.
        assert!(
            text.contains("ascript_resilience_singleflight_joins_total 3\n"),
            "unlabeled series:\n{text}"
        );
        // Escaping: \ → \\, " → \", newline → \n.
        assert!(
            text.contains(
                "ascript_resilience_breaker_calls_total{name=\"a\\\"b\\\\c\\nd\",result=\"success\"} 1\n"
            ),
            "escaped label value:\n{text}"
        );

        // # TYPE for the multi-series counter appears EXACTLY once.
        let type_count = text
            .matches("# TYPE ascript_resilience_breaker_calls_total ")
            .count();
        assert_eq!(type_count, 1, "no duplicate # TYPE for multi-series metric:\n{text}");
    }

    /// `resilience.metricsHandler()` returns a callable `NativeMethod` whose call
    /// (with a request arg) yields the `{status:200, headers:{content-type}, body}`
    /// HTTP-response shape the server understands.
    #[tokio::test]
    async fn metrics_handler_returns_http_response_shape() {
        let out = run(r##"
import * as resilience from "std/resilience"
import * as string from "std/string"
let b = resilience.breaker({name: "t", failureRate: 0.5, window: 4, minCalls: 4, cooldownMs: 999999, halfOpenMax: 1})
fn ok() { return 1 }
b.call(ok)
let h = resilience.metricsHandler()
let resp = h({method: "GET", path: "/metrics"})
let body = resp.body
print(resp.status)
print(resp.headers["content-type"])
print(string.contains(body, "# TYPE ascript_resilience_breaker_calls_total counter"))
print(string.contains(body, "ascript_resilience_breaker_calls_total{name=\"t\",result=\"success\"} 1"))
"##)
        .await;
        assert_eq!(out, "200\ntext/plain; version=0.0.4\ntrue\ntrue\n");
    }

    // ── §6.3 resilience.health ────────────────────────────────────────────────

    /// Liveness: empty `{}` → 200, `{"status":"ok","checks":{}}`.
    #[tokio::test]
    async fn health_empty_is_liveness_200() {
        let out = run(r##"
import * as resilience from "std/resilience"
import * as json from "std/json"
import * as object from "std/object"
let h = resilience.health({})
let resp = h({method: "GET", path: "/healthz"})
print(resp.status)
print(resp.headers["content-type"])
let [d, e] = json.parse(resp.body)
print(d.status)
print(len(object.keys(d.checks)))
"##)
        .await;
        assert_eq!(out, "200\napplication/json\nok\n0\n");
    }

    /// All checks pass (truthy / ok-pair) → 200 degraded=false, per-check detail.
    #[tokio::test]
    async fn health_all_pass_200() {
        let out = run(r##"
import * as resilience from "std/resilience"
import * as json from "std/json"
fn pingDb() { return true }
fn pingCache() { return [1, nil] }
let h = resilience.health({ checks: { db: pingDb, cache: pingCache } })
let resp = h({})
print(resp.status)
let [d, e] = json.parse(resp.body)
print(d.status)
print(d.checks.db.ok)
print(d.checks.cache.ok)
"##)
        .await;
        assert_eq!(out, "200\nok\ntrue\ntrue\n");
    }

    /// One failing check (falsy) → 503 degraded; others still reported (containment).
    #[tokio::test]
    async fn health_one_failing_503_others_reported() {
        let out = run(r##"
import * as resilience from "std/resilience"
import * as json from "std/json"
fn good() { return true }
fn bad() { return false }
let h = resilience.health({ checks: { a: good, b: bad, c: good } })
let resp = h({})
print(resp.status)
let [d, e] = json.parse(resp.body)
print(d.status)
print(d.checks.a.ok)
print(d.checks.b.ok)
print(d.checks.c.ok)
"##)
        .await;
        assert_eq!(out, "503\ndegraded\ntrue\nfalse\ntrue\n");
    }

    /// A check that PANICS is contained (recover-style): reported ok:false, never 500.
    #[tokio::test]
    async fn health_panicking_check_contained_503() {
        let out = run(r##"
import * as resilience from "std/resilience"
import * as json from "std/json"
fn good() { return true }
fn boom() { let x = nil; return x.field }
let h = resilience.health({ checks: { a: good, b: boom } })
let resp = h({})
print(resp.status)
let [d, e] = json.parse(resp.body)
print(d.status)
print(d.checks.a.ok)
print(d.checks.b.ok)
"##)
        .await;
        assert_eq!(out, "503\ndegraded\ntrue\nfalse\n");
    }

    /// An err-pair return `[nil, e]` is a failing check.
    #[tokio::test]
    async fn health_err_pair_check_fails_503() {
        let out = run(r##"
import * as resilience from "std/resilience"
import * as json from "std/json"
fn bad() { return [nil, {message: "db down", code: "down"}] }
let h = resilience.health({ checks: { db: bad } })
let resp = h({})
print(resp.status)
let [d, e] = json.parse(resp.body)
print(d.status)
print(d.checks.db.ok)
"##)
        .await;
        assert_eq!(out, "503\ndegraded\nfalse\n");
    }

    // ── §6.4 resilience.handler ───────────────────────────────────────────────

    /// Empty `{}` wrapper is the identity: passes `fn`'s result straight through.
    #[tokio::test]
    async fn handler_empty_is_identity() {
        let out = run(r##"
import * as resilience from "std/resilience"
fn handle(req) { return { status: 201, body: "hi" } }
let h = resilience.handler({}, handle)
let resp = h({method: "GET", path: "/x"})
print(resp.status)
print(resp.body)
"##)
        .await;
        assert_eq!(out, "201\nhi\n");
    }

    /// A plain handler error-pair passes through unchanged (server maps it to 500).
    #[tokio::test]
    async fn handler_fn_error_pair_passes_through() {
        let out = run(r##"
import * as resilience from "std/resilience"
fn handle(req) { return [nil, {message: "kaboom", code: "boom"}] }
let h = resilience.handler({}, handle)
let resp = h({})
let [v, e] = resp
print(v)
print(e.code)
"##)
        .await;
        assert_eq!(out, "nil\nboom\n");
    }

    /// A drained limiter → 429 with a `retry-after` header.
    #[tokio::test]
    async fn handler_rate_limited_429() {
        let out = run(r##"
import * as resilience from "std/resilience"
let lim = resilience.limiter({capacity: 1, refillPerSec: 1})
fn handle(req) { return { status: 200, body: "ok" } }
let h = resilience.handler({ limiter: lim }, handle)
let r1 = h({})
print(r1.status)
let r2 = h({})
print(r2.status)
print(r2.headers["retry-after"] != nil)
"##)
        .await;
        assert_eq!(out, "200\n429\ntrue\n");
    }

    /// A keyed limiter with a `key` extractor that receives the request object.
    #[tokio::test]
    async fn handler_keyed_limiter_key_receives_request() {
        let out = run(r##"
import * as resilience from "std/resilience"
let lim = resilience.keyedLimiter({capacity: 1, refillPerSec: 1, maxKeys: 16})
fn handle(req) { return { status: 200, body: "ok" } }
let h = resilience.handler({ limiter: lim, key: (req) => req.headers["x-api-key"] }, handle)
let r1 = h({headers: {"x-api-key": "alice"}})
print(r1.status)
let r2 = h({headers: {"x-api-key": "bob"}})
print(r2.status)
let r3 = h({headers: {"x-api-key": "alice"}})
print(r3.status)
"##)
        .await;
        assert_eq!(out, "200\n200\n429\n");
    }

    /// Breaker open → 503.
    #[tokio::test]
    async fn handler_breaker_open_503() {
        let out = run(r##"
import * as resilience from "std/resilience"
let b = resilience.breaker({name: "bq", failureRate: 0.5, window: 2, minCalls: 2, cooldownMs: 999999, halfOpenMax: 1})
fn fail() { return [nil, {message: "x", code: "e"}] }
b.call(fail)
b.call(fail)
fn handle(req) { return { status: 200, body: "ok" } }
let h = resilience.handler({ breaker: b }, handle)
let resp = h({})
print(resp.status)
"##)
        .await;
        assert_eq!(out, "503\n");
    }

    /// Deadline expiry → 504. A zero budget is already-expired on entry, so the
    /// handler `fn` never runs — exercises the code→status map deterministically and
    /// SIDESTEPS the pre-existing module-method-from-native async-driver bug (a
    /// module-qualified call inside an async closure invoked by `deadline`/`bulkhead`
    /// from native code is "value is not callable" on BOTH engines — out of scope).
    /// The end-to-end async timeout path is covered by `tests/resil_handler_server.rs`.
    #[tokio::test]
    async fn handler_deadline_exceeded_504() {
        let out = run(r##"
import * as resilience from "std/resilience"
fn handle(req) { return { status: 200, body: "ok" } }
let h = resilience.handler({ deadlineMs: 0 }, handle)
let resp = h({})
print(resp.status)
"##)
        .await;
        assert_eq!(out, "504\n");
    }

    /// A successful `fn` value through the full stack returns unchanged.
    #[tokio::test]
    async fn handler_success_through_full_stack() {
        let out = run(r##"
import * as resilience from "std/resilience"
let lim = resilience.limiter({capacity: 5, refillPerSec: 1})
let bh = resilience.bulkhead({name: "bh2", limit: 4, queue: 4})
let b = resilience.breaker({name: "b2", failureRate: 0.5, window: 4, minCalls: 4})
fn handle(req) { return { status: 200, body: req.path } }
let h = resilience.handler({ limiter: lim, bulkhead: bh, breaker: b, deadlineMs: 1000 }, handle)
let resp = await h({path: "/quote"})
print(resp.status)
print(resp.body)
"##)
        .await;
        assert_eq!(out, "200\n/quote\n");
    }
}
