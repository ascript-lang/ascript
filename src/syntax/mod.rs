//! Lossless concrete-syntax-tree front-end (cstree-backed).
//!
//! This CST front-end is the production front-end: it drives `compile` → the
//! default VM engine. The legacy `lexer`/`token`/`parser`/`ast` front-end now
//! backs only the `--tree-walker` reference oracle.

pub mod ast;
pub mod cst;
pub mod doc_comment;
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

/// A surfaced front-end syntax error: a human message and its byte range in the
/// source. Returned by [`first_syntax_error`] so the VM compile path can reject
/// malformed source with a clean parse error instead of compiling the recovered
/// (best-effort) tree — the legacy tree-walker front-end already rejects such
/// source at its parser, so this keeps the two engines' accept/reject behavior
/// aligned (e.g. an anonymous `fn(){...}` EXPRESSION, which is not a language
/// construct — only the arrow `() => ...` is — is rejected on BOTH engines
/// rather than crashing the VM compiler on a recovered, name-less `FnDecl`).
pub struct SyntaxError {
    pub message: String,
    pub start: usize,
    pub end: usize,
}

/// Parse `src` and return its FIRST syntax error (grammar or lexical), if any,
/// as a byte-ranged [`SyntaxError`]. The CST parser never aborts — it records
/// errors and recovers into a best-effort tree — so the compile path must
/// consult this before trusting the tree's shape. Lexical errors are reported
/// before grammar errors only when they occur earlier in the source (both are
/// mapped to byte ranges and the earliest is chosen).
///
/// Convenience for callers that only hold a `&str`. A caller that ALSO needs the
/// tree (e.g. the VM compile path) should parse ONCE and call
/// [`first_syntax_error_in`] on the resulting [`parser::Parse`], then hand that
/// same `Parse` to [`tree_builder::build_tree`] — parsing twice is wasted work.
pub fn first_syntax_error(src: &str) -> Option<SyntaxError> {
    first_syntax_error_in(&parser::parse(src))
}

/// The earliest [`SyntaxError`] recorded on an already-built [`parser::Parse`]
/// (grammar or lexical, by byte `start`), or `None` if the parse was clean. This
/// is the single source of truth; [`first_syntax_error`] is the parse-from-`&str`
/// wrapper. Reading the errors off the `Parse` is borrow-only, so the caller can
/// still move the `Parse` into `build_tree` afterwards.
pub fn first_syntax_error_in(parsed: &parser::Parse) -> Option<SyntaxError> {
    // Map a non-trivia token index to a byte range (mirrors `check::analyze`).
    let grammar = parsed.errors.first().map(|e| {
        let mut byte = 0usize;
        let mut non_trivia = 0usize;
        for t in &parsed.tokens {
            let len = t.text.len();
            if !t.kind.is_trivia() {
                if non_trivia == e.token_index {
                    return SyntaxError {
                        message: e.message.clone(),
                        start: byte,
                        end: byte + len.max(1),
                    };
                }
                non_trivia += 1;
            }
            byte += len;
        }
        let end = byte;
        SyntaxError {
            message: e.message.clone(),
            start: end.saturating_sub(1),
            end,
        }
    });
    // Lexical errors are indexed by FULL-token position.
    let lexical = parsed.lex_errors.first().map(|le| {
        let start: usize = parsed.tokens.iter().take(le.token).map(|t| t.text.len()).sum();
        let end = start
            + parsed
                .tokens
                .get(le.token)
                .map(|t| t.text.len())
                .unwrap_or(0);
        SyntaxError {
            message: le.message.clone(),
            start,
            end,
        }
    });
    match (grammar, lexical) {
        (Some(g), Some(l)) => Some(if l.start < g.start { l } else { g }),
        (Some(g), None) => Some(g),
        (None, l) => l,
    }
}

/// ALL syntax errors (grammar + lexical) recorded on an already-built
/// [`parser::Parse`], each as a byte-ranged [`SyntaxError`], sorted by `start`
/// (source order). Where [`first_syntax_error_in`] yields only the EARLIEST,
/// this batches them for multi-error reporting (DX D4 §5.1): a file with several
/// parse errors renders them all at once instead of "fix one, recompile, find the
/// next". Borrow-only — the caller can still move the `Parse` into `build_tree`.
pub fn all_syntax_errors_in(parsed: &parser::Parse) -> Vec<SyntaxError> {
    let mut out: Vec<SyntaxError> = Vec::new();
    // Grammar errors are indexed by NON-TRIVIA token position; map each to a byte
    // range exactly as `first_syntax_error_in` does for the first one.
    for e in &parsed.errors {
        let mut byte = 0usize;
        let mut non_trivia = 0usize;
        let mut mapped: Option<SyntaxError> = None;
        for t in &parsed.tokens {
            let len = t.text.len();
            if !t.kind.is_trivia() {
                if non_trivia == e.token_index {
                    mapped = Some(SyntaxError {
                        message: e.message.clone(),
                        start: byte,
                        end: byte + len.max(1),
                    });
                    break;
                }
                non_trivia += 1;
            }
            byte += len;
        }
        out.push(mapped.unwrap_or_else(|| {
            // token_index past the end → point at the final byte (EOF).
            let end: usize = parsed.tokens.iter().map(|t| t.text.len()).sum();
            SyntaxError {
                message: e.message.clone(),
                start: end.saturating_sub(1),
                end,
            }
        }));
    }
    // Lexical errors are indexed by FULL-token position.
    for le in &parsed.lex_errors {
        let start: usize = parsed.tokens.iter().take(le.token).map(|t| t.text.len()).sum();
        let end = start
            + parsed
                .tokens
                .get(le.token)
                .map(|t| t.text.len())
                .unwrap_or(0);
        out.push(SyntaxError {
            message: le.message.clone(),
            start,
            end,
        });
    }
    // Stable source order (DX D4 §5.1 determinism): by start, then end.
    out.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    out
}

/// Parse + resolve in one step (convenience for tests/tools).
pub fn resolve_source(src: &str) -> resolve::types::ResolveResult {
    resolve::resolve(&parse_to_tree(src))
}

/// Parse + format in one step (convenience for tests/tools).
pub fn format_tree(src: &str) -> String {
    format::format(&parse_to_tree(src))
}
