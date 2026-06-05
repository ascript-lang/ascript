//! Provider/model handles, credential resolution, the genai `Client` lifetime, and
//! the `Value` → `genai::chat::ChatRequest`/`ChatOptions` mapping.
//!
//! The genai `Client` runs on the current-thread `LocalSet` (Phase A spike). It is
//! built once per `Interp` (with our pooled rustls reqwest client injected) and
//! cloned out of `Interp.ai` (a cheap `Arc`-backed handle) BEFORE any `.await`, so
//! no `RefCell` borrow is held across a genai future (take-out-across-await;
//! `await_holding_refcell_ref` stays satisfied).

use std::cell::RefCell;

use genai::adapter::AdapterKind;
use genai::chat::{ChatMessage, ChatRequest, ChatRole, ContentPart, MessageContent};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget};

use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;

thread_local! {
    /// Fixture-replay seam (tests only). When `Some(base_url)`, the genai client's
    /// `ServiceTargetResolver` rewrites EVERY request's endpoint to `base_url` and
    /// injects a dummy auth key — so the fixture-replay tests hit a loopback mock
    /// server with NO real network and NO secret. Empty in production (the resolver
    /// is a no-op and genai uses each adapter's real endpoint + env credential).
    /// Set by `ai_set_test_endpoint` (a `#[cfg(test)]`-style hook also reachable
    /// from the integration tests via an env var; see `dispatch`).
    static AI_TEST_ENDPOINT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Override (or clear) the fixture-replay endpoint for the current thread. Used by
/// the integration tests (which see only the crate's public API); production never
/// calls it. Re-exported at `crate::stdlib::ai::set_test_endpoint`.
pub fn set_test_endpoint(base_url: Option<String>) {
    AI_TEST_ENDPOINT.with(|c| *c.borrow_mut() = base_url);
}

fn test_endpoint() -> Option<String> {
    AI_TEST_ENDPOINT.with(|c| c.borrow().clone())
}

/// Per-`Interp` AI state: caches the lazily-built genai `Client`.
#[derive(Default)]
pub struct AiClient {
    client: Option<Client>,
}

impl AiClient {
    /// Get (building once) the per-`Interp` genai `Client`. The client's
    /// `ServiceTargetResolver` consults the thread-local fixture seam, the
    /// injected reqwest client is our pooled rustls client (shared with
    /// std/net/http), so connection pooling + TLS config are reused.
    pub(crate) fn client(&mut self) -> Client {
        if let Some(c) = &self.client {
            return c.clone();
        }
        let resolver = ServiceTargetResolver::from_resolver_fn(
            |mut tgt: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                if let Some(base) = test_endpoint() {
                    tgt.endpoint = Endpoint::from_owned(ensure_trailing_slash(&base));
                    tgt.auth = AuthData::from_single("test-key");
                }
                Ok(tgt)
            },
        );
        // NOTE: genai (v0.6) depends on reqwest 0.13, while the rest of AScript is on
        // reqwest 0.12 (two distinct crate versions / distinct `Client` types), so we
        // do NOT inject `net_http::shared_client()` — the types are incompatible. genai
        // builds its own pooled rustls reqwest client internally, which is fine; the
        // fixture-replay seam works through the `ServiceTargetResolver` endpoint
        // override above, not through client injection.
        let client = Client::builder()
            .with_service_target_resolver(resolver)
            .build();
        self.client = Some(client.clone());
        client
    }
}

/// A model resolved from a `Value` `model:` argument: which genai adapter, the bare
/// model name, and any explicit endpoint/auth override (from an `ai.provider(...)`
/// handle). Plain `Send` data — no `Value`/`Rc`.
#[derive(Clone)]
pub(crate) struct ResolvedModel {
    pub adapter: AdapterKind,
    pub model: String,
    /// The original provider tag (e.g. `"openai"`, `"ollama"`) for telemetry/errors.
    pub provider_tag: String,
    /// Explicit base URL override (openai-compatible / azure / a custom provider).
    pub base_url: Option<String>,
    /// Explicit API key override.
    pub api_key: Option<String>,
    /// Azure `api-version` (appended as a query param by genai's azure adapter).
    pub api_version: Option<String>,
}

