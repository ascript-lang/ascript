//! PostHog exporter: `capture`/`identify` payloads (`api_key` + `event` +
//! `distinct_id` + `properties`; `$identify` with `$set`) posted to `/capture/`
//! or `/batch/`. The HTTP POST is filled in in phase F4.

use super::model::{AnalyticsEvent, PostHogExporter, TelemetryState};
use crate::interp::Interp;
use crate::value::Value;
use serde_json::{json, Map, Value as J};

/// Parse a `telemetry.posthog(...)` descriptor. `apiKey` defaults to
/// `POSTHOG_KEY` then `POSTHOG_API_KEY`; `host` defaults to
/// `https://us.i.posthog.com`. A missing API key is a Tier-1 error.
pub fn parse_descriptor(d: &Value) -> Result<PostHogExporter, String> {
    let api_key = super::obj_str(d, "apiKey")
        .or_else(|| std::env::var("POSTHOG_KEY").ok())
        .or_else(|| std::env::var("POSTHOG_API_KEY").ok())
        .filter(|s| !s.is_empty());
    let api_key = match api_key {
        Some(k) => k,
        None => {
            return Err(
                "telemetry.posthog: missing apiKey (set apiKey or POSTHOG_KEY)".to_string(),
            )
        }
    };
    let host = super::obj_str(d, "host")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://us.i.posthog.com".to_string());
    let host = host.trim_end_matches('/').to_string();
    Ok(PostHogExporter { host, api_key })
}

/// Flush buffered analytics events → PostHog `/batch/` (one batch request with
/// the project `api_key` and a `batch` array of events). A `capture` becomes a
/// `{event, distinct_id, properties}` entry; an `identify` becomes a `$identify`
/// event with `$set` person properties. Routed through the send seam (capture in
/// tests, POST live). Events not bound for PostHog (no exporter configured) are
/// never enqueued, so this only runs when there is something to send.
pub async fn flush(interp: &Interp, state: &mut TelemetryState) -> Result<(), String> {
    let exp = match &state.exporters.posthog {
        Some(e) => e.clone(),
        None => return Ok(()),
    };
    if state.events.is_empty() {
        return Ok(());
    }
    let batch: Vec<J> = state.events.iter().map(event_obj).collect();
    let body = json!({
        "api_key": exp.api_key,
        "historical_migration": false,
        "batch": batch,
    })
    .to_string();
    let req = super::model::CapturedRequest {
        exporter: "posthog",
        signal: "events",
        url: format!("{}/batch/", exp.host),
        headers: Vec::new(),
        body,
    };
    interp.telemetry_send(req).await
}

/// One PostHog batch entry. `$identify` events carry their person properties
/// under `properties.$set`; ordinary captures carry their properties directly.
fn event_obj(e: &AnalyticsEvent) -> J {
    let mut props = Map::new();
    for (k, v) in &e.properties {
        props.insert(k.clone(), crate::stdlib::json::to_json_lossy(v, &mut Vec::new()));
    }
    if e.event == "$identify" && !e.set_props.is_empty() {
        let mut set = Map::new();
        for (k, v) in &e.set_props {
            set.insert(k.clone(), crate::stdlib::json::to_json_lossy(v, &mut Vec::new()));
        }
        props.insert("$set".to_string(), J::Object(set));
    }
    json!({
        "event": e.event,
        "distinct_id": e.distinct_id,
        "properties": J::Object(props),
        "timestamp": super::model::iso_timestamp(e.time_unix_nano),
    })
}
