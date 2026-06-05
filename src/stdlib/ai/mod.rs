//! `std/ai` — a unified, multi-provider LLM client (SP11).
//!
//! Wraps the [`genai`](https://crates.io/crates/genai) crate to cover the full v1
//! provider set in ONE dependency. Providers: OpenAI, every OpenAI-compatible
//! endpoint (Ollama, LM Studio, OpenRouter, LiteLLM, xAI, DeepSeek, groq, together,
//! Azure key-auth), native Anthropic, native Gemini, AWS Bedrock (SigV4), and GCP
//! Vertex (ADC).
//!
//! Surface (spec §2): `"provider:model"` selection with env-default credentials;
//! non-streaming text as a Tier-1 `[value, err]`; streaming via generators and
//! `for await`; class/`std/schema` structured output via a JSON-Schema projector;
//! and an in-interpreter tool-use loop. GenAI-convention OTel spans are emitted
//! through SP12's soft `Interp::telemetry_*` hook (opt-in, no Cargo dependency on
//! telemetry).
//!
//! ## The `!Send` path (Phase A spike result)
//!
//! AScript's runtime is `!Send` (`#[tokio::main(flavor = "current_thread")]` + a
//! `LocalSet`, `Rc`/`RefCell` interior mutability, `await_holding_refcell_ref =
//! "deny"`). The Phase-A spike (`tests/ai.rs::spike_*`) proved genai's
//! `Client::exec_chat` / `exec_chat_stream` / `embed` futures run **directly on the
//! current-thread `LocalSet` via `tokio::task::spawn_local`** with an `Rc<()>` held
//! in scope — they do NOT require `Send` and do NOT assume a multi-thread runtime
//! (genai is plain reqwest-based; reqwest works on a current-thread runtime). So
//! SP11 takes the **in-LocalSet path**: the genai `Client` is built once per
//! `Interp`, taken out of `Interp.resources`-style state across each `.await`
//! (take-out-across-await), and the futures are driven on our own loop — NO worker
//! thread, NO `mpsc` bridge. (The documented worker-thread fallback in the design
//! §1 was not needed and is not built.)

use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;

mod request;
mod response;
mod stream;
mod tools;

pub mod json_schema;

pub use request::AiClient;
pub use stream::AiStreamState;
/// Test-only fixture-replay seam: see [`request::set_test_endpoint`]. Re-exported
/// publicly so the integration tests (which can only see the crate's public API) can
/// point the genai client at a loopback mock server. Production never calls it.
pub use request::set_test_endpoint;

use genai::chat::ChatOptions;

/// The `(name, Value)` bindings `import * as ai from "std/ai"` brings in.
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("provider", super::bi("ai.provider")),
        ("generate", super::bi("ai.generate")),
        ("stream", super::bi("ai.stream")),
        ("embed", super::bi("ai.embed")),
        ("embedMany", super::bi("ai.embedMany")),
        ("tool", super::bi("ai.tool")),
    ]
}

/// Route a qualified `ai.<func>` call. Phase A returns a Tier-2 "not yet
/// implemented" panic for every function; Phases B–F replace these arms.
pub(crate) async fn dispatch(
    interp: &Interp,
    func: &str,
    args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    match func {
        "provider" => request::make_provider(interp, args, span),
        "generate" => generate(interp, args, span).await,
        "stream" => stream::stream(interp, args, span).await,
        "tool" => tools::make_tool(interp, args, span),
        "embed" | "embedMany" => {
            Err(AsError::at(format!("std/ai: '{}' is not yet implemented", func), span).into())
        }
        other => Err(AsError::at(format!("std/ai has no function '{}'", other), span).into()),
    }
}

