//! Streaming chat (`ai.stream`) → generators + `for await`.
//!
//! `ai.stream(opts)` returns a Tier-1 `[stream, err]`; `stream` is a native handle
//! (`NativeKind::AiStream`) backed by [`AiStreamState`] in `Interp.resources`. It is
//! consumed by `for await (chunk in stream)` (which calls `next()` until a `nil`
//! chunk) or directly via `await stream.next()`. `stream.textOnly()` returns an
//! adapter handle (`AiTextStream`) over the SAME underlying genai stream that yields
//! bare text strings; `stream.result()` returns the terminal aggregate once the loop
//! has drained.
//!
//! Each chunk is `{ type: "text"|"toolCall"|"finish", ... }`. `next()` polls ONE
//! genai event per call using the take-out-across-await discipline: the stream state
//! is taken OUT of `Interp.resources`, polled on the owned value (no `RefCell` borrow
//! across the await), then returned.

use futures_util::StreamExt;
use genai::chat::{ChatStream, ChatStreamEvent};

use crate::error::AsError;
use crate::interp::{Control, Interp, ResourceState};
use crate::span::Span;
use crate::value::Value;

use super::response::{finish_reason_str, NeutralResponse, NeutralToolCall};

/// The live streaming-chat resource: the genai event stream + the running terminal
/// aggregate (so `result()` is available after the loop) + a done flag.
pub struct AiStreamState {
    stream: ChatStream,
    /// The aggregate built as chunks flow (text concatenation, tool calls, usage,
    /// finishReason) — returned by `stream.result()`.
    aggregate: NeutralResponse,
    /// True once the genai stream has yielded its terminal `End` (or errored / run
    /// out): further `next()` calls return the end sentinel `[nil, nil]`.
    done: bool,
}

impl AiStreamState {
    pub(crate) fn new(stream: ChatStream) -> Self {
        AiStreamState {
            stream,
            aggregate: NeutralResponse::default(),
            done: false,
        }
    }
}

/// One mapped streaming step: the AScript chunk value (or `Nil` at end) and whether
/// the stream is now finished.
struct Step {
    chunk: Value,
    finished: bool,
}

/// `ai.stream(opts)` — open a streaming chat. Returns Tier-1 `[stream, err]`.
pub(crate) async fn stream(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let opts = match args.first() {
        Some(v @ Value::Object(_)) | Some(v @ Value::Instance(_)) => v.clone(),
        _ => {
            return Err(AsError::at(
                "ai.stream(opts): expected an options object with a 'model'",
                span,
            )
            .into())
        }
    };
    let model_arg = super::request::get_field(&opts, "model");
    if matches!(model_arg, Value::Nil) {
        return Err(AsError::at("ai.stream: 'model' is required", span).into());
    }
    let resolved = super::request::resolve_model(&model_arg, span)?;
    if let Some(err) = super::request::credential_missing_error(&resolved) {
        return Ok(crate::interp::make_pair(Value::Nil, err));
    }

    let chat_req = super::request::build_chat_request(&opts, span)?;
    let gen_opts = super::request::parse_gen_opts(&opts);
    let chat_options = super::build_chat_options(&gen_opts);

    let client = interp.ai_state().client();
    let spec = resolved.to_service_target_or_iden();
    let result = match spec {
        super::request::ServiceTargetOrIden::Target(t) => {
            client.exec_chat_stream(t, chat_req, Some(&chat_options)).await
        }
        super::request::ServiceTargetOrIden::Iden(iden) => {
            client.exec_chat_stream(iden, chat_req, Some(&chat_options)).await
        }
    };
    match result {
        Ok(resp) => {
            let state = AiStreamState::new(resp.stream);
            let handle = interp.register_resource(
                crate::value::NativeKind::AiStream,
                indexmap::IndexMap::new(),
                ResourceState::AiStream(Box::new(state)),
            );
            Ok(crate::interp::make_pair(handle, Value::Nil))
        }
        Err(e) => Ok(crate::interp::make_pair(
            Value::Nil,
            super::response::error_to_value(&e),
        )),
    }
}

