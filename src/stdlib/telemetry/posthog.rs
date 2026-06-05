//! PostHog exporter: `capture`/`identify` payloads (`api_key` + `event` +
//! `distinct_id` + `properties`; `$identify` with `$set`) posted to `/capture/`
//! or `/batch/`. The HTTP POST is filled in in phase F4.

use super::model::{PostHogExporter, TelemetryState};
use crate::interp::Interp;
use crate::value::Value;

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

/// Flush buffered analytics events → PostHog `/batch/`. Filled in phase F4.
pub async fn flush(_interp: &Interp, _state: &mut TelemetryState) -> Result<(), String> {
    Ok(())
}
