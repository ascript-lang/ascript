//! Analysis driver: runs the CST parser and collects diagnostics.

use crate::check::diagnostic::{AsDiagnostic, ByteSpan, Severity};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::parser::{parse, Parse, ParseError};
use std::collections::{HashMap, HashSet};

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

    // Apply inline `ascript-ignore` suppressions before sorting.
    let supp = suppressions(src);
    let line_starts = line_start_offsets(src);
    diagnostics.retain(|d| {
        !supp.suppressed_on_line(line_of(&line_starts, d.range.start), &d.code)
    });

    diagnostics.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(a.code.cmp(&b.code))
    });

    Analysis { diagnostics }
}

/// Inline suppression directives parsed from comments.
#[derive(Debug, Clone, Default)]
pub struct Suppressions {
    /// Codes suppressed on a specific (0-based) line.
    per_line: HashMap<usize, HashSet<String>>,
    /// File-wide suppressed codes (`ascript-ignore-file`).
    file_wide: HashSet<String>,
}

impl Suppressions {
    /// Is `code` suppressed for a diagnostic on (0-based) `line`?
    ///
    /// A directive applies to its own line or the line immediately following
    /// it, and `*` matches every code.
    pub fn suppressed_on_line(&self, line: usize, code: &str) -> bool {
        if self.file_wide.contains(code) || self.file_wide.contains("*") {
            return true;
        }
        let matches = |l: usize| {
            self.per_line
                .get(&l)
                .map(|set| set.contains(code) || set.contains("*"))
                .unwrap_or(false)
        };
        matches(line) || matches(line.wrapping_sub(1))
    }
}

/// Scan `src` for inline `ascript-ignore` directives in comments.
pub fn suppressions(src: &str) -> Suppressions {
    let mut supp = Suppressions::default();
    let mut line = 0usize;
    for t in crate::syntax::lex(src) {
        if matches!(t.kind, SyntaxKind::LineComment | SyntaxKind::BlockComment) {
            if let Some((file_wide, codes)) = parse_ignore(&t.text) {
                if file_wide {
                    supp.file_wide.extend(codes);
                } else {
                    supp.per_line.entry(line).or_default().extend(codes);
                }
            }
        }
        line += t.text.matches('\n').count();
    }
    supp
}

/// Parse an `ascript-ignore` directive out of a comment's raw text.
///
/// Returns `(file_wide, codes)`. An empty code list means `*` (all codes).
fn parse_ignore(comment: &str) -> Option<(bool, Vec<String>)> {
    let body = comment.trim_start_matches('/').trim_start_matches('*');
    let body = body.trim_end_matches('*').trim_end_matches('/');
    let body = body.trim();

    let (file_wide, rest) = if let Some(rest) = body.strip_prefix("ascript-ignore-file") {
        (true, rest)
    } else if let Some(rest) = body.strip_prefix("ascript-ignore") {
        (false, rest)
    } else {
        return None;
    };

    let rest = rest.trim();
    if rest.is_empty() {
        return Some((file_wide, vec!["*".to_string()]));
    }

    // Parse `[a, b, c]`.
    let inner = rest.trim_start_matches('[').trim_end_matches(']');
    let codes: Vec<String> = inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let codes = if codes.is_empty() {
        vec!["*".to_string()]
    } else {
        codes
    };
    Some((file_wide, codes))
}

/// Byte offsets of the start of each line (line 0 starts at 0).
fn line_start_offsets(src: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// 0-based line containing `byte`, via the line-start table.
fn line_of(line_starts: &[usize], byte: usize) -> usize {
    match line_starts.binary_search(&byte) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
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
    fn inline_ignore_suppresses_on_that_line() {
        let src = "// ascript-ignore[syntax-error]\n@\n";
        let supp = suppressions(src);
        // Directive on line 0 covers its own line (and, by the documented
        // `line || line-1` rule, the following line so the next-line `@`
        // error is actually suppressed — verified end-to-end below).
        assert!(supp.suppressed_on_line(0, "syntax-error"));
        // A line the directive cannot reach is not suppressed.
        assert!(!supp.suppressed_on_line(2, "syntax-error"));
        // Unrelated codes are never suppressed by a scoped directive.
        assert!(!supp.suppressed_on_line(0, "some-other-code"));
        // End-to-end: the next-line `@` syntax error is suppressed.
        assert!(analyze(src).diagnostics.is_empty());
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
