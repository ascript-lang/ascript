//! Neutral diagnostic model for `ascript check`.
//!
//! These types are deliberately free of any front-end / interpreter coupling so
//! the analysis core stays feature-independent and serde-free.

/// A byte-offset span into the source text (half-open: `[start, end)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,
}

impl From<cstree::text::TextRange> for ByteSpan {
    fn from(r: cstree::text::TextRange) -> Self {
        ByteSpan {
            start: usize::from(r.start()),
            end: usize::from(r.end()),
        }
    }
}

/// Diagnostic severity, ordered least-to-most severe for config promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Hint,
    Info,
    Warning,
    Error,
}

/// A single text replacement within a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: ByteSpan,
    pub replacement: String,
}

/// A named, applicable fix composed of one or more edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub title: String,
    pub edits: Vec<TextEdit>,
}

/// A diagnostic emitted by the analysis core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsDiagnostic {
    pub range: ByteSpan,
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub fix: Option<Fix>,
}

impl AsDiagnostic {
    /// Construct an error-severity diagnostic with no fix.
    pub fn error(code: &str, range: ByteSpan, message: impl Into<String>) -> Self {
        AsDiagnostic {
            range,
            severity: Severity::Error,
            code: code.to_string(),
            message: message.into(),
            fix: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_constructs() {
        let d = AsDiagnostic::error("syntax-error", ByteSpan { start: 3, end: 7 }, "boom");
        assert_eq!(d.code, "syntax-error");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.range, ByteSpan { start: 3, end: 7 });
        assert_eq!(d.message, "boom");
        assert!(d.fix.is_none());
        // Severity ordering used by config promotion.
        assert!(Severity::Error > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
        assert!(Severity::Info > Severity::Hint);
    }
}
