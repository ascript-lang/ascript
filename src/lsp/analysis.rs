//! Pure static-analysis layer for the LSP.
//!
//! Every function here takes `&str` source and returns owned `lsp_types` data.
//! It reuses the interpreter's `lexer`/`parser` but NEVER runs the interpreter, so
//! it holds no `Rc`/`RefCell`/`Value` and is trivially `Send`. This keeps the
//! tower-lsp `LanguageServer` impl `Send + Sync`-clean.

use crate::error::AsError;
use crate::lexer;
use crate::lsp::line_index::LineIndex;
use crate::parser;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

/// Lex + parse `text`, reporting the first lex-or-parse error as a single
/// `Diagnostic`. Valid programs produce an empty vec.
pub fn diagnostics(text: &str) -> Vec<Diagnostic> {
    let index = LineIndex::new(text);
    match lexer::lex(text) {
        Err(e) => vec![error_diagnostic(&e, &index)],
        Ok(tokens) => match parser::parse(&tokens) {
            Err(e) => vec![error_diagnostic(&e, &index)],
            Ok(_) => Vec::new(),
        },
    }
}

/// Build an Error-severity diagnostic from an `AsError`, using its span (via the
/// `LineIndex`) for the range, or the whole first line when no span is present.
fn error_diagnostic(error: &AsError, index: &LineIndex) -> Diagnostic {
    let range = match error.span {
        Some(span) => Range { start: index.position(span.start), end: index.position(span.end) },
        // No span: point at the start of the document (line 0).
        None => Range { start: Position::new(0, 0), end: Position::new(0, 0) },
    };
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("ascript".to_string()),
        message: error.message.clone(),
        ..Diagnostic::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_program_has_no_diagnostics() {
        let diags = diagnostics("let x = 1\nprint(x)");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn unterminated_string_is_one_error() {
        let diags = diagnostics("let s = \"oops");
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.source.as_deref(), Some("ascript"));
        assert!(
            d.message.to_lowercase().contains("string"),
            "message should mention string, got: {}",
            d.message
        );
        // A plausible range: start no later than end, within the single line.
        assert_eq!(d.range.start.line, 0);
        assert!(d.range.start.character <= d.range.end.character || d.range.end.line > 0);
    }

    #[test]
    fn parse_error_is_one_error() {
        let diags = diagnostics("let = 5");
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.source.as_deref(), Some("ascript"));
        assert!(!d.message.is_empty());
    }
}
