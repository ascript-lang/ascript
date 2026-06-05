//! Sentry exporter: DSN parse (→ envelope ingest URL + public key) and envelope
//! construction (a span tree → one transaction envelope; an error-status span →
//! an additional error-event item). Newline-delimited envelope format. The
//! envelope POST is filled in in phase F3.

use super::model::{SentryExporter, TelemetryState};
use crate::interp::Interp;
use crate::value::Value;

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

/// Flush buffered spans → a Sentry transaction envelope (+ error events for
/// error-status spans). Filled in phase F3.
pub async fn flush(_interp: &Interp, _state: &mut TelemetryState) -> Result<(), String> {
    Ok(())
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