impl ResolvedModel {
    /// Build the genai `ModelSpec` argument: a full `ServiceTarget` when an explicit
    /// endpoint/key override is present (provider handle), else a `ModelIden` so
    /// genai resolves the adapter's default endpoint + env credential.
    pub(crate) fn to_service_target_or_iden(&self) -> ServiceTargetOrIden {
        let iden = ModelIden::new(self.adapter, self.model.clone());
        if self.base_url.is_some() || self.api_key.is_some() {
            let endpoint = match &self.base_url {
                Some(u) => {
                    // Azure OpenAI keys its API on an `api-version` query param; fold
                    // it into the endpoint base so genai's adapter carries it through.
                    let base = match &self.api_version {
                        Some(v) if !u.contains("api-version=") => {
                            let sep = if u.contains('?') { '&' } else { '?' };
                            format!("{}{}api-version={}", ensure_trailing_slash(u), sep, v)
                        }
                        _ => ensure_trailing_slash(u),
                    };
                    Endpoint::from_owned(base)
                }
                None => default_endpoint_for(self.adapter),
            };
            let auth = match &self.api_key {
                Some(k) => AuthData::from_single(k.clone()),
                None => AuthData::from_env(default_key_env(self.adapter)),
            };
            ServiceTargetOrIden::Target(ServiceTarget {
                endpoint,
                auth,
                model: iden,
            })
        } else {
            ServiceTargetOrIden::Iden(iden)
        }
    }
}

/// Either a full target (explicit config) or a bare iden (env-resolved).
pub(crate) enum ServiceTargetOrIden {
    Target(ServiceTarget),
    Iden(ModelIden),
}

fn ensure_trailing_slash(u: &str) -> String {
    if u.ends_with('/') {
        u.to_string()
    } else {
        format!("{}/", u)
    }
}

/// Map a provider tag (the `"provider:"` prefix or `ai.provider(kind, ...)` kind) to
/// a genai `AdapterKind`. Returns `None` for an unknown kind (caller → Tier-2 panic).
/// The named openai-compatible presets (`ollama`/`openrouter`/`xai`/…) map to their
/// dedicated genai adapter where one exists, else to `OpenAI` over a custom base URL.
pub(crate) fn adapter_for(tag: &str) -> Option<AdapterKind> {
    let k = match tag {
        "openai" | "openai-compatible" | "azure" => AdapterKind::OpenAI,
        "anthropic" => AdapterKind::Anthropic,
        "google" | "gemini" => AdapterKind::Gemini,
        "bedrock" => AdapterKind::BedrockSigv4,
        "vertex" => AdapterKind::Vertex,
        "ollama" => AdapterKind::Ollama,
        "groq" => AdapterKind::Groq,
        "xai" => AdapterKind::Xai,
        "deepseek" => AdapterKind::DeepSeek,
        "together" => AdapterKind::Together,
        "fireworks" => AdapterKind::Fireworks,
        "cohere" => AdapterKind::Cohere,
        "openrouter" => AdapterKind::OpenRouter,
        "nebius" => AdapterKind::Nebius,
        "moonshot" => AdapterKind::Moonshot,
        _ => return None,
    };
    Some(k)
}

/// Does this provider tag carry credentials some other way than a single API key
/// env var (so a "missing key" pre-check would be wrong)? Bedrock uses the AWS
/// credential chain; Vertex uses ADC; ollama/openai-compatible/azure with an
/// explicit handle key need no env var.
fn uses_api_key_env(tag: &str) -> bool {
    !matches!(tag, "bedrock" | "vertex" | "ollama")
}

/// The env var name a provider's API key is read from (matches genai's adapter
/// defaults). Used both to build an explicit `AuthData::from_env` and to pre-check a
/// missing credential into a clean Tier-1 error.
pub(crate) fn default_key_env(adapter: AdapterKind) -> &'static str {
    match adapter {
        AdapterKind::OpenAI | AdapterKind::OpenAIResp => "OPENAI_API_KEY",
        AdapterKind::Anthropic => "ANTHROPIC_API_KEY",
        AdapterKind::Gemini => "GEMINI_API_KEY",
        AdapterKind::Groq => "GROQ_API_KEY",
        AdapterKind::Xai => "XAI_API_KEY",
        AdapterKind::DeepSeek => "DEEPSEEK_API_KEY",
        AdapterKind::Together => "TOGETHER_API_KEY",
        AdapterKind::Fireworks => "FIREWORKS_API_KEY",
        AdapterKind::Cohere => "COHERE_API_KEY",
        AdapterKind::OpenRouter => "OPENROUTER_API_KEY",
        AdapterKind::Nebius => "NEBIUS_API_KEY",
        AdapterKind::Moonshot => "MOONSHOT_API_KEY",
        _ => "OPENAI_API_KEY",
    }
}

