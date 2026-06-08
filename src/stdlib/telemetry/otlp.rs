//! OTLP HTTP/JSON exporter: wire-shaping (spans → `ResourceSpans`, metrics →
//! `ResourceMetrics`, logs → `ResourceLogs`) and the `telemetry.otlp(...)`
//! descriptor parse. Hand-rolled proto3-JSON per the OTLP `http/json` mapping:
//! - trace/span ids are lowercase HEX strings (NOT base64),
//! - `*UnixNano` timestamps are DECIMAL STRINGS (proto3 maps 64-bit ints to JSON
//!   strings to avoid precision loss),
//! - attribute values are `KeyValue { key, value: AnyValue }`.
//!
//! Cumulative aggregation temporality (v1, spec §9.2).

use super::model::{
    AnalyticsEvent, MetricInstrument, MetricKind, OtlpExporter, SpanRecord, TelemetryState,
};
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use serde_json::{json, Map, Value as J};

/// OTLP aggregation temporality: cumulative (v1).
const AGGREGATION_TEMPORALITY_CUMULATIVE: i32 = 2;

/// Parse a `telemetry.otlp(...)` descriptor into an [`OtlpExporter`]. Endpoint
/// defaults to `OTEL_EXPORTER_OTLP_ENDPOINT` then `http://localhost:4318`. The
/// only supported `protocol` is `http/json`; `http/protobuf`/`grpc` is a Tier-2
/// misuse panic. Outer `Err` = Tier-2; inner `Err(String)` = Tier-1 (not
/// currently produced — OTLP has a working default endpoint — but the shape
/// matches `sentry`/`posthog`).
pub fn parse_descriptor(d: &Value, span: Span) -> Result<Result<OtlpExporter, String>, Control> {
    if let Some(proto) = super::obj_str(d, "protocol") {
        if proto != "http/json" {
            return Err(AsError::at(
                format!(
                    "telemetry.otlp: unsupported OTLP protocol {:?}; v1 supports http/json",
                    proto
                ),
                span,
            )
            .into());
        }
    }
    let endpoint = super::obj_str(d, "endpoint")
        .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok())
        .unwrap_or_else(|| "http://localhost:4318".to_string());
    let endpoint = endpoint.trim_end_matches('/').to_string();
    let headers = super::obj_headers(d, "headers");
    Ok(Ok(OtlpExporter { endpoint, headers }))
}

