//! Streaming chat (generators + `for await`). Phase C materializes the genai
//! `exec_chat_stream` → typed-chunk mapping. Phase B carries the method-dispatch
//! entry point so the native-method router compiles.

use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;

/// Dispatch a method on an `AiStream`/`AiTextStream` handle (`next`/`textOnly`/
/// `result`). Phase C implements these; Phase B returns a clear "not yet" error.
pub(crate) async fn call_stream_method(
    _interp: &Interp,
    m: &crate::value::NativeMethod,
    _args: Vec<Value>,
    span: Span,
) -> Result<Value, Control> {
    Err(AsError::at(
        format!("ai stream method '{}' is not yet implemented", m.method),
        span,
    )
    .into())
}
