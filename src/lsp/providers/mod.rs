//! LSP providers. Each is a pure `fn(&SemanticModel, …) -> …` over the cached
//! model — no provider re-parses or touches the legacy `crate::{ast,lexer,parser}`.
pub mod code_action;
pub mod completion;
pub mod docs;
pub mod formatting;
pub mod highlight;
pub mod hover;
pub mod navigation;
pub mod semantic_tokens;
pub mod signature;
pub mod symbols;
pub mod token_spans;
