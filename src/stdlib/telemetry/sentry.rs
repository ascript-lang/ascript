//! Sentry exporter: DSN parse (→ envelope ingest URL + public key) and envelope
//! construction (a span tree → one transaction envelope; an error-status span →
//! an additional error-event item). Newline-delimited envelope format. The
//! envelope POST is filled in in phase F3.

use super::model::{hex, SentryExporter, SpanRecord, SpanStatusCode, TelemetryState};
use crate::interp::Interp;
use crate::value::Value;
use serde_json::{json, Map, Value as J};

/// Parse a `telemetry.sentry(...)` descriptor. `dsn` defaults to `SENTRY_DSN`.
/// The DSN `https://<public_key>@<host>/<project_id>` becomes the envelope URL
/// `https://<host>/api/<project_id>/envelope/`. A missing/unparseable DSN is a
/// Tier-1 error (`Err(String)`) — a misconfigured exporter is operational, not a
/// programmer bug.
pub fn parse_descriptor(d: &Value) -> Result<SentryExporter, String> {
    let dsn = super::obj_str(d, "dsn")
        .or_else(|| std::env::var("SENTRY_DSN").ok())
        .filter(|s| !s.is_empty());
    let dsn = match dsn {
        Some(s) => s,
        None => return Err("telemetry.sentry: missing DSN (set dsn or SENTRY_DSN)".to_string()),
    };
    parse_dsn(&dsn)
}

/// Parse a Sentry DSN into the envelope URL + public key. Format:
/// `<scheme>://<public_key>[:<secret>]@<host>[:<port>]/<path?>/<project_id>`.
pub fn parse_dsn(dsn: &str) -> Result<SentryExporter, String> {
    let bad = || format!("telemetry.sentry: malformed DSN {:?}", dsn);
    let (scheme, rest) = dsn.split_once("://").ok_or_else(bad)?;
    if scheme != "http" && scheme != "https" {
        return Err(bad());
    }
    let (userinfo, host_and_path) = rest.split_once('@').ok_or_else(bad)?;
    // public key is the user component (drop any `:secret`).
    let public_key = userinfo.split(':').next().unwrap_or("").to_string();
    if public_key.is_empty() {
        return Err(bad());
    }
    // host_and_path = host[:port]/<optional path>/<project_id>
    let (host_port, path) = match host_and_path.split_once('/') {
        Some((h, p)) => (h, p),
        None => return Err(bad()),
    };
    if host_port.is_empty() {
        return Err(bad());
    }
    // The project id is the LAST path segment; any preceding segments are a path
    // prefix (e.g. on a self-hosted Sentry behind a sub-path).
    let trimmed = path.trim_end_matches('/');
    let (prefix, project_id) = match trimmed.rsplit_once('/') {
        Some((pre, id)) => (format!("/{}", pre), id),
        None => (String::new(), trimmed),
    };
    if project_id.is_empty() || project_id.chars().any(|c| !c.is_ascii_alphanumeric()) {
        return Err(bad());
    }
    let envelope_url = format!(
        "{}://{}{}/api/{}/envelope/",
        scheme, host_port, prefix, project_id
    );
    Ok(SentryExporter {
        envelope_url,
        public_key,
    })
}

/// Nanoseconds → fractional seconds since the Unix epoch (Sentry timestamps).
fn nanos_to_secs(ns: u128) -> f64 {
    ns as f64 / 1_000_000_000.0
}

/// The Sentry `X-Sentry-Auth` header value for a public key.
fn auth_header(public_key: &str) -> String {
    format!(
        "Sentry sentry_version=7, sentry_client=ascript-telemetry/1.0, sentry_key={}",
        public_key
    )
}

/// Flush buffered spans → one Sentry **transaction** envelope per trace (the root
/// span is the transaction; its descendants are embedded child spans), plus an
/// **error event** item for each error-status span. The newline-delimited
/// envelope (envelope header line, then per-item: item-header line + payload
/// line) is POSTed to the DSN's `/api/<project>/envelope/` via the send seam.
pub async fn flush(interp: &Interp, state: &mut TelemetryState) -> Result<(), String> {
    let exp = match &state.exporters.sentry {
        Some(e) => e.clone(),
        None => return Ok(()),
    };
    if state.spans.is_empty() {
        return Ok(());
    }
    let resource = state.resource_attributes();
    let body = build_envelope(&exp, &resource, &state.spans);
    let req = super::model::CapturedRequest {
        exporter: "sentry",
        signal: "envelope",
        url: exp.envelope_url.clone(),
        headers: vec![("X-Sentry-Auth".to_string(), auth_header(&exp.public_key))],
        body,
    };
    interp.telemetry_send(req).await
}

