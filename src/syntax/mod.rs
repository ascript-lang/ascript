//! Lossless concrete-syntax-tree front-end (cstree-backed).
//!
//! This CST front-end is the production front-end: it drives `compile` → the
//! default VM engine. The legacy `lexer`/`token`/`parser`/`ast` front-end now
//! backs only the `--tree-walker` reference oracle.

pub mod ast;
pub mod cst;
pub mod event;
pub mod format;
pub mod kind;
pub mod lexer;
pub mod parser;
pub mod resolve;
pub mod tree_builder;

pub use cst::{build_flat_tree, ResolvedNode, SyntaxNode, SyntaxToken};
pub use kind::SyntaxKind;
pub use lexer::{lex, render, LexToken};

/// Parse source into a structured, lossless cstree tree.
pub fn parse_to_tree(src: &str) -> crate::syntax::cst::ResolvedNode {
    tree_builder::build_tree(parser::parse(src))
}

/// Parse + resolve in one step (convenience for tests/tools).
pub fn resolve_source(src: &str) -> resolve::types::ResolveResult {
    resolve::resolve(&parse_to_tree(src))
}

/// Parse + format in one step (convenience for tests/tools).
pub fn format_tree(src: &str) -> String {
    format::format(&parse_to_tree(src))
}
