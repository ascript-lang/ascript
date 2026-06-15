//! Map a genai `ChatResponse` (or error) into the AScript `out` Value and the
//! Tier-1 `[value, err]` pair. Also the error→Tier-1 mapping shared by generate /
//! stream / embed.

use genai::chat::{ChatResponse, StopReason, Usage};

use crate::value::Value;

/// The neutral, `Value`-free view of a finished chat turn. Built on the genai side
/// (the only place that touches genai types) and converted to a `Value` here. This
/// is also the shape `stream.result()` aggregates to.
#[derive(Default, Clone)]
pub(crate) struct NeutralResponse {
    pub text: String,
    pub finish_reason: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    /// Tool calls the model requested this turn (id, name, JSON args).
    pub tool_calls: Vec<NeutralToolCall>,
    /// The provider-native raw body (escape hatch), if captured.
    pub raw: Option<serde_json::Value>,
}

#[derive(Clone)]
pub(crate) struct NeutralToolCall {
    pub call_id: String,
    pub name: String,
    pub args_json: serde_json::Value,
}

/// Normalize a genai `StopReason` to the spec's finishReason strings.
pub(crate) fn finish_reason_str(sr: &StopReason) -> &'static str {
    match sr {
        StopReason::Completed(_) => "stop",
        StopReason::MaxTokens(_) => "length",
        StopReason::ToolCall(_) => "tool_calls",
        StopReason::ContentFilter(_) => "content_filter",
        StopReason::StopSequence(_) => "stop",
        StopReason::Other(_) => "other",
    }
}

fn usage_to_neutral(u: &Usage) -> (Option<i64>, Option<i64>, Option<i64>) {
    (
        u.prompt_tokens.map(|n| n as i64),
        u.completion_tokens.map(|n| n as i64),
        u.total_tokens.map(|n| n as i64),
    )
}

/// Build a [`NeutralResponse`] from a genai `ChatResponse`.
pub(crate) fn neutral_from_genai(resp: ChatResponse) -> NeutralResponse {
    let text = resp.first_text().unwrap_or_default().to_string();
    let finish_reason = resp
        .stop_reason
        .as_ref()
        .map(|s| finish_reason_str(s).to_string());
    let (input_tokens, output_tokens, total_tokens) = usage_to_neutral(&resp.usage);
    let tool_calls = resp
        .content
        .tool_calls()
        .into_iter()
        .map(|tc| NeutralToolCall {
            call_id: tc.call_id.clone(),
            name: tc.fn_name.clone(),
            args_json: tc.fn_arguments.clone(),
        })
        .collect();
    NeutralResponse {
        text,
        finish_reason,
        input_tokens,
        output_tokens,
        total_tokens,
        tool_calls,
        raw: resp.captured_raw_body,
    }
}

/// Build the AScript `out` object from a [`NeutralResponse`]. `steps` is supplied by
/// the tool loop (empty for a single non-tool turn).
pub(crate) fn out_object(n: &NeutralResponse, steps: Vec<Value>) -> Value {
    let mut map = indexmap::IndexMap::new();
    map.insert("text".to_string(), Value::str(n.text.clone()));
    map.insert(
        "finishReason".to_string(),
        match &n.finish_reason {
            Some(s) => Value::str(s.clone()),
            None => Value::nil(),
        },
    );
    map.insert("usage".to_string(), usage_object(n));
    map.insert(
        "toolCalls".to_string(),
        Value::array(tool_calls_value(n)),
    );
    map.insert(
        "steps".to_string(),
        Value::array(steps),
    );
    map.insert(
        "raw".to_string(),
        match &n.raw {
            Some(j) => crate::stdlib::json::to_ascript(j),
            None => Value::nil(),
        },
    );
    Value::object(map)
}

/// The `toolCalls` array value (`[{id, name, arguments}]`).
pub(crate) fn tool_calls_value(n: &NeutralResponse) -> Vec<Value> {
    n.tool_calls
        .iter()
        .map(|tc| {
            let mut m = indexmap::IndexMap::new();
            m.insert("id".to_string(), Value::str(tc.call_id.clone()));
            m.insert("name".to_string(), Value::str(tc.name.clone()));
            m.insert(
                "arguments".to_string(),
                crate::stdlib::json::to_ascript(&tc.args_json),
            );
            Value::object(m)
        })
        .collect()
}

