//! Tool calling + the in-interpreter sequential tool-use loop (spec §2.5).
//!
//! `ai.tool({description, input, execute})` mints an `AiTool` native handle storing
//! the description, the input schema (class or std/schema), and the `execute`
//! callable. `ai.generate({..., tools: {name: tool}, maxSteps})` runs the loop INSIDE
//! std/ai (NOT genai's agent layer, to keep the Tier-1 semantics): genai returns tool
//! calls → validate the args against the tool's schema → call `execute` (awaited if
//! async) → append a `tool`-role message with the result → re-call genai → repeat up
//! to `maxSteps` or until a final (non-tool) answer. A tool `execute` returning
//! `[nil, err]` is fed back to the model as the tool result (recoverable). Sequential
//! in v1 (non-goal: parallel).

use genai::chat::{
    ChatMessage, ChatRequest, ChatResponse, ChatRole, MessageContent, Tool, ToolResponse,
};

use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;

/// A resolved tool ready for the loop: its name, genai `Tool` (with projected
/// schema), the `input` shape (for arg validation), and the `execute` callable.
pub(crate) struct ResolvedTool {
    pub name: String,
    pub tool: Tool,
    pub input_shape: Value,
    pub execute: Value,
}

/// `ai.tool({description, input, execute})` → an `AiTool` native handle. A malformed
/// definition (missing `input`, `execute` not callable) is a Tier-2 panic.
pub(crate) fn make_tool(interp: &Interp, args: &[Value], span: Span) -> Result<Value, Control> {
    let def = match args.first() {
        Some(v @ Value::Object(_)) | Some(v @ Value::Instance(_)) => v.clone(),
        _ => {
            return Err(AsError::at(
                "ai.tool(def): expected an object with input + execute",
                span,
            )
            .into())
        }
    };
    let input = super::request::get_field(&def, "input");
    let is_shape =
        matches!(input, Value::Class(_)) || crate::stdlib::schema::schema_kind(&input).is_some();
    if !is_shape {
        return Err(AsError::at("ai.tool: 'input' must be a class or a std/schema", span).into());
    }
    let execute = super::request::get_field(&def, "execute");
    if !is_callable(&execute) {
        return Err(AsError::at("ai.tool: 'execute' must be a function", span).into());
    }
    let mut fields = indexmap::IndexMap::new();
    if let Value::Str(d) = super::request::get_field(&def, "description") {
        fields.insert("description".to_string(), Value::Str(d));
    }
    fields.insert("input".to_string(), input);
    fields.insert("execute".to_string(), execute);
    Ok(interp.make_native_data(crate::value::NativeKind::AiTool, fields))
}

fn is_callable(v: &Value) -> bool {
    matches!(
        v,
        Value::Function(_)
            | Value::Closure(_)
            | Value::Builtin(_)
            | Value::BoundMethod(_)
            | Value::NativeMethod(_)
            | Value::ClassMethod(_, _)
    )
}

/// Resolve the `tools:` option (`{name: AiTool}` object) into [`ResolvedTool`]s,
/// projecting each tool's `input` shape to a JSON Schema for genai. A non-AiTool
/// value is a Tier-2 panic.
pub(crate) fn resolve_tools(
    interp: &Interp,
    tools_opt: &Value,
    span: Span,
) -> Result<Vec<ResolvedTool>, Control> {
    let map = match tools_opt {
        Value::Object(o) => o.borrow().clone(),
        Value::Nil => return Ok(Vec::new()),
        other => {
            return Err(AsError::at(
                format!(
                    "ai.generate: 'tools' must be an object of named tools, got {}",
                    crate::interp::type_name(other)
                ),
                span,
            )
            .into())
        }
    };
    let mut out = Vec::new();
    for (name, tool_val) in map.iter() {
        let n = match tool_val {
            Value::Native(nat) if nat.kind == crate::value::NativeKind::AiTool => nat,
            other => {
                return Err(AsError::at(
                    format!(
                        "ai.generate: tool '{}' must be an ai.tool(...) handle, got {}",
                        name,
                        crate::interp::type_name(other)
                    ),
                    span,
                )
                .into())
            }
        };
        let input_shape = n.fields.get("input").cloned().unwrap_or(Value::Nil);
        let execute = n.fields.get("execute").cloned().unwrap_or(Value::Nil);
        let description = match n.fields.get("description") {
            Some(Value::Str(s)) => Some(s.to_string()),
            _ => None,
        };
        let schema_json = interp.project_shape_json(&input_shape);
        let mut tool = Tool::new(name.clone()).with_schema(schema_json);
        if let Some(d) = description {
            tool = tool.with_description(d);
        }
        out.push(ResolvedTool {
            name: name.clone(),
            tool,
            input_shape,
            execute,
        });
    }
    Ok(out)
}

