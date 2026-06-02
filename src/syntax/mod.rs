//! Lossless concrete-syntax-tree front-end (cstree-backed).
//!
//! Built in parallel with the legacy `lexer`/`token`/`parser`/`ast` front-end;
//! does not yet drive the binary.

pub mod ast;
pub mod cst;
pub mod event;
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
