//! `std/telemetry` — a thin, vendor-neutral observability facade: tracing spans,
//! metrics (counter/histogram/gauge), and analytics events (`capture`/`identify`),
//! delivered through three HAND-ROLLED HTTP exporters (OTLP http/json, Sentry
//! envelopes, PostHog capture) over the pooled `reqwest::Client` in `net_http.rs`.
//!
//! Telemetry is **opt-in at runtime** — a no-op until `telemetry.init(...)` runs —
//! and **opt-in at build** (the `telemetry` Cargo feature, not in `default`). State
//! is an `Interp`-stateful singleton (mirroring `std/log`): the configured pipeline
//! lives behind interior-mutability cells and emits to the network (live) or to a
//! capture buffer (tests, `Interp::telemetry_capture()`).
//!
//! ## Routing
//! `exports()` returns the builtin bindings; the qualified calls (`telemetry.init`,
//! `telemetry.startSpan`, …) route through `Interp::call_telemetry` in `interp.rs`.
//! Span/instrument handles are `Value::Native` whose methods dispatch back into
//! `call_telemetry`-adjacent handlers on `Interp`.
//!
//! ## No new `Value` variant
//! Exporter descriptors are tagged Objects (`{__exporter: "otlp", ...}`), same
//! discipline as `std/schema`'s tagged objects — `init` reads them to build the
//! live `TelemetryState`.

pub mod model;
pub mod otlp;
pub mod posthog;
pub mod sentry;

use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::{NativeKind, NativeObject, Value};
use indexmap::IndexMap;
use model::{
    AnalyticsEvent, Exporters, MetricKind, OtlpExporter, PostHogExporter, SentryExporter, SpanCtx,
    SpanStatusCode, TelemetryState,
};
use std::rc::Rc;

/// The builtin bindings an `import * as telemetry from "std/telemetry"` brings in.
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("init", super::bi("telemetry.init")),
        ("otlp", super::bi("telemetry.otlp")),
        ("sentry", super::bi("telemetry.sentry")),
        ("posthog", super::bi("telemetry.posthog")),
        ("flush", super::bi("telemetry.flush")),
        ("shutdown", super::bi("telemetry.shutdown")),
        ("startSpan", super::bi("telemetry.startSpan")),
        ("span", super::bi("telemetry.span")),
        ("counter", super::bi("telemetry.counter")),
        ("histogram", super::bi("telemetry.histogram")),
        ("gauge", super::bi("telemetry.gauge")),
        ("capture", super::bi("telemetry.capture")),
        ("identify", super::bi("telemetry.identify")),
    ]
}

/// Is `func` a known `std/telemetry` builtin name? Used by `dispatch` to reject
/// typos with a clear message rather than silently no-op'ing.
pub fn is_known_func(func: &str) -> bool {
    matches!(
        func,
        "init"
            | "otlp"
            | "sentry"
            | "posthog"
            | "flush"
            | "shutdown"
            | "startSpan"
            | "span"
            | "counter"
            | "histogram"
            | "gauge"
            | "capture"
            | "identify"
    )
}

