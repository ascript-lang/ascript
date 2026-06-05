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
    _args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    // Touch the per-Interp AI client state so the genai `Client` is materialized
    // lazily on first use (Phases B–F populate the cache here before any await).
    interp.ai_state().ensure_initialized();
    match func {
        "provider" | "generate" | "stream" | "embed" | "embedMany" | "tool" => {
            Err(AsError::at(format!("std/ai: '{}' is not yet implemented", func), span).into())
        }
        other => Err(AsError::at(format!("std/ai has no function '{}'", other), span).into()),
    }
}
