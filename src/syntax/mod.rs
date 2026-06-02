//! Lossless concrete-syntax-tree front-end (cstree-backed).
//!
//! Built in parallel with the legacy `lexer`/`token`/`parser`/`ast` front-end;
//! does not yet drive the binary.

pub mod cst;
pub mod kind;

pub use kind::SyntaxKind;