/// Inputs to the tool-use loop, bundled to keep the entry point's arity small.
pub(crate) struct ToolLoop<'a> {
    pub client: &'a genai::Client,
    pub target: super::request::ServiceTargetOrIden,
    pub chat_req: ChatRequest,
    pub chat_options: &'a genai::chat::ChatOptions,
    pub tools: &'a [ResolvedTool],
    pub max_steps: u32,
}

/// Run the sequential tool-use loop. `chat_req` already has the tools set. Returns
/// `Ok((neutral, steps))` on a final answer, or `Err(tier1_err)` on a provider error.
/// `max_steps` caps the tool-call turns.
pub(crate) async fn run_tool_loop(
    interp: &Interp,
    cfg: ToolLoop<'_>,
    span: Span,
) -> Result<Result<(super::response::NeutralResponse, Vec<Value>), Value>, Control> {
    let ToolLoop {
        client,
        target,
        mut chat_req,
        chat_options,
        tools,
        max_steps,
    } = cfg;
    let mut steps: Vec<Value> = Vec::new();

    for _step in 0..max_steps.max(1) {
        let resp: ChatResponse = match exec_turn(client, &target, chat_req.clone(), chat_options).await
        {
            Ok(r) => r,
            Err(e) => return Ok(Err(super::response::error_to_value(&e))),
        };

        let calls = resp.content.tool_calls();
        if calls.is_empty() {
            return Ok(Ok((super::response::neutral_from_genai(resp), steps)));
        }

        // Append the assistant's tool-call message, then run each tool sequentially
        // and append a single tool-responses message.
        let assistant_calls: Vec<genai::chat::ToolCall> =
            calls.iter().map(|c| (*c).clone()).collect();
        let mut tool_responses: Vec<ToolResponse> = Vec::new();
        for call in &assistant_calls {
            let tool = tools.iter().find(|t| t.name == call.fn_name);
            let (result_str, step_val) = match tool {
                Some(t) => run_one_tool(interp, t, call, span).await?,
                None => {
                    let msg = format!("unknown tool '{}'", call.fn_name);
                    (
                        format!("{{\"error\":{:?}}}", msg),
                        tool_step_value(&call.fn_name, &call.fn_arguments, &msg, true),
                    )
                }
            };
            tool_responses.push(ToolResponse::new(call.call_id.clone(), result_str));
            steps.push(step_val);
        }

        chat_req = chat_req
            .append_message(ChatMessage::assistant(MessageContent::from_tool_calls(
                assistant_calls,
            )))
            .append_message(ChatMessage::new(
                ChatRole::Tool,
                MessageContent::from_tool_responses(tool_responses),
            ));
    }

    // maxSteps reached without a final answer: one last call to surface the result.
    match exec_turn(client, &target, chat_req, chat_options).await {
        Ok(r) => Ok(Ok((super::response::neutral_from_genai(r), steps))),
        Err(e) => Ok(Err(super::response::error_to_value(&e))),
    }
}

