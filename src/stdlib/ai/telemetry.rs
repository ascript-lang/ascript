//! GenAI-convention OTel spans emitted through SP12's runtime soft hook
//! (`Interp::telemetry_*`). NO Cargo dependency on `telemetry` — the hook methods
//! have always-present signatures that are inert when telemetry is off or
//! uninitialized, so these calls compile and run whether or not the `telemetry`
//! feature is enabled and take NO branch on a Cargo feature.
//!
//! Spans follow the OpenTelemetry GenAI semantic conventions (spec §3): a span named
//! `chat {provider:model}` (or `embeddings {model}`) with `gen_ai.*` attributes.
//! **PII-safe defaults:** usage/timing/model are always recorded when telemetry is
//! active; prompt/response CONTENT is recorded only when `telemetry.recordInputs` /
//! `recordOutputs` are explicitly set true (off by default). A per-call
//! `telemetry: { enabled: false }` disables the span for that call.

use crate::interp::{Interp, SpanStatus};
use crate::value::Value;

use super::request::{GenOpts, ResolvedModel};
use super::response::NeutralResponse;

/// Per-call telemetry options parsed from `opts.telemetry`.
struct TelemetryOpts {
    enabled: bool,
    record_inputs: bool,
    record_outputs: bool,
}

fn parse_opts(opts: &Value) -> TelemetryOpts {
    let t = super::request::get_field(opts, "telemetry");
    let enabled = match super::request::get_field(&t, "enabled") {
        Value::Bool(b) => b,
        _ => true, // default ON when telemetry is initialized
    };
    let record_inputs = matches!(super::request::get_field(&t, "recordInputs"), Value::Bool(true));
    let record_outputs =
        matches!(super::request::get_field(&t, "recordOutputs"), Value::Bool(true));
    TelemetryOpts {
        enabled,
        record_inputs,
        record_outputs,
    }
}

/// Open a `chat {model}` GenAI span. Returns the span id (or `None` when telemetry is
/// inactive / disabled for this call). Records request attributes; prompt CONTENT
/// only when `recordInputs` is set.
pub(crate) fn open_chat_span(
    interp: &Interp,
    opts: &Value,
    resolved: &ResolvedModel,
    model_label: &str,
    gen_opts: &GenOpts,
) -> Option<u64> {
    let topts = parse_opts(opts);
    if !topts.enabled || !interp.telemetry_active() {
        return None;
    }
    let mut attrs: Vec<(String, Value)> = vec![
        ("gen_ai.operation.name".into(), s("chat")),
        ("gen_ai.provider.name".into(), s(&resolved.provider_tag)),
        ("gen_ai.request.model".into(), s(&resolved.model)),
    ];
    if let Some(t) = gen_opts.temperature {
        attrs.push(("gen_ai.request.temperature".into(), Value::Float(t)));
    }
    if let Some(mt) = gen_opts.max_tokens {
        attrs.push(("gen_ai.request.max_tokens".into(), Value::Float(mt as f64)));
    }
    if let Some(p) = gen_opts.top_p {
        attrs.push(("gen_ai.request.top_p".into(), Value::Float(p)));
    }
    if topts.record_inputs {
        if let Value::Str(p) = super::request::get_field(opts, "prompt") {
            attrs.push(("gen_ai.prompt".into(), Value::Str(p)));
        }
    }
    let id = interp.telemetry_span_start(&format!("chat {}", model_label), attrs);
    // Stash recordOutputs on the span so close can honor it without re-parsing.
    if let Some(sid) = id {
        interp.telemetry_span_set(
            sid,
            "_ascript.record_outputs",
            Value::Bool(topts.record_outputs),
        );
    }
    id
}

/// Close a chat span: record response attributes (finish reason, usage) and the
/// status. Response CONTENT only when `recordOutputs` was set. No-op if `span` is
/// `None`.
pub(crate) fn close_chat_span(
    interp: &Interp,
    span: Option<u64>,
    neutral: Option<&NeutralResponse>,
    is_error: bool,
) {
    let Some(id) = span else { return };
    if let Some(n) = neutral {
        if let Some(fr) = &n.finish_reason {
            interp.telemetry_span_set(id, "gen_ai.response.finish_reasons", s(fr));
        }
        if let Some(it) = n.input_tokens {
            interp.telemetry_span_set(id, "gen_ai.usage.input_tokens", Value::Float(it as f64));
        }
        if let Some(ot) = n.output_tokens {
            interp.telemetry_span_set(id, "gen_ai.usage.output_tokens", Value::Float(ot as f64));
        }
    }
    let status = if is_error {
        SpanStatus::Error
    } else {
        SpanStatus::Ok
    };
    interp.telemetry_span_end(id, status);
}

/// Open an `embeddings {model}` GenAI span. Returns the span id (or `None`).
pub(crate) fn open_embed_span(
    interp: &Interp,
    opts: &Value,
    resolved: &ResolvedModel,
    model_label: &str,
) -> Option<u64> {
    let topts = parse_opts(opts);
    if !topts.enabled || !interp.telemetry_active() {
        return None;
    }
    let attrs: Vec<(String, Value)> = vec![
        ("gen_ai.operation.name".into(), s("embeddings")),
        ("gen_ai.provider.name".into(), s(&resolved.provider_tag)),
        ("gen_ai.request.model".into(), s(&resolved.model)),
    ];
    interp.telemetry_span_start(&format!("embeddings {}", model_label), attrs)
}

/// Close an embeddings span with usage + status.
pub(crate) fn close_embed_span(
    interp: &Interp,
    span: Option<u64>,
    input_tokens: Option<i64>,
    is_error: bool,
) {
    let Some(id) = span else { return };
    if let Some(it) = input_tokens {
        interp.telemetry_span_set(id, "gen_ai.usage.input_tokens", Value::Float(it as f64));
    }
    interp.telemetry_span_end(
        id,
        if is_error {
            SpanStatus::Error
        } else {
            SpanStatus::Ok
        },
    );
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}