/// Flush buffered spans + metrics + (mirrored) event logs through the OTLP
/// exporter via the send seam. Returns `Err(message)` (already non-fatal at the
/// call site) on any HTTP/transport failure. The send seam records the request
/// in capture mode (tests) and POSTs in live mode (no borrow across the await).
pub async fn flush(interp: &Interp, state: &mut TelemetryState) -> Result<(), String> {
    let exp = match &state.exporters.otlp {
        Some(e) => e.clone(),
        None => return Ok(()),
    };
    let resource = state.resource_attributes();
    let mut first_err: Option<String> = None;

    // ---- traces ----
    if !state.spans.is_empty() {
        let body = serialize_spans(&resource, &state.spans);
        let req = super::model::CapturedRequest {
            exporter: "otlp",
            signal: "traces",
            url: format!("{}/v1/traces", exp.endpoint),
            headers: exp.headers.clone(),
            body: body.to_string(),
        };
        if let Err(e) = interp.telemetry_send(req).await {
            first_err.get_or_insert(e);
        }
    }

    // ---- metrics ----
    if !state.instruments.is_empty() {
        let instruments: Vec<&MetricInstrument> = state.instruments.values().collect();
        let body = serialize_metrics(&resource, &instruments);
        let req = super::model::CapturedRequest {
            exporter: "otlp",
            signal: "metrics",
            url: format!("{}/v1/metrics", exp.endpoint),
            headers: exp.headers.clone(),
            body: body.to_string(),
        };
        if let Err(e) = interp.telemetry_send(req).await {
            first_err.get_or_insert(e);
        }
    }

    // ---- logs (mirrored analytics events, opt-in) ----
    if state.mirror_events_to_otlp && !state.events.is_empty() {
        let body = serialize_logs(&resource, &state.events);
        let req = super::model::CapturedRequest {
            exporter: "otlp",
            signal: "logs",
            url: format!("{}/v1/logs", exp.endpoint),
            headers: exp.headers.clone(),
            body: body.to_string(),
        };
        if let Err(e) = interp.telemetry_send(req).await {
            first_err.get_or_insert(e);
        }
    }

    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ---- shaping ----

/// An OTLP `AnyValue` for an attribute value (string/int/double/bool; everything
/// else → `stringValue` via display, which is total and never panics).
fn any_value(v: &Value) -> J {
    match v {
        Value::Bool(b) => json!({ "boolValue": b }),
        Value::Float(n) => {
            // Integral numbers → intValue (as a string, proto3 64-bit mapping);
            // fractional → doubleValue.
            if n.fract() == 0.0 && n.is_finite() && n.abs() < 9.007_199_254_740_992e15 {
                json!({ "intValue": (*n as i64).to_string() })
            } else {
                json!({ "doubleValue": n })
            }
        }
        Value::Str(s) => json!({ "stringValue": s.as_ref() }),
        Value::Nil => json!({ "stringValue": "" }),
        other => json!({ "stringValue": other.to_string() }),
    }
}

/// A list of OTLP `KeyValue` from `(key, Value)` pairs.
fn key_values(attrs: &[(String, Value)]) -> J {
    J::Array(
        attrs
            .iter()
            .map(|(k, v)| json!({ "key": k, "value": any_value(v) }))
            .collect(),
    )
}

/// An OTLP `Resource` object from resource attributes.
fn resource_obj(resource: &[(String, Value)]) -> J {
    json!({ "attributes": key_values(resource) })
}

/// One OTLP `Span` object. Field order (name precedes ids/status) is relied on by
/// the tests' `field_for_name` helper.
fn span_obj(s: &SpanRecord) -> J {
    let mut obj = Map::new();
    obj.insert("name".into(), json!(s.name));
    obj.insert("traceId".into(), json!(super::model::hex(&s.trace_id)));
    obj.insert("spanId".into(), json!(super::model::hex(&s.span_id)));
    if let Some(p) = &s.parent_id {
        obj.insert("parentSpanId".into(), json!(super::model::hex(p)));
    }
    // SPAN_KIND_INTERNAL (1) — v1 does not distinguish client/server spans.
    obj.insert("kind".into(), json!(1));
    obj.insert(
        "startTimeUnixNano".into(),
        json!(s.start_unix_nano.to_string()),
    );
    obj.insert("endTimeUnixNano".into(), json!(s.end_unix_nano.to_string()));
    obj.insert("attributes".into(), key_values(&s.attributes));
    if !s.events.is_empty() {
        let events: Vec<J> = s
            .events
            .iter()
            .map(|e| {
                json!({
                    "name": e.name,
                    "timeUnixNano": e.time_unix_nano.to_string(),
                    "attributes": key_values(&e.attributes),
                })
            })
            .collect();
        obj.insert("events".into(), J::Array(events));
    }
    let mut status = Map::new();
    status.insert("code".into(), json!(s.status.code.otlp_code()));
    if let Some(m) = &s.status.message {
        status.insert("message".into(), json!(m));
    }
    obj.insert("status".into(), J::Object(status));
    J::Object(obj)
}

/// Serialize finished spans into an OTLP `ExportTraceServiceRequest` JSON value
/// (`{ resourceSpans: [ { resource, scopeSpans: [ { scope, spans:[...] } ] } ] }`).
pub fn serialize_spans(resource: &[(String, Value)], spans: &[SpanRecord]) -> J {
    let span_objs: Vec<J> = spans.iter().map(span_obj).collect();
    json!({
        "resourceSpans": [{
            "resource": resource_obj(resource),
            "scopeSpans": [{
                "scope": { "name": "ascript/telemetry", "version": "1" },
                "spans": span_objs,
            }],
        }],
    })
}

/// One OTLP `Metric` object for an instrument (Sum / Histogram / Gauge), with
/// cumulative temporality.
fn metric_obj(inst: &MetricInstrument) -> J {
    let mut metric = Map::new();
    metric.insert("name".into(), json!(inst.name));
    if let Some(u) = &inst.unit {
        metric.insert("unit".into(), json!(u));
    }
    if let Some(d) = &inst.description {
        metric.insert("description".into(), json!(d));
    }
    let start = inst.start_unix_nano.to_string();
    let now = super::model::now_unix_nanos().to_string();
    match inst.kind {
        MetricKind::Counter => {
            let dps: Vec<J> = inst
                .points
                .values()
                .map(|(attrs, p)| {
                    json!({
                        "attributes": key_values(attrs),
                        "startTimeUnixNano": start,
                        "timeUnixNano": now,
                        "asDouble": p.value,
                    })
                })
                .collect();
            metric.insert(
                "sum".into(),
                json!({
                    "dataPoints": dps,
                    "aggregationTemporality": AGGREGATION_TEMPORALITY_CUMULATIVE,
                    "isMonotonic": true,
                }),
            );
        }
        MetricKind::Gauge => {
            let dps: Vec<J> = inst
                .points
                .values()
                .map(|(attrs, p)| {
                    json!({
                        "attributes": key_values(attrs),
                        "timeUnixNano": now,
                        "asDouble": p.value,
                    })
                })
                .collect();
            metric.insert("gauge".into(), json!({ "dataPoints": dps }));
        }
        MetricKind::Histogram => {
            let dps: Vec<J> = inst
                .points
                .values()
                .map(|(attrs, p)| {
                    json!({
                        "attributes": key_values(attrs),
                        "startTimeUnixNano": start,
                        "timeUnixNano": now,
                        "count": p.count.to_string(),
                        "sum": p.value,
                        "min": p.min,
                        "max": p.max,
                    })
                })
                .collect();
            metric.insert(
                "histogram".into(),
                json!({
                    "dataPoints": dps,
                    "aggregationTemporality": AGGREGATION_TEMPORALITY_CUMULATIVE,
                }),
            );
        }
    }
    J::Object(metric)
}

/// Serialize instruments into an OTLP `ExportMetricsServiceRequest` JSON value.
pub fn serialize_metrics(resource: &[(String, Value)], instruments: &[&MetricInstrument]) -> J {
    let metrics: Vec<J> = instruments.iter().map(|i| metric_obj(i)).collect();
    json!({
        "resourceMetrics": [{
            "resource": resource_obj(resource),
            "scopeMetrics": [{
                "scope": { "name": "ascript/telemetry", "version": "1" },
                "metrics": metrics,
            }],
        }],
    })
}

/// Serialize analytics events as OTLP log records (the `mirrorEventsToOtlp`
/// path): each `capture` becomes a log record whose body is the event name and
/// whose attributes carry the properties + `distinct.id`.
pub fn serialize_logs(resource: &[(String, Value)], events: &[AnalyticsEvent]) -> J {
    let records: Vec<J> = events
        .iter()
        .map(|e| {
            let mut attrs = e.properties.clone();
            attrs.push((
                "distinct.id".to_string(),
                Value::Str(e.distinct_id.as_str().into()),
            ));
            json!({
                "timeUnixNano": e.time_unix_nano.to_string(),
                "body": { "stringValue": e.event },
                "attributes": key_values(&attrs),
            })
        })
        .collect();
    json!({
        "resourceLogs": [{
            "resource": resource_obj(resource),
            "scopeLogs": [{
                "scope": { "name": "ascript/telemetry", "version": "1" },
                "logRecords": records,
            }],
        }],
    })
}