fn default_endpoint_for(adapter: AdapterKind) -> Endpoint {
    // genai exposes per-adapter default endpoints internally; for the explicit-target
    // path we only override the endpoint when the caller gave a base URL, so this is
    // a conservative fallback used when api_key is set but base_url is not (rare).
    match adapter {
        AdapterKind::OpenAI => Endpoint::from_static("https://api.openai.com/v1/"),
        AdapterKind::Anthropic => Endpoint::from_static("https://api.anthropic.com/v1/"),
        AdapterKind::Gemini => {
            Endpoint::from_static("https://generativelanguage.googleapis.com/v1beta/")
        }
        _ => Endpoint::from_static("https://api.openai.com/v1/"),
    }
}

// ---- model-argument resolution from a Value -------------------------------

/// Resolve the `model:` argument of an ai.* call into a [`ResolvedModel`].
///
/// - A string `"provider:model"` → parse the provider prefix, env-resolved creds.
///   A bare `"model"` with no `:` is a Tier-2 misuse (the provider is required).
/// - An `AiModel` native handle (from `provider.model(id)`) → its stored config.
///
/// An unknown provider kind → Tier-2 panic (`AsError`). A missing required-key env
/// var (when no explicit key) is NOT checked here — it surfaces as a Tier-1 error at
/// call time (see `credential_missing_error`).
pub(crate) fn resolve_model(model: &Value, span: Span) -> Result<ResolvedModel, Control> {
    match model {
        Value::Str(s) => {
            let s = s.as_ref();
            let Some((tag, name)) = s.split_once(':') else {
                return Err(AsError::at(
                    format!(
                        "ai: model '{}' must be in 'provider:model' form (e.g. 'openai:gpt-4.1')",
                        s
                    ),
                    span,
                )
                .into());
            };
            let adapter = adapter_for(tag).ok_or_else(|| {
                Control::from(AsError::at(format!("ai: unknown provider '{}'", tag), span))
            })?;
            Ok(ResolvedModel {
                adapter,
                model: name.to_string(),
                provider_tag: tag.to_string(),
                base_url: None,
                api_key: None,
                api_version: None,
            })
        }
        Value::Native(n) if n.kind == crate::value::NativeKind::AiModel => {
            let tag = field_str(n, "provider").unwrap_or_default();
            let adapter = adapter_for(&tag).ok_or_else(|| {
                Control::from(AsError::at(format!("ai: unknown provider '{}'", tag), span))
            })?;
            Ok(ResolvedModel {
                adapter,
                model: field_str(n, "model").unwrap_or_default(),
                provider_tag: tag,
                base_url: field_str(n, "baseUrl"),
                api_key: field_str(n, "apiKey"),
                api_version: field_str(n, "apiVersion"),
            })
        }
        other => Err(AsError::at(
            format!(
                "ai: 'model' must be a 'provider:model' string or a provider.model(...) handle, got {}",
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

fn field_str(n: &crate::value::NativeObject, key: &str) -> Option<String> {
    match n.fields.get(key) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// Pre-check that a required API-key env var is present for an env-resolved model.
/// Returns `Some(tier1_err_value)` (a `{message}` object) when missing, so the
/// caller can return `[nil, err]`. `None` when credentials are present or are
/// resolved some other way (explicit handle key, Bedrock chain, Vertex ADC, ollama).
pub(crate) fn credential_missing_error(m: &ResolvedModel) -> Option<Value> {
    if m.api_key.is_some() {
        return None; // explicit key on the handle
    }
    if !uses_api_key_env(&m.provider_tag) {
        return None; // bedrock/vertex/ollama: no single-key env var
    }
    // The fixture seam injects a dummy key, so skip the check under replay.
    if test_endpoint().is_some() {
        return None;
    }
    let env_name = default_key_env(m.adapter);
    if std::env::var(env_name).is_ok() {
        return None;
    }
    Some(crate::interp::make_error(Value::Str(
        format!("no credential for provider '{}'", m.provider_tag).into(),
    )))
}

// ---- building the genai ChatRequest + ChatOptions from opts ----------------

/// The parsed-out chat options that map to genai `ChatOptions` setters.
#[derive(Default, Clone)]
pub(crate) struct GenOpts {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
}

/// Build a genai `ChatRequest` from the ai.generate/stream `opts` object. Handles
/// `prompt` (string) / `messages` (array) mutual exclusion (both set → Tier-2) and
/// hoists `system` to the request's system slot.
pub(crate) fn build_chat_request(opts: &Value, span: Span) -> Result<ChatRequest, Control> {
    let prompt = get_field(opts, "prompt");
    let messages = get_field(opts, "messages");
    let system = get_field(opts, "system");

    let has_prompt = !matches!(prompt, Value::Nil);
    let has_messages = !matches!(messages, Value::Nil);
    if has_prompt && has_messages {
        return Err(AsError::at(
            "ai.generate: 'prompt' and 'messages' are mutually exclusive — set only one",
            span,
        )
        .into());
    }
    if !has_prompt && !has_messages {
        return Err(AsError::at("ai.generate: provide a 'prompt' or 'messages'", span).into());
    }

    let mut chat_messages: Vec<ChatMessage> = Vec::new();
    if has_prompt {
        let p = match &prompt {
            Value::Str(s) => s.to_string(),
            other => {
                return Err(AsError::at(
                    format!(
                        "ai.generate: 'prompt' must be a string, got {}",
                        crate::interp::type_name(other)
                    ),
                    span,
                )
                .into())
            }
        };
        chat_messages.push(ChatMessage::user(p));
    } else {
        let arr = match &messages {
            Value::Array(a) => a.borrow().clone(),
            other => {
                return Err(AsError::at(
                    format!(
                        "ai.generate: 'messages' must be an array, got {}",
                        crate::interp::type_name(other)
                    ),
                    span,
                )
                .into())
            }
        };
        for msg in &arr {
            chat_messages.push(build_message(msg, span)?);
        }
    }

    let mut req = ChatRequest::new(chat_messages);
    if let Value::Str(s) = &system {
        req = req.with_system(s.to_string());
    }
    Ok(req)
}

/// Build one genai `ChatMessage` from a `{role, content}` object. `content` is a
/// string (text shorthand) or an array of typed parts (`text`/`image`/`file`).
fn build_message(msg: &Value, span: Span) -> Result<ChatMessage, Control> {
    let role_str = match get_field(msg, "role") {
        Value::Str(s) => s.to_string(),
        _ => {
            return Err(AsError::at(
                "ai.generate: each message needs a string 'role'",
                span,
            )
            .into())
        }
    };
    let role = match role_str.as_str() {
        "system" => ChatRole::System,
        "user" => ChatRole::User,
        "assistant" => ChatRole::Assistant,
        "tool" => ChatRole::Tool,
        other => {
            return Err(AsError::at(
                format!("ai.generate: unknown message role '{}'", other),
                span,
            )
            .into())
        }
    };
    let content = get_field(msg, "content");
    let mc = build_content(&content, span)?;
    Ok(ChatMessage::new(role, mc))
}

fn build_content(content: &Value, span: Span) -> Result<MessageContent, Control> {
    match content {
        Value::Str(s) => Ok(MessageContent::from(s.to_string())),
        Value::Array(a) => {
            let parts = a.borrow().clone();
            let mut out: Vec<ContentPart> = Vec::with_capacity(parts.len());
            for part in &parts {
                out.push(build_content_part(part, span)?);
            }
            Ok(MessageContent::from(out))
        }
        other => Err(AsError::at(
            format!(
                "ai.generate: message 'content' must be a string or array of parts, got {}",
                crate::interp::type_name(other)
            ),
            span,
        )
        .into()),
    }
}

fn build_content_part(part: &Value, span: Span) -> Result<ContentPart, Control> {
    let ty = match get_field(part, "type") {
        Value::Str(s) => s.to_string(),
        _ => "text".to_string(),
    };
    match ty.as_str() {
        "text" => match get_field(part, "text") {
            Value::Str(s) => Ok(ContentPart::from_text(s.to_string())),
            _ => Err(AsError::at("ai.generate: text part needs a string 'text'", span).into()),
        },
        "image" => build_binary_part(part, "image", span),
        "file" => build_binary_part(part, "file", span),
        other => Err(AsError::at(
            format!("ai.generate: unknown content part type '{}'", other),
            span,
        )
        .into()),
    }
}

fn build_binary_part(part: &Value, kind: &str, span: Span) -> Result<ContentPart, Control> {
    let media_type = match get_field(part, "mediaType") {
        Value::Str(s) => s.to_string(),
        _ => {
            return Err(AsError::at(
                format!("ai.generate: {} part needs a string 'mediaType'", kind),
                span,
            )
            .into())
        }
    };
    // A URL string ('data' is a string) or raw bytes ('data' is Bytes).
    match get_field(part, "data") {
        Value::Str(url) => Ok(ContentPart::from_binary_url(media_type, url.to_string(), None)),
        Value::Bytes(b) => {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(b.borrow().as_slice());
            Ok(ContentPart::from_binary_base64(media_type, encoded, None))
        }
        _ => Err(AsError::at(
            format!("ai.generate: {} part needs 'data' (a URL string or bytes)", kind),
            span,
        )
        .into()),
    }
}

/// Parse the numeric/sampling options from the `opts` object into [`GenOpts`].
pub(crate) fn parse_gen_opts(opts: &Value) -> GenOpts {
    let mut g = GenOpts::default();
    if let Value::Number(n) = get_field(opts, "maxTokens") {
        if n >= 0.0 {
            g.max_tokens = Some(n as u32);
        }
    }
    if let Value::Number(n) = get_field(opts, "temperature") {
        g.temperature = Some(n);
    }
    if let Value::Number(n) = get_field(opts, "topP") {
        g.top_p = Some(n);
    }
    g
}

/// Read a field from an `Object` or `Instance`; `Nil` if absent / not a container.
pub(crate) fn get_field(v: &Value, key: &str) -> Value {
    match v {
        Value::Object(o) => o.borrow().get(key).cloned().unwrap_or(Value::Nil),
        Value::Instance(i) => i.borrow().fields.get(key).cloned().unwrap_or(Value::Nil),
        _ => Value::Nil,
    }
}

// ---- provider/model handle construction ------------------------------------

/// Build the `ai.provider(kind, config)` native handle. Validates the kind (unknown
/// → Tier-2) and stores the config in the handle's `fields`.
pub(crate) fn make_provider(
    interp: &crate::interp::Interp,
    args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    let kind = match args.first() {
        Some(Value::Str(s)) => s.to_string(),
        _ => {
            return Err(AsError::at(
                "ai.provider(kind, config?): 'kind' must be a string",
                span,
            )
            .into())
        }
    };
    if adapter_for(&kind).is_none() {
        return Err(AsError::at(format!("ai: unknown provider '{}'", kind), span).into());
    }
    let mut fields = indexmap::IndexMap::new();
    fields.insert("kind".to_string(), Value::Str(kind.clone().into()));
    if let Some(cfg) = args.get(1) {
        for key in ["baseUrl", "apiKey", "apiVersion"] {
            if let Value::Str(s) = get_field(cfg, key) {
                fields.insert(key.to_string(), Value::Str(s));
            }
        }
    }
    Ok(interp.make_native_data(crate::value::NativeKind::AiProvider, fields))
}

/// Build the `provider.model(id)` native handle from a provider handle's stored
/// config + the model id.
pub(crate) fn make_model_from_provider(
    interp: &crate::interp::Interp,
    provider: &crate::value::NativeObject,
    model_id: &str,
) -> Value {
    let mut fields = indexmap::IndexMap::new();
    let kind = field_str(provider, "kind").unwrap_or_default();
    fields.insert("provider".to_string(), Value::Str(kind.into()));
    fields.insert("model".to_string(), Value::Str(model_id.into()));
    for key in ["baseUrl", "apiKey", "apiVersion"] {
        if let Some(v) = field_str(provider, key) {
            fields.insert(key.to_string(), Value::Str(v.into()));
        }
    }
    interp.make_native_data(crate::value::NativeKind::AiModel, fields)
}
