//! Analysis driver: runs the CST parser and collects diagnostics.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::parser::{parse, Parse, ParseError};

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub diagnostics: Vec<AsDiagnostic>,
}

/// Run the full analysis over `src` and return all diagnostics, sorted by
/// start offset then code.
pub fn analyze(src: &str) -> Analysis {
    let parsed = parse(src);

    let mut diagnostics: Vec<AsDiagnostic> = Vec::new();
    for err in &parsed.errors {
        diagnostics.push(AsDiagnostic {
            range: error_range(&parsed, err),
            severity: Severity::Error,
            code: "syntax-error".into(),
            message: err.message.clone(),
            fix: None,
        });
    }

    diagnostics.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(a.code.cmp(&b.code))
    });

    Analysis { diagnostics }
}

/// Map a `ParseError`'s non-trivia token index to a byte span in `src`.
fn error_range(parsed: &Parse, err: &ParseError) -> ByteSpan {
    let mut byte = 0usize;
    let mut non_trivia = 0usize;
    for t in &parsed.tokens {
        let len = t.text.len();
        if !t.kind.is_trivia() {
            if non_trivia == err.token_index {
                return ByteSpan {
                    start: byte,
                    end: byte + len.max(1),
                };
            }
            non_trivia += 1;
        }
        byte += len;
    }
    // EOF / never-matched: point at the final byte.
    let end = byte;
    ByteSpan {
        start: end.saturating_sub(1),
        end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_no_diagnostics_for_valid_program() {
        let a = analyze("let x = 1\nprint(x)\n");
        assert!(
            a.diagnostics.is_empty(),
            "expected no diagnostics, got {:?}",
            a.diagnostics
        );
    }

    #[test]
    fn reports_all_syntax_errors_not_just_first() {
        let a = analyze("let = 1\nlet = 2\n");
        let n = a
            .diagnostics
            .iter()
            .filter(|d| d.code == "syntax-error")
            .count();
        assert!(
            n >= 2,
            "expected >=2 syntax-error diagnostics, got {n}: {:?}",
            a.diagnostics
        );
    }

    #[test]
    fn diagnostic_has_a_plausible_range() {
        let a = analyze("@\n");
        assert!(!a.diagnostics.is_empty(), "expected at least one diagnostic");
        let d = &a.diagnostics[0];
        assert!(
            d.range.start < d.range.end,
            "expected non-empty range, got {:?}",
            d.range
        );
    }
}
