//! LSP providers. Each is a pure `fn(&SemanticModel, …) -> …` over the cached
//! model — no provider re-parses or touches the legacy `crate::{ast,lexer,parser}`.
pub mod docs;
pub mod hover;
pub mod navigation;
pub mod symbols;