thread_local! {
    /// Test seam: when set, the capture-mode send seam returns an error so tests
    /// can exercise the error model (flush failure → logged once + dropped, never
    /// a program abort). Per-thread, so concurrent `#[tokio::test]`s (each on its
    /// own current-thread runtime) don't interfere. Always `false` outside tests.
    static FORCE_SEND_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Set the per-thread force-send-error test seam (see [`FORCE_SEND_ERROR`]).
pub fn set_test_force_send_error(on: bool) {
    FORCE_SEND_ERROR.with(|c| c.set(on));
}

/// Read the force-send-error test seam.
pub(crate) fn test_force_send_error() -> bool {
    FORCE_SEND_ERROR.with(|c| c.get())
}

/// A Tier-1 success acknowledgement `[true, nil]` (from `init`/`flush`).
fn tier1_ok() -> Value {
    crate::interp::make_pair(Value::Bool(true), Value::Nil)
}

/// A Tier-1 failure `[nil, {message}]` (from `init`/`flush`).
fn tier1_err(msg: &str) -> Value {
    crate::interp::make_pair(
        Value::Nil,
        crate::interp::make_error(Value::Str(msg.into())),
    )
}

/// An inert `Value::Native` handle returned when telemetry is not initialized:
/// every method on it is a no-op (`call_telemetry_noop_method`). Lets `.as` code
/// call `telemetry.startSpan(...).end()` / `telemetry.counter(...).add(1)`
/// unconditionally — the "safe to leave in production" promise.
fn noop_handle() -> Value {
    Value::Native(Rc::new(NativeObject {
        id: u64::MAX,
        kind: NativeKind::TelemetryNoop,
        fields: IndexMap::new(),
    }))
}

/// Build an instrument handle pointing at resource `id`.
fn instrument_handle(id: u64) -> Value {
    Value::Native(Rc::new(NativeObject {
        id,
        kind: NativeKind::TelemetryInstrument,
        fields: IndexMap::new(),
    }))
}

// ---- argument helpers (Tier-2 panic on type misuse, spec §5) ----

fn want_obj_or_nil<'a>(v: &'a Value, span: Span, ctx: &str) -> Result<Option<&'a Value>, Control> {
    match v {
        Value::Nil => Ok(None),
        Value::Object(_) => Ok(Some(v)),
        other => Err(AsError::at(
            format!(
                "{} expects an options object, got {}",
                ctx,
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

/// Read a string field from an object (None if absent or non-string-nil).
fn obj_str(obj: &Value, key: &str) -> Option<String> {
    if let Value::Object(o) = obj {
        if let Some(Value::Str(s)) = o.borrow().get(key) {
            return Some(s.to_string());
        }
    }
    None
}

/// Read a bool field from an object (default false).
fn obj_bool(obj: &Value, key: &str) -> bool {
    if let Value::Object(o) = obj {
        matches!(o.borrow().get(key), Some(Value::Bool(true)))
    } else {
        false
    }
}

/// Read the `attributes`/`properties`/`headers` sub-object as ordered pairs.
fn obj_pairs(obj: &Value, key: &str) -> Vec<(String, Value)> {
    if let Value::Object(o) = obj {
        if let Some(Value::Object(inner)) = o.borrow().get(key) {
            return inner
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
        }
    }
    Vec::new()
}

/// Read string headers from an object's `headers` sub-object (string values only).
fn obj_headers(obj: &Value, key: &str) -> Vec<(String, String)> {
    obj_pairs(obj, key)
        .into_iter()
        .map(|(k, v)| match v {
            Value::Str(s) => (k, s.to_string()),
            other => (k, other.to_string()),
        })
        .collect()
}

/// The `std/telemetry` dispatch (called from `Interp::call_telemetry`). Routes
/// each builtin; uninitialized → inert handles / no-ops (never an error).
pub async fn dispatch(
    interp: &Interp,
    func: &str,
    args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    match func {
        "otlp" => build_otlp_descriptor(args, span),
        "sentry" => build_sentry_descriptor(args, span),
        "posthog" => build_posthog_descriptor(args, span),
        "init" => init(interp, args, span).await,
        "shutdown" => shutdown(interp).await,
        "flush" => flush(interp).await,
        "startSpan" => start_span(interp, args, span),
        "span" => scoped_span(interp, args, span).await,
        "counter" => instrument(interp, MetricKind::Counter, args, span),
        "histogram" => instrument(interp, MetricKind::Histogram, args, span),
        "gauge" => instrument(interp, MetricKind::Gauge, args, span),
        "capture" => capture(interp, args, span),
        "identify" => identify(interp, args, span),
        other => Err(AsError::at(
            format!("std/telemetry has no function '{}'", other),
            span,
        )
        .into()),
    }
}

// ---- exporter descriptors (tagged Objects, like std/schema) ----

/// Build a tagged exporter descriptor Object `{__exporter: kind, ...opts}` from
/// the (optional) options object. `init` reads `__exporter` to dispatch the
/// per-exporter parse. Shared by otlp/sentry/posthog (same discipline as
/// `std/schema`'s tagged objects — no new `Value` variant).
fn tagged_descriptor(kind: &str, args: &[Value], span: Span, ctx: &str) -> Result<Value, Control> {
    let opts = super::arg(args, 0);
    let opts = want_obj_or_nil(&opts, span, ctx)?;
    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert("__exporter".into(), Value::Str(kind.into()));
    if let Some(Value::Object(src)) = opts {
        for (k, v) in src.borrow().iter() {
            m.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(crate::value::ObjectCell::new(m)))
}

fn build_otlp_descriptor(args: &[Value], span: Span) -> Result<Value, Control> {
    tagged_descriptor("otlp", args, span, "telemetry.otlp")
}

fn build_sentry_descriptor(args: &[Value], span: Span) -> Result<Value, Control> {
    tagged_descriptor("sentry", args, span, "telemetry.sentry")
}

fn build_posthog_descriptor(args: &[Value], span: Span) -> Result<Value, Control> {
    tagged_descriptor("posthog", args, span, "telemetry.posthog")
}

// ---- init / shutdown / flush ----

/// Parse the exporter descriptors + service config into a `TelemetryState`. A
/// missing/unparseable required exporter config is a Tier-1 `[nil, err]`; a wrong
/// argument *type* (init not an object, unknown exporter kind, unsupported OTLP
/// protocol) is a Tier-2 panic.
async fn init(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let cfg = super::arg(args, 0);
    let cfg_obj = match &cfg {
        Value::Object(_) => cfg.clone(),
        other => {
            return Err(AsError::at(
                format!(
                    "telemetry.init expects a config object, got {}",
                    crate::interp::type_name(other)
                ),
                span,
            )
            .into())
        }
    };
    let service = match obj_str(&cfg_obj, "service") {
        Some(s) => s,
        None => {
            return Ok(tier1_err(
                "telemetry.init: 'service' (service.name) is required",
            ))
        }
    };
    let version = obj_str(&cfg_obj, "version");
    let env = obj_str(&cfg_obj, "env");
    let resource = obj_pairs(&cfg_obj, "resource");
    let mirror_events_to_otlp = obj_bool(&cfg_obj, "mirrorEventsToOtlp");

    // Parse the exporters array.
    let mut exporters = Exporters::default();
    if let Value::Object(o) = &cfg_obj {
        let exporters_val = o.borrow().get("exporters").cloned();
        if let Some(Value::Array(a)) = exporters_val {
            for d in a.borrow().iter() {
                match parse_exporter(d, span)? {
                    Ok(ParsedExporter::Otlp(e)) => exporters.otlp = Some(e),
                    Ok(ParsedExporter::Sentry(e)) => exporters.sentry = Some(e),
                    Ok(ParsedExporter::PostHog(e)) => exporters.posthog = Some(e),
                    Err(msg) => return Ok(tier1_err(&msg)),
                }
            }
        }
    }

    let state = TelemetryState {
        service,
        version,
        env,
        resource,
        exporters,
        mirror_events_to_otlp,
        spans: Vec::new(),
        instruments: IndexMap::new(),
        events: Vec::new(),
    };
    // Re-init REPLACES the pipeline, flushing the previous one first (spec §2) so
    // its buffered signals are not lost. A flush failure here is swallowed (the
    // old pipeline is being discarded regardless).
    if interp.telemetry_active() {
        let _ = flush(interp).await;
    }
    interp.telemetry_install(state);
    Ok(tier1_ok())
}

/// One successfully-parsed exporter, or a Tier-1 error message (missing config).
enum ParsedExporter {
    Otlp(OtlpExporter),
    Sentry(SentryExporter),
    PostHog(PostHogExporter),
}

/// Parse one exporter descriptor. Outer `Err` = Tier-2 (wrong kind/protocol);
/// inner `Err(String)` = Tier-1 (missing/unparseable required config).
fn parse_exporter(d: &Value, span: Span) -> Result<Result<ParsedExporter, String>, Control> {
    let kind = obj_str(d, "__exporter");
    match kind.as_deref() {
        Some("otlp") => Ok(otlp::parse_descriptor(d, span)?.map(ParsedExporter::Otlp)),
        Some("sentry") => Ok(sentry::parse_descriptor(d).map(ParsedExporter::Sentry)),
        Some("posthog") => Ok(posthog::parse_descriptor(d).map(ParsedExporter::PostHog)),
        _ => Err(AsError::at(
            "telemetry.init: each exporter must be telemetry.otlp(...)/sentry(...)/posthog(...)",
            span,
        )
        .into()),
    }
}

/// `telemetry.shutdown()` — flush, then tear the pipeline down to no-op.
async fn shutdown(interp: &Interp) -> Result<Value, Control> {
    let result = flush(interp).await;
    let _ = interp.telemetry_take_state();
    result
}

/// `telemetry.flush()` — force-export all buffered signals. A flush failure is
/// logged once to stderr and dropped (best-effort); it never aborts the program.
async fn flush(interp: &Interp) -> Result<Value, Control> {
    // Take the pipeline out across the awaits (no RefCell borrow held).
    let mut state = match interp.telemetry_take_state() {
        Some(s) => s,
        None => return Ok(tier1_ok()),
    };
    let outcome = flush_state(interp, &mut state).await;
    // Clear the buffered signals (cumulative metrics keep their accumulators).
    state.spans.clear();
    state.events.clear();
    interp.telemetry_return_state(state);
    // Surface as a Tier-1 `[ok, err]` — never a panic (spec §5). A failed flush is
    // observability noise the program may inspect, not a fatal error.
    match outcome {
        Ok(()) => Ok(tier1_ok()),
        Err(msg) => Ok(tier1_err(&msg)),
    }
}

/// Export every buffered signal through each configured exporter. Returns the
/// FIRST exporter failure (already non-fatal — the caller turns it into a Tier-1
/// pair or logs+drops it on the exit path). Used by `flush`, re-`init`, and
/// flush-on-exit.
pub(crate) async fn flush_state_public(
    interp: &Interp,
    state: &mut TelemetryState,
) -> Result<(), String> {
    flush_state(interp, state).await
}

async fn flush_state(interp: &Interp, state: &mut TelemetryState) -> Result<(), String> {
    let mut first_err: Option<String> = None;
    // OTLP: spans + metrics + (mirrored) event logs.
    if state.exporters.otlp.is_some() {
        if let Err(e) = otlp::flush(interp, state).await {
            first_err.get_or_insert(e);
        }
    }
    if state.exporters.sentry.is_some() {
        if let Err(e) = sentry::flush(interp, state).await {
            first_err.get_or_insert(e);
        }
    }
    if state.exporters.posthog.is_some() {
        if let Err(e) = posthog::flush(interp, state).await {
            first_err.get_or_insert(e);
        }
    }
    match first_err {
        None => Ok(()),
        Some(msg) => Err(msg),
    }
}

// ---- spans ----

fn start_span(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let name = super::want_string(&super::arg(args, 0), span, "telemetry.startSpan")?;
    let opts = super::arg(args, 1);
    want_obj_or_nil(&opts, span, "telemetry.startSpan")?;
    let attrs = obj_pairs(&opts, "attributes");
    // Route through the SP11 soft hook so script-`startSpan` and SP11 GenAI spans
    // share ONE funnel: `None` (telemetry off) → an inert handle.
    match interp.telemetry_span_start(&name, attrs) {
        Some(id) => Ok(span_handle(id)),
        None => Ok(noop_handle()),
    }
}

/// Build a span handle pointing at resource `id`.
fn span_handle(id: u64) -> Value {
    Value::Native(Rc::new(NativeObject {
        id,
        kind: NativeKind::TelemetrySpan,
        fields: IndexMap::new(),
    }))
}

/// Scoped helper: starts a span, pushes it current, drives the callback (sync or
/// `async fn`, awaited), auto-ends, sets `error` status + records the message if
/// the callback panics (caught like `recover`), and returns `[value, err]`.
async fn scoped_span(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let name = super::want_string(&super::arg(args, 0), span, "telemetry.span")?;
    let cb = super::arg(args, 1);
    if !interp.telemetry_active() {
        // No telemetry: still run the callback, return its [value, err] pair, but
        // record nothing.
        return run_scoped_cb(interp, cb, span).await;
    }
    let id = interp.telemetry_open_span(&name, Vec::new());
    let (trace_id, span_id) = interp
        .with_resource(id, |r| match r {
            Some(crate::interp::ResourceState::TelemetrySpan(s)) => Some((s.trace_id, s.span_id)),
            _ => None,
        })
        .expect("freshly opened span is present");
    // Set THIS task's current span around the callback (save → set → restore), so
    // a span created inside the callback (or in the spawned async-fn body, which
    // captures this task's current at spawn time) parents to it. Per-task
    // task-local isolation means concurrent scoped spans never cross-parent.
    let prev = interp.telemetry_set_current(Some(SpanCtx {
        resource_id: id,
        trace_id,
        span_id,
    }));
    let result = run_scoped_cb(interp, cb, span).await;
    interp.telemetry_set_current(prev);
    // Determine status from the callback outcome.
    match &result {
        Ok(pair) => {
            let errd = matches!(pair, Value::Array(a) if a.borrow().len() == 2 && a.borrow()[1] != Value::Nil);
            if errd {
                if let Value::Array(a) = pair {
                    let msg = pair_err_message(&a.borrow()[1]);
                    interp.telemetry_span_set_status(id, SpanStatusCode::Error, msg);
                }
                interp.telemetry_finish_span(id, crate::interp::SpanStatus::Error);
            } else {
                interp.telemetry_finish_span(id, crate::interp::SpanStatus::Ok);
            }
        }
        Err(_) => {
            interp.telemetry_finish_span(id, crate::interp::SpanStatus::Error);
        }
    }
    result
}

/// Run the scoped callback, catching a Tier-2 panic like `recover` and returning
/// `[value, err]`. A `?`-propagation or `exit` flows through unchanged.
async fn run_scoped_cb(interp: &Interp, cb: Value, span: Span) -> Result<Value, Control> {
    let res = interp.call_value(cb, vec![], span).await;
    match res {
        Ok(v) => {
            // Drive an `async fn` callback's Future to completion.
            let v = match v {
                Value::Future(f) => match f.get().await {
                    Ok(x) => x,
                    Err(Control::Panic(e)) => {
                        return Ok(crate::interp::make_pair(
                            Value::Nil,
                            crate::interp::make_error(Value::Str(e.message.into())),
                        ))
                    }
                    Err(other) => return Err(other),
                },
                other => other,
            };
            Ok(crate::interp::make_pair(v, Value::Nil))
        }
        Err(Control::Panic(e)) => Ok(crate::interp::make_pair(
            Value::Nil,
            crate::interp::make_error(Value::Str(e.message.into())),
        )),
        Err(other) => Err(other),
    }
}

/// Pull a human message out of an err value (`{message: ...}` or any value).
fn pair_err_message(err: &Value) -> Option<String> {
    match err {
        Value::Object(o) => match o.borrow().get("message") {
            Some(Value::Str(s)) => Some(s.to_string()),
            Some(other) => Some(other.to_string()),
            None => Some(err.to_string()),
        },
        other => Some(other.to_string()),
    }
}

// ---- metrics ----

/// Register (idempotently) a metric instrument and return its handle. Inert when
/// telemetry is off.
fn instrument(
    interp: &Interp,
    kind: MetricKind,
    args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    if !interp.telemetry_active() {
        return Ok(noop_handle());
    }
    let name = super::want_string(&super::arg(args, 0), span, "telemetry metric")?;
    let opts = super::arg(args, 1);
    want_obj_or_nil(&opts, span, "telemetry metric")?;
    let unit = obj_str(&opts, "unit");
    let description = obj_str(&opts, "description");
    let id = interp.telemetry_register_instrument(&name, kind, unit, description);
    Ok(instrument_handle(id))
}

// ---- analytics events ----

fn capture(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    if !interp.telemetry_active() {
        return Ok(Value::Nil);
    }
    let event = super::want_string(&super::arg(args, 0), span, "telemetry.capture")?;
    let opts = super::arg(args, 1);
    want_obj_or_nil(&opts, span, "telemetry.capture")?;
    let distinct_id = obj_str(&opts, "distinctId").unwrap_or_default();
    let properties = obj_pairs(&opts, "properties");
    interp.telemetry_enqueue_event(AnalyticsEvent {
        event: event.to_string(),
        distinct_id,
        properties,
        set_props: Vec::new(),
        time_unix_nano: model::now_unix_nanos(),
    });
    Ok(Value::Nil)
}

fn identify(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    if !interp.telemetry_active() {
        return Ok(Value::Nil);
    }
    let distinct_id = super::want_string(&super::arg(args, 0), span, "telemetry.identify")?;
    let props_val = super::arg(args, 1);
    want_obj_or_nil(&props_val, span, "telemetry.identify")?;
    let set_props: Vec<(String, Value)> = match &props_val {
        Value::Object(o) => o
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        _ => Vec::new(),
    };
    interp.telemetry_enqueue_event(AnalyticsEvent {
        event: "$identify".to_string(),
        distinct_id: distinct_id.to_string(),
        properties: Vec::new(),
        set_props,
        time_unix_nano: model::now_unix_nanos(),
    });
    Ok(Value::Nil)
}

/// Native-method dispatch for span / instrument / no-op handles. Routed from
/// `Interp::call_native_method`.
pub async fn call_method(
    interp: &Interp,
    recv: &NativeObject,
    method: &str,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, Control> {
    match recv.kind {
        NativeKind::TelemetryNoop => Ok(Value::Nil),
        NativeKind::TelemetrySpan => span_method(interp, recv.id, method, args, span),
        NativeKind::TelemetryInstrument => instrument_method(interp, recv.id, method, args, span),
        _ => Err(AsError::at(
            format!("telemetry handle has no method '{}'", method),
            span,
        )
        .into()),
    }
}

fn span_method(
    interp: &Interp,
    id: u64,
    method: &str,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, Control> {
    match method {
        "setAttribute" => {
            let key = super::want_string(&super::arg(&args, 0), span, "span.setAttribute")?;
            let val = super::arg(&args, 1);
            // Route through the SP11 soft hook (the shared funnel).
            interp.telemetry_span_set(id, &key, val);
            Ok(Value::Nil)
        }
        "addEvent" => {
            let name = super::want_string(&super::arg(&args, 0), span, "span.addEvent")?;
            let opts = super::arg(&args, 1);
            want_obj_or_nil(&opts, span, "span.addEvent")?;
            let attrs = obj_pairs(&opts, "attributes");
            // Allow a flat attributes object too: addEvent("x", {k:v}).
            let attrs = if attrs.is_empty() {
                match &opts {
                    Value::Object(o) => o
                        .borrow()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    _ => Vec::new(),
                }
            } else {
                attrs
            };
            interp.telemetry_span_event(id, &name, attrs);
            Ok(Value::Nil)
        }
        "setStatus" => {
            let status = super::want_string(&super::arg(&args, 0), span, "span.setStatus")?;
            let code = match status.as_ref() {
                "ok" => SpanStatusCode::Ok,
                "error" => SpanStatusCode::Error,
                "unset" => SpanStatusCode::Unset,
                other => {
                    return Err(AsError::at(
                        format!("span.setStatus: unknown status {:?} (ok|error|unset)", other),
                        span,
                    )
                    .into())
                }
            };
            let message = match super::arg(&args, 1) {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            };
            interp.telemetry_span_set_status(id, code, message);
            Ok(Value::Nil)
        }
        "end" => {
            // Route through the SP11 soft hook; `Unset` preserves any status set
            // via `setStatus`.
            interp.telemetry_span_end(id, crate::interp::SpanStatus::Unset);
            Ok(Value::Nil)
        }
        other => Err(AsError::at(format!("span has no method '{}'", other), span).into()),
    }
}

fn instrument_method(
    interp: &Interp,
    id: u64,
    method: &str,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, Control> {
    match method {
        "add" | "record" | "set" => {
            let amount = super::want_number(&super::arg(&args, 0), span, "telemetry metric")?;
            let attrs_val = super::arg(&args, 1);
            want_obj_or_nil(&attrs_val, span, "telemetry metric")?;
            let attrs: Vec<(String, Value)> = match &attrs_val {
                Value::Object(o) => o
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                _ => Vec::new(),
            };
            interp.telemetry_record_metric(id, method, amount, attrs);
            Ok(Value::Nil)
        }
        other => Err(AsError::at(format!("instrument has no method '{}'", other), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use crate::interp::{Interp, SpanStatus};
    use crate::stdlib::telemetry::model::SpanCtx;
    use crate::value::Value;
    use std::rc::Rc;

    /// A fresh interp with telemetry initialized to a single OTLP exporter, used
    /// to exercise the SP11 hook + the in-Interp seam helpers directly in Rust.
    async fn active_interp() -> Rc<Interp> {
        let (_out, interp) = crate::run_source_with_interp(
            r#"
import * as telemetry from "std/telemetry"
telemetry.init({ service: "hook-test", exporters: [ telemetry.otlp({ endpoint: "http://localhost:4318" }) ] })
"#,
        )
        .await
        .expect("init program runs");
        interp
    }

    #[tokio::test]
    async fn soft_hook_active_only_when_initialized() {
        let interp = Rc::new(Interp::new());
        interp.install_self();
        // Uninitialized: the SP11 hook is inert.
        assert!(!interp.telemetry_active());
        assert!(interp.telemetry_span_start("x", vec![]).is_none());
    }

    #[tokio::test]
    async fn soft_hook_records_a_span() {
        let interp = active_interp().await;
        assert!(interp.telemetry_active());
        let id = interp
            .telemetry_span_start("chat openai:gpt-4.1", vec![("gen_ai.system".into(), Value::Str("openai".into()))])
            .expect("active → Some id");
        interp.telemetry_span_set(id, "gen_ai.usage.input_tokens", Value::Number(12.0));
        interp.telemetry_span_event(id, "first-token", vec![]);
        interp.telemetry_span_end(id, SpanStatus::Ok);
        // The span is buffered (capture is empty until flush, but the pipeline
        // holds exactly one span). Re-end is a no-op (id already removed).
        interp.telemetry_span_end(id, SpanStatus::Ok);
    }

    #[tokio::test]
    async fn current_span_task_local_set_restore() {
        let interp = active_interp().await;
        // The task-local current is per-task: set/restore round-trips inside a
        // telemetry scope (mirrors `telemetry.span`'s save → set → restore).
        crate::interp::telemetry_root_scope(async {
            assert!(interp.telemetry_current().is_none());
            let prev = interp.telemetry_set_current(Some(SpanCtx {
                resource_id: 7,
                trace_id: [1u8; 16],
                span_id: [2u8; 8],
            }));
            assert!(prev.is_none());
            assert_eq!(interp.telemetry_current().map(|c| c.resource_id), Some(7));
            interp.telemetry_set_current(prev);
            assert!(interp.telemetry_current().is_none());
        })
        .await;
    }
}
