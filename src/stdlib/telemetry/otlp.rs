//! OTLP HTTP/JSON exporter: wire-shaping (spans → `ResourceSpans`, metrics →
//! `ResourceMetrics`, logs → `ResourceLogs`) and the `telemetry.otlp(...)`
//! descriptor parse. Hand-rolled proto3-JSON per the OTLP `http/json` mapping
//! (hex trace/span ids, `*UnixNano` as strings — NOT base64, NOT numbers).

use super::model::{OtlpExporter, TelemetryState};
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;

/// Parse a `telemetry.otlp(...)` descriptor into an [`OtlpExporter`]. Endpoint
/// defaults to `OTEL_EXPORTER_OTLP_ENDPOINT` then `http://localhost:4318`. The
/// only supported `protocol` is `http/json`; `http/protobuf`/`grpc` is a Tier-2
/// misuse panic. Outer `Err` = Tier-2; inner `Err(String)` = Tier-1 (never
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
    // Trim a trailing slash so per-signal paths join cleanly.
    let endpoint = endpoint.trim_end_matches('/').to_string();
    let headers = super::obj_headers(d, "headers");
    Ok(Ok(OtlpExporter { endpoint, headers }))
}

/// Flush buffered spans + metrics + (mirrored) event logs through the OTLP
/// exporter. Returns `Err(message)` (already not-fatal at the call site) on any
/// HTTP/transport failure. Filled in phase F2/F4.
pub async fn flush(_interp: &Interp, _state: &mut TelemetryState) -> Result<(), String> {
    Ok(())
}