/// Build the newline-delimited envelope text for the buffered spans.
fn build_envelope(
    _exp: &SentryExporter,
    resource: &[(String, Value)],
    spans: &[SpanRecord],
) -> String {
    let mut lines: Vec<String> = Vec::new();
    // Envelope header (minimal; no event_id at the envelope level — each item
    // carries its own).
    lines.push(json!({ "sent_at": iso_now() }).to_string());

    // Group spans by trace id. The root (parent_id == None) is the transaction.
    let mut traces: indexmap::IndexMap<[u8; 16], Vec<&SpanRecord>> = indexmap::IndexMap::new();
    for s in spans {
        traces.entry(s.trace_id).or_default().push(s);
    }
    for (trace_id, group) in &traces {
        if let Some(tx) = transaction_item(*trace_id, group, resource) {
            lines.push(json!({ "type": "transaction" }).to_string());
            lines.push(tx.to_string());
        }
        // An error-status span → an additional error event item.
        for s in group {
            if s.status.code == SpanStatusCode::Error {
                lines.push(json!({ "type": "event" }).to_string());
                lines.push(error_event_item(s, resource).to_string());
            }
        }
    }
    lines.join("\n")
}

/// The transaction payload for one trace: the root span as the transaction, its
/// descendants as embedded `spans`.
fn transaction_item(
    trace_id: [u8; 16],
    group: &[&SpanRecord],
    resource: &[(String, Value)],
) -> Option<J> {
    let root = group
        .iter()
        .find(|s| s.parent_id.is_none())
        .or_else(|| group.first())?;
    let children: Vec<J> = group
        .iter()
        .filter(|s| s.span_id != root.span_id)
        .map(|s| {
            json!({
                "span_id": hex(&s.span_id),
                "parent_span_id": s.parent_id.map(|p| hex(&p)).unwrap_or_else(|| hex(&root.span_id)),
                "trace_id": hex(&trace_id),
                "op": s.name,
                "description": s.name,
                "start_timestamp": nanos_to_secs(s.start_unix_nano),
                "timestamp": nanos_to_secs(s.end_unix_nano),
                "status": status_str(s.status.code),
                "data": data_map(&s.attributes),
            })
        })
        .collect();
    Some(json!({
        "event_id": hex(&root.trace_id),
        "type": "transaction",
        "transaction": root.name,
        "start_timestamp": nanos_to_secs(root.start_unix_nano),
        "timestamp": nanos_to_secs(root.end_unix_nano),
        "contexts": {
            "trace": {
                "trace_id": hex(&trace_id),
                "span_id": hex(&root.span_id),
                "op": root.name,
                "status": status_str(root.status.code),
            }
        },
        "tags": tags_map(resource),
        "spans": children,
    }))
}

/// An error event item for an error-status span.
fn error_event_item(s: &SpanRecord, resource: &[(String, Value)]) -> J {
    let message = s
        .status
        .message
        .clone()
        .unwrap_or_else(|| format!("span '{}' failed", s.name));
    json!({
        "event_id": hex(&s.span_id) + &hex(&s.span_id),
        "level": "error",
        "message": message,
        "timestamp": nanos_to_secs(s.end_unix_nano),
        "contexts": {
            "trace": {
                "trace_id": hex(&s.trace_id),
                "span_id": hex(&s.span_id),
            }
        },
        "tags": tags_map(resource),
        "extra": data_map(&s.attributes),
    })
}

/// Sentry span status string from an OTLP status code.
fn status_str(code: SpanStatusCode) -> &'static str {
    match code {
        SpanStatusCode::Ok => "ok",
        SpanStatusCode::Error => "internal_error",
        SpanStatusCode::Unset => "unknown",
    }
}

/// Resource attributes → a Sentry `tags` string map (values stringified).
fn tags_map(resource: &[(String, Value)]) -> J {
    let mut m = Map::new();
    for (k, v) in resource {
        m.insert(k.clone(), J::String(v.to_string()));
    }
    J::Object(m)
}

/// Span attributes → a Sentry `data`/`extra` map (JSON-typed values).
fn data_map(attrs: &[(String, Value)]) -> J {
    let mut m = Map::new();
    for (k, v) in attrs {
        m.insert(k.clone(), crate::stdlib::json::to_json_lossy(v, &mut Vec::new()));
    }
    J::Object(m)
}

/// A coarse ISO-8601 `sent_at` timestamp. We avoid a chrono dependency (telemetry
/// builds with only `data` + `net`): emit the epoch-seconds as an RFC3339-ish
/// string Sentry accepts (it tolerates a numeric or string timestamp; we send a
/// UTC string built from the current time).
fn iso_now() -> String {
    let secs = nanos_to_secs(super::model::now_unix_nanos());
    // Sentry accepts a unix-timestamp number serialized as a string here.
    format!("{}", secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_standard_dsn() {
        let e = parse_dsn("https://abc123@o123.ingest.sentry.io/456").unwrap();
        assert_eq!(
            e.envelope_url,
            "https://o123.ingest.sentry.io/api/456/envelope/"
        );
        assert_eq!(e.public_key, "abc123");
    }

    #[test]
    fn rejects_a_malformed_dsn() {
        assert!(parse_dsn("not-a-dsn").is_err());
        assert!(parse_dsn("https://o123.ingest.sentry.io/456").is_err()); // no key
        assert!(parse_dsn("https://key@host").is_err()); // no project
    }
}