/// Dispatch a method on an `AiStream`/`AiTextStream` handle.
pub(crate) async fn call_stream_method(
    interp: &Interp,
    m: &crate::value::NativeMethod,
    _args: Vec<Value>,
    span: Span,
) -> Result<Value, Control> {
    let text_only = m.receiver.kind == crate::value::NativeKind::AiTextStream;
    match m.method.as_str() {
        "next" => next(interp, m.receiver.id, text_only, span).await,
        "textOnly" => {
            // Return an AiTextStream adapter over the SAME resource id, so it polls
            // the one underlying genai stream and yields bare text strings.
            let mut fields = indexmap::IndexMap::new();
            fields.insert(
                "__streamId".to_string(),
                Value::Float(m.receiver.id as f64),
            );
            // The adapter shares the resource id: register_resource is NOT used; the
            // adapter handle carries the same id so `next()` resolves the same state.
            Ok(Value::Native(std::rc::Rc::new(crate::value::NativeObject {
                id: m.receiver.id,
                kind: crate::value::NativeKind::AiTextStream,
                fields,
            })))
        }
        "result" => result(interp, m.receiver.id, span),
        other => Err(AsError::at(
            format!("ai stream has no method '{}'", other),
            span,
        )
        .into()),
    }
}

/// Poll ONE genai event, map it to a chunk, and update the aggregate. Returns a
/// Tier-1 `[chunk, err]` pair (the `for await` contract): a `nil` chunk = end.
async fn next(interp: &Interp, id: u64, text_only: bool, _span: Span) -> Result<Value, Control> {
    loop {
        // Take the stream state OUT across the await (no resources borrow held).
        let mut state = match interp.take_resource(id) {
            Some(ResourceState::AiStream(s)) => s,
            // Already consumed/closed (or the text-only adapter outlived it): end.
            other => {
                if let Some(o) = other {
                    interp.return_resource(id, o);
                }
                return Ok(crate::interp::make_pair(Value::Nil, Value::Nil));
            }
        };
        if state.done {
            interp.return_resource(id, ResourceState::AiStream(state));
            return Ok(crate::interp::make_pair(Value::Nil, Value::Nil));
        }
        let event = state.stream.next().await;
        let outcome = map_event(&mut state, event, text_only);
        let finished = matches!(&outcome, StepOutcome::Err(_)) ;
        match outcome {
            StepOutcome::Yield(step) => {
                if step.finished {
                    state.done = true;
                }
                interp.return_resource(id, ResourceState::AiStream(state));
                return Ok(crate::interp::make_pair(step.chunk, Value::Nil));
            }
            StepOutcome::Skip => {
                // A non-emitting event (Start / reasoning / a text-only non-text
                // chunk): return the state and poll again.
                interp.return_resource(id, ResourceState::AiStream(state));
                continue;
            }
            StepOutcome::Err(err) => {
                let _ = finished;
                state.done = true;
                interp.return_resource(id, ResourceState::AiStream(state));
                return Ok(crate::interp::make_pair(Value::Nil, err));
            }
        }
    }
}

enum StepOutcome {
    Yield(Step),
    Skip,
    Err(Value),
}

/// Map a polled genai stream event into a step, updating the running aggregate.
fn map_event(
    state: &mut AiStreamState,
    event: Option<Result<ChatStreamEvent, genai::Error>>,
    text_only: bool,
) -> StepOutcome {
    match event {
        None => {
            // Stream exhausted without an explicit End frame: synthesize a finish.
            StepOutcome::Yield(Step {
                chunk: if text_only {
                    // text-only consumers get end-of-stream as a nil sentinel
                    Value::Nil
                } else {
                    finish_chunk(&state.aggregate)
                },
                finished: true,
            })
        }
        Some(Err(e)) => StepOutcome::Err(super::response::error_to_value(&e)),
        Some(Ok(ev)) => match ev {
            ChatStreamEvent::Start
            | ChatStreamEvent::ReasoningChunk(_)
            | ChatStreamEvent::ThoughtSignatureChunk(_) => StepOutcome::Skip,
            ChatStreamEvent::Chunk(c) => {
                state.aggregate.text.push_str(&c.content);
                if text_only {
                    StepOutcome::Yield(Step {
                        chunk: Value::Str(c.content.into()),
                        finished: false,
                    })
                } else {
                    StepOutcome::Yield(Step {
                        chunk: text_chunk(&c.content),
                        finished: false,
                    })
                }
            }
            ChatStreamEvent::ToolCallChunk(tc) => {
                let call = NeutralToolCall {
                    call_id: tc.tool_call.call_id.clone(),
                    name: tc.tool_call.fn_name.clone(),
                    args_json: tc.tool_call.fn_arguments.clone(),
                };
                state.aggregate.tool_calls.push(call.clone());
                if text_only {
                    StepOutcome::Skip
                } else {
                    StepOutcome::Yield(Step {
                        chunk: tool_call_chunk(&call),
                        finished: false,
                    })
                }
            }
            ChatStreamEvent::End(end) => {
                if let Some(u) = &end.captured_usage {
                    state.aggregate.input_tokens = u.prompt_tokens.map(|n| n as i64);
                    state.aggregate.output_tokens = u.completion_tokens.map(|n| n as i64);
                    state.aggregate.total_tokens = u.total_tokens.map(|n| n as i64);
                }
                if let Some(sr) = &end.captured_stop_reason {
                    state.aggregate.finish_reason = Some(finish_reason_str(sr).to_string());
                }
                // If genai captured the final text and we accumulated nothing (some
                // adapters only deliver text at End), use the captured text.
                if state.aggregate.text.is_empty() {
                    if let Some(t) = end.captured_first_text() {
                        state.aggregate.text = t.to_string();
                    }
                }
                StepOutcome::Yield(Step {
                    chunk: if text_only {
                        Value::Nil
                    } else {
                        finish_chunk(&state.aggregate)
                    },
                    finished: true,
                })
            }
        },
    }
}