/// Dispatch a method call on an `ai*` native handle (provider/model/stream/tool).
/// Phase B handles `provider.model(id)`; later phases add stream/tool methods.
pub(crate) async fn call_method(
    interp: &Interp,
    m: &crate::value::NativeMethod,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, Control> {
    use crate::value::NativeKind;
    match m.receiver.kind {
        NativeKind::AiProvider => match m.method.as_str() {
            "model" => {
                let id = match args.first() {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => {
                        return Err(AsError::at(
                            "provider.model(id): 'id' must be a string",
                            span,
                        )
                        .into())
                    }
                };
                Ok(request::make_model_from_provider(interp, &m.receiver, &id))
            }
            other => Err(AsError::at(
                format!("ai provider has no method '{}'", other),
                span,
            )
            .into()),
        },
        NativeKind::AiStream | NativeKind::AiTextStream => {
            stream::call_stream_method(interp, m, args, span).await
        }
        other => Err(AsError::at(
            format!("ai {} has no callable method", other.type_name()),
            span,
        )
        .into()),
    }
}

/// `ai.generate(opts)` — non-streaming text generation → Tier-1 `[out, err]`.
///
/// `opts` must be an object with a `model` plus a `prompt` or `messages`. Builds the
/// genai request, clones the genai `Client` OUT of `Interp.ai` BEFORE the await
/// (take-out-across-await — no `RefCell` borrow across the genai future), runs
/// `exec_chat`, and maps the result (or a missing-credential / provider error) into
/// a single Tier-1 pair.
async fn generate(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let opts = match args.first() {
        Some(v @ Value::Object(_)) | Some(v @ Value::Instance(_)) => v.clone(),
        _ => {
            return Err(AsError::at(
                "ai.generate(opts): expected an options object with a 'model'",
                span,
            )
            .into())
        }
    };
    let model_arg = request::get_field(&opts, "model");
    if matches!(model_arg, Value::Nil) {
        return Err(AsError::at("ai.generate: 'model' is required", span).into());
    }
    let resolved = request::resolve_model(&model_arg, span)?;

    // Missing credential → Tier-1 `[nil, err]` (expected operational failure).
    if let Some(err) = request::credential_missing_error(&resolved) {
        return Ok(crate::interp::make_pair(Value::Nil, err));
    }

    let mut chat_req = request::build_chat_request(&opts, span)?;
    let gen_opts = request::parse_gen_opts(&opts);
    let mut chat_options = build_chat_options(&gen_opts);

    // Structured output (`shape:` a class or std/schema). Project to JSON Schema,
    // set the provider's structured-output config AND embed the schema in the system
    // prompt (the always-available fallback, spec §7.2), then decode the result text
    // via `validate_into`/`parse_value`, fusing decode + shape into ONE Tier-1 pair.
    let shape = request::get_field(&opts, "shape");
    let shape_type = match &shape {
        Value::Class(_) => Some(shape.clone()),
        other if crate::stdlib::schema::schema_kind(other).is_some() => Some(shape.clone()),
        Value::Nil => None,
        other => {
            return Err(AsError::at(
                format!(
                    "ai.generate: 'shape' must be a class or a std/schema, got {}",
                    crate::interp::type_name(other)
                ),
                span,
            )
            .into())
        }
    };
    if let Some(st) = &shape_type {
        let schema_json = interp.project_shape_json(st);
        let sys = structured_system_prompt(&chat_req, &schema_json);
        chat_req = chat_req.with_system(sys);
        chat_options = chat_options.with_response_format(genai::chat::JsonSpec::new(
            "ascript_structured_output",
            schema_json,
        ));
    }

    // Tools: resolve the `tools:` map (an unknown/malformed tool → Tier-2 panic) and,
    // if any, set them on the request + run the in-interpreter tool-use loop.
    let resolved_tools = tools::resolve_tools(interp, &request::get_field(&opts, "tools"), span)?;
    let max_steps = match request::get_field(&opts, "maxSteps") {
        Value::Number(n) if n >= 1.0 => n as u32,
        _ => 5,
    };

    // Take the genai client OUT (clone the Arc-backed handle) before the await.
    let client = interp.ai_state().client();

    if !resolved_tools.is_empty() {
        let genai_tools: Vec<genai::chat::Tool> =
            resolved_tools.iter().map(|t| t.tool.clone()).collect();
        let chat_req = chat_req.with_tools(genai_tools);
        let spec = resolved.to_service_target_or_iden();
        let loop_result = tools::run_tool_loop(
            interp,
            tools::ToolLoop {
                client: &client,
                target: spec,
                chat_req,
                chat_options: &chat_options,
                tools: &resolved_tools,
                max_steps,
            },
            span,
        )
        .await?;
        return Ok(match loop_result {
            Ok((neutral, steps)) => {
                crate::interp::make_pair(response::out_object(&neutral, steps), Value::Nil)
            }
            Err(err) => crate::interp::make_pair(Value::Nil, err),
        });
    }

    let spec = resolved.to_service_target_or_iden();
    let result = match spec {
        request::ServiceTargetOrIden::Target(t) => {
            client.exec_chat(t, chat_req, Some(&chat_options)).await
        }
        request::ServiceTargetOrIden::Iden(iden) => {
            client.exec_chat(iden, chat_req, Some(&chat_options)).await
        }
    };

    let resp = match result {
        Ok(resp) => resp,
        Err(e) => {
            return Ok(crate::interp::make_pair(
                Value::Nil,
                response::error_to_value(&e),
            ))
        }
    };
    let neutral = response::neutral_from_genai(resp);

    // No shape → the plain `out` object.
    let Some(st) = shape_type else {
        let out = response::out_object(&neutral, Vec::new());
        return Ok(crate::interp::make_pair(out, Value::Nil));
    };

    // shape: → parse the model's text as JSON, then validate-into the shape, fusing a
    // decode failure and a shape mismatch into ONE Tier-1 [value, err] pair.
    let parsed_pair = crate::stdlib::json::call("parse", &[Value::Str(neutral.text.clone().into())], span)?;
    interp.typed_decode(parsed_pair, &st, false, "", span).await
}

/// Build a system prompt instructing the model to reply with ONLY JSON matching the
/// schema. Preserves any caller-supplied system text. This is the documented
/// always-available fallback (spec §7.2) on top of the provider structured-output
/// config, so even providers without native structured mode return parseable JSON.
fn structured_system_prompt(req: &genai::chat::ChatRequest, schema_json: &serde_json::Value) -> String {
    let existing = req.system.clone().unwrap_or_default();
    let schema_str = serde_json::to_string(schema_json).unwrap_or_else(|_| "{}".to_string());
    let instruction = format!(
        "Reply with ONLY a single JSON value (no markdown, no prose) that conforms to this JSON Schema:\n{}",
        schema_str
    );
    if existing.is_empty() {
        instruction
    } else {
        format!("{}\n\n{}", existing, instruction)
    }
}

/// Build genai `ChatOptions` from the parsed [`request::GenOpts`]. Always captures
/// usage + the raw body (the `out.raw` escape hatch) + tool calls.
pub(crate) fn build_chat_options(g: &request::GenOpts) -> ChatOptions {
    let mut o = ChatOptions::default()
        .with_capture_usage(true)
        .with_capture_content(true)
        .with_capture_tool_calls(true)
        .with_capture_raw_body(true);
    if let Some(mt) = g.max_tokens {
        o = o.with_max_tokens(mt);
    }
    if let Some(t) = g.temperature {
        o = o.with_temperature(t);
    }
    if let Some(p) = g.top_p {
        o = o.with_top_p(p);
    }
    o
}