fn usage_object(n: &NeutralResponse) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("inputTokens".to_string(), opt_num(n.input_tokens));
    m.insert("outputTokens".to_string(), opt_num(n.output_tokens));
    m.insert("totalTokens".to_string(), opt_num(n.total_tokens));
    Value::object(m)
}

fn opt_num(v: Option<i64>) -> Value {
    match v {
        // NUM §4: a token count is an `int`.
        Some(n) => Value::int(n),
        None => Value::nil(),
    }
}

/// Build the embeddings `out` object. Single: `{embedding:[..], usage}`.
/// Many: `{embeddings:[[..],..], usage}`. `usage` carries `inputTokens`.
pub(crate) fn embed_object(resp: &genai::embed::EmbedResponse, many: bool) -> Value {
    let mut m = indexmap::IndexMap::new();
    if many {
        let arrs: Vec<Value> = resp
            .embeddings
            .iter()
            .map(|e| vector_value(&e.vector))
            .collect();
        m.insert(
            "embeddings".to_string(),
            Value::array(arrs),
        );
    } else {
        let v = resp
            .embeddings
            .first()
            .map(|e| vector_value(&e.vector))
            .unwrap_or_else(|| Value::array(Vec::new()));
        m.insert("embedding".to_string(), v);
    }
    let mut usage = indexmap::IndexMap::new();
    usage.insert(
        "inputTokens".to_string(),
        match resp.usage.prompt_tokens {
            // NUM §4: a token count is an `int`.
            Some(n) => Value::int(n as i64),
            None => Value::nil(),
        },
    );
    m.insert(
        "usage".to_string(),
        Value::object(usage),
    );
    Value::object(m)
}

fn vector_value(v: &[f32]) -> Value {
    Value::array(
        v.iter().map(|f| Value::float(*f as f64)).collect(),
    )
}

/// Map a genai `Error` to the AScript Tier-1 `err` object `{message, status?}`.
pub(crate) fn error_to_value(err: &genai::Error) -> Value {
    let mut map = indexmap::IndexMap::new();
    let (message, status) = describe_error(err);
    map.insert("message".to_string(), Value::str(message));
    if let Some(s) = status {
        // NUM §4: an HTTP status code is an `Int`.
        map.insert("status".to_string(), Value::int(i64::from(s)));
    }
    Value::object(map)
}

/// Extract a human message + optional HTTP status from a genai error, digging into
/// the `WebModelCall { webc_error: ResponseFailedStatus { status, body } }` shape so
/// a provider 4xx/5xx surfaces `err.status`.
fn describe_error(err: &genai::Error) -> (String, Option<u16>) {
    match err {
        genai::Error::WebModelCall { webc_error, .. } => match webc_error {
            genai::webc::Error::ResponseFailedStatus { status, body, .. } => (
                format!("HTTP {} from provider: {}", status.as_u16(), short_body(body)),
                Some(status.as_u16()),
            ),
            other => (other.to_string(), None),
        },
        genai::Error::HttpError { status, body, .. } => (
            format!("HTTP {} from provider: {}", status.as_u16(), short_body(body)),
            Some(status.as_u16()),
        ),
        // A streaming HTTP failure: genai boxes the underlying `HttpError` inside
        // `WebStream { error: BoxError }`. Downcast to recover the status/body.
        genai::Error::WebStream { cause, error, .. } => {
            if let Some(genai::Error::HttpError { status, body, .. }) =
                error.downcast_ref::<genai::Error>()
            {
                return (
                    format!("HTTP {} from provider: {}", status.as_u16(), short_body(body)),
                    Some(status.as_u16()),
                );
            }
            (cause.clone(), None)
        }
        other => (other.to_string(), None),
    }
}

fn short_body(body: &str) -> String {
    let t = body.trim();
    if t.chars().count() > 600 {
        let truncated: String = t.chars().take(600).collect();
        format!("{}…", truncated)
    } else {
        t.to_string()
    }
}
