//! Lossless concrete-syntax-tree front-end (cstree-backed).
//!
//! Built in parallel with the legacy `lexer`/`token`/`parser`/`ast` front-end;
//! does not yet drive the binary.

pub mod cst;
pub mod kind;
pub mod lexer;

pub use cst::{build_flat_tree, ResolvedNode, SyntaxNode, SyntaxToken};
pub use kind::SyntaxKind;
pub use lexer::{lex, render, LexToken};
