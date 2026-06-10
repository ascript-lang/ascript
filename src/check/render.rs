//! Rendering of diagnostics: human-readable (ariadne) and machine-readable JSON.

use crate::check::diagnostic::{AsDiagnostic, Severity};
use ariadne::{Label, Report, ReportKind, Source};

/// Render diagnostics as a human-readable report using ariadne.
pub fn human(path: &str, src: &str, diags: &[AsDiagnostic]) -> String {
    let mut buf: Vec<u8> = Vec::new();
    for d in diags {
        let kind = match d.severity {
            Severity::Error => ReportKind::Error,
            Severity::Warning => ReportKind::Warning,
            Severity::Info | Severity::Hint => ReportKind::Advice,
        };
        // The check core's `ByteSpan` carries genuine BYTE offsets; ariadne 0.6
        // renders in char-mode (`IndexType::Char` default), so convert byte→char
        // before building the range — otherwise a multibyte char before the span
        // blanks/shifts the caret.
        let start = byte_to_char(src, d.range.start);
        let end = byte_to_char(src, d.range.end);
        let mut out: Vec<u8> = Vec::new();
        let report = Report::build(kind, (path, start..end))
            .with_code(&d.code)
            .with_message(&d.message)
            .with_label(Label::new((path, start..end)).with_message(&d.message))
            .finish();
        // Writing to an in-memory buffer cannot fail in practice.
        let _ = report.write((path, Source::from(src)), &mut out);
        buf.extend_from_slice(&out);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Convert a BYTE offset into a CHAR offset within `src` (clamping a mid-codepoint
/// byte down to the largest char boundary `<= byte`). The check core speaks BYTE
/// offsets; the human renderer feeds char-mode ariadne, so it converts here. Mirrors
/// `crate::lsp::convert::byte_to_char` (kept local so this CORE renderer builds under
/// `--no-default-features`, where the LSP module is absent).
fn byte_to_char(src: &str, byte: usize) -> usize {
    let mut b = byte.min(src.len());
    while b > 0 && !src.is_char_boundary(b) {
        b -= 1;
    }
    src[..b].chars().count()
}

/// Render diagnostics as a JSON array (hand-rolled, serde-free).
pub fn json(path: &str, diags: &[AsDiagnostic]) -> String {
    let mut s = String::from("[");
    for (i, d) in diags.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let severity = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        };
        s.push_str("{\"path\":");
        s.push_str(&json_str(path));
        s.push_str(",\"code\":");
        s.push_str(&json_str(&d.code));
        s.push_str(",\"severity\":");
        s.push_str(&json_str(severity));
        s.push_str(",\"start\":");
        s.push_str(&d.range.start.to_string());
        s.push_str(",\"end\":");
        s.push_str(&d.range.end.to_string());
        s.push_str(",\"message\":");
        s.push_str(&json_str(&d.message));
        s.push('}');
    }
    s.push(']');
    s
}

/// Escape a string as a JSON string literal (with surrounding quotes).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::diagnostic::ByteSpan;

    #[test]
    fn json_shape() {
        let d = AsDiagnostic::error(
            "syntax-error",
            ByteSpan { start: 0, end: 1 },
            "boom \"quoted\"\nnext",
        );
        let out = json("f.as", &[d]);
        assert!(out.starts_with('['));
        assert!(out.ends_with(']'));
        assert!(out.contains("\"code\":\"syntax-error\""));
        assert!(out.contains("\"severity\":\"error\""));
        // Message is JSON-escaped.
        assert!(out.contains("\\\"quoted\\\""));
        assert!(out.contains("\\n"));
    }

    #[test]
    fn human_points_at_source() {
        let src = "boom\n";
        let d = AsDiagnostic::error("syntax-error", ByteSpan { start: 0, end: 4 }, "boom");
        let out = human("f.as", src, &[d]);
        assert!(out.contains("boom"), "output: {out}");
        assert!(out.contains("syntax-error"), "output: {out}");
    }
}