/// Run one genai turn, cloning the (non-`Clone`-by-value-consumed) model spec.
async fn exec_turn(
    client: &genai::Client,
    target: &super::request::ServiceTargetOrIden,
    chat_req: ChatRequest,
    chat_options: &genai::chat::ChatOptions,
) -> Result<ChatResponse, genai::Error> {
    match target {
        super::request::ServiceTargetOrIden::Target(t) => {
            client.exec_chat(t.clone(), chat_req, Some(chat_options)).await
        }
        super::request::ServiceTargetOrIden::Iden(id) => {
            client.exec_chat(id.clone(), chat_req, Some(chat_options)).await
        }
    }
}

/// Validate one tool call's args against the tool's input shape, run `execute`
/// (awaited if it returns a future), and return (result-as-string, step-value).
/// Validation failures and a `[nil, err]` return feed the error back to the model.
async fn run_one_tool(
    interp: &Interp,
    tool: &ResolvedTool,
    call: &genai::chat::ToolCall,
    span: Span,
) -> Result<(String, Value), Control> {
    let args_value = crate::stdlib::json::to_ascript(&call.fn_arguments);
    let arg_for_exec = match validate_args(interp, &tool.input_shape, args_value, span).await? {
        Ok(v) => v,
        Err(err) => {
            let msg = crate::interp::error_message(&err);
            return Ok((
                format!("{{\"error\":{:?}}}", msg),
                tool_step_value(&tool.name, &call.fn_arguments, &msg, true),
            ));
        }
    };

    let ret = interp
        .call_value(tool.execute.clone(), vec![arg_for_exec], span)
        .await?;
    let ret = interp.await_if_future(ret).await?;

    let (value, err) = unpair(&ret);
    if !matches!(err, Value::Nil) {
        let msg = crate::interp::error_message(&err);
        return Ok((
            format!("{{\"error\":{:?}}}", msg),
            tool_step_value(&tool.name, &call.fn_arguments, &msg, true),
        ));
    }
    let result_json = crate::stdlib::json::to_json_lossy(&value, &mut Vec::new());
    let result_str = serde_json::to_string(&result_json).unwrap_or_else(|_| "null".to_string());
    let step = tool_step_value(&tool.name, &call.fn_arguments, &result_str, false);
    Ok((result_str, step))
}

/// Validate `args` against `shape` (class or schema), returning `Ok(value)` or
/// `Err(tier1_err_value)` (NOT a panic — arg errors feed back to the model).
async fn validate_args(
    interp: &Interp,
    shape: &Value,
    args: Value,
    span: Span,
) -> Result<Result<Value, Value>, Control> {
    let pair = crate::interp::make_pair(args, Value::Nil);
    let decoded = interp.typed_decode(pair, shape, false, "", span).await?;
    Ok(unpair_result(&decoded))
}

fn unpair(v: &Value) -> (Value, Value) {
    match v {
        Value::Array(a) if a.borrow().len() == 2 => {
            let b = a.borrow();
            (b[0].clone(), b[1].clone())
        }
        other => (other.clone(), Value::Nil),
    }
}

fn unpair_result(v: &Value) -> Result<Value, Value> {
    let (value, err) = unpair(v);
    if matches!(err, Value::Nil) {
        Ok(value)
    } else {
        Err(err)
    }
}

/// Build a per-turn `step` value `{tool, arguments, result|error}`.
fn tool_step_value(name: &str, args: &serde_json::Value, result: &str, is_error: bool) -> Value {
    let mut m = indexmap::IndexMap::new();
    m.insert("tool".to_string(), Value::Str(name.into()));
    m.insert("arguments".to_string(), crate::stdlib::json::to_ascript(args));
    if is_error {
        m.insert("error".to_string(), Value::Str(result.into()));
    } else {
        m.insert("result".to_string(), Value::Str(result.into()));
    }
    Value::Object(crate::value::ObjectCell::new(m))
}