/// `stream.result()` — the terminal aggregate `{text, finishReason, usage, toolCalls}`.
fn result(interp: &Interp, id: u64, _span: Span) -> Result<Value, Control> {
    interp.with_resource(id, |r| match r {
        Some(ResourceState::AiStream(s)) => {
            let mut m = indexmap::IndexMap::new();
            m.insert("text".to_string(), Value::Str(s.aggregate.text.clone().into()));
            m.insert(
                "finishReason".to_string(),
                match &s.aggregate.finish_reason {
                    Some(fr) => Value::Str(fr.clone().into()),
                    None => Value::Nil,
                },
            );
            m.insert("usage".to_string(), usage_value(&s.aggregate));
            m.insert(
                "toolCalls".to_string(),
                Value::Array(crate::value::ArrayCell::new(
                    super::response::tool_calls_value(&s.aggregate),
                )),
            );
            Ok(Value::Object(crate::value::ObjectCell::new(m)))
        }
        _ => {
            // Stream already fully drained + reclaimed: return an empty aggregate.
            let mut m = indexmap::IndexMap::new();
            m.insert("text".to_string(), Value::Str("".into()));
            m.insert("finishReason".to_string(), Value::Nil);
            m.insert("usage".to_string(), usage_value(&NeutralResponse::default()));
            m.insert(
                "toolCalls".to_string(),
                Value::Array(crate::value::ArrayCell::new(Vec::new())),
            );
            Ok(Value::Object(crate::value::ObjectCell::new(m)))
        }
    })
}

fn usage_value(n: &NeutralResponse) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("inputTokens".to_string(), opt_num(n.input_tokens));
    m.insert("outputTokens".to_string(), opt_num(n.output_tokens));
    m.insert("totalTokens".to_string(), opt_num(n.total_tokens));
    Value::Object(crate::value::ObjectCell::new(m))
}

fn opt_num(v: Option<i64>) -> Value {
    match v {
        // NUM §4: a token count is an `int`.
        Some(n) => Value::Int(n),
        None => Value::Nil,
    }
}

fn text_chunk(text: &str) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("type".to_string(), Value::Str("text".into()));
    m.insert("text".to_string(), Value::Str(text.into()));
    Value::Object(crate::value::ObjectCell::new(m))
}

fn tool_call_chunk(call: &NeutralToolCall) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("type".to_string(), Value::Str("toolCall".into()));
    m.insert("id".to_string(), Value::Str(call.call_id.clone().into()));
    m.insert("name".to_string(), Value::Str(call.name.clone().into()));
    m.insert(
        "arguments".to_string(),
        crate::stdlib::json::to_ascript(&call.args_json),
    );
    Value::Object(crate::value::ObjectCell::new(m))
}

fn finish_chunk(agg: &NeutralResponse) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("type".to_string(), Value::Str("finish".into()));
    m.insert(
        "finishReason".to_string(),
        match &agg.finish_reason {
            Some(fr) => Value::Str(fr.clone().into()),
            None => Value::Nil,
        },
    );
    m.insert("usage".to_string(), usage_value(agg));
    Value::Object(crate::value::ObjectCell::new(m))
}
