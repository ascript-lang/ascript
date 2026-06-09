//! Coordinate conversion for the LSP: the analysis core speaks BYTE offsets,
//! LSP speaks UTF-16 line/character `Position`s. All conversion lives here.

use crate::check::ByteSpan;
use crate::lsp::line_index::LineIndex;
use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticSeverity, NumberOrString, Range,
};

/// Convert a byte offset to a char offset (the char-based `LineIndex` then maps
/// char→`Position`). Clamps to the largest char boundary `<= byte` so a
/// mid-codepoint byte never panics.
pub fn byte_to_char(src: &str, byte: usize) -> usize {
    let mut b = byte.min(src.len());
    while b > 0 && !src.is_char_boundary(b) {
        b -= 1;
    }
    src[..b].chars().count()
}

/// Convert a char offset to a byte offset into `src`. Clamps an out-of-range
/// char offset to the end of `src` so the result is always a valid byte index.
pub fn char_to_byte(src: &str, char_off: usize) -> usize {
    src.char_indices()
        .nth(char_off)
        .map(|(b, _)| b)
        .unwrap_or(src.len())
}

/// Convert a byte-offset `ByteSpan` to an LSP `Range`.
pub fn byte_span_to_range(src: &str, index: &LineIndex, span: ByteSpan) -> Range {
    Range {
        start: index.position(byte_to_char(src, span.start)),
        end: index.position(byte_to_char(src, span.end)),
    }
}

/// Convert one neutral `AsDiagnostic` (byte ranges) into an LSP `Diagnostic`.
/// Shared by `SemanticModel::lsp_diagnostics` and the index-backed file-module
/// call-arity merge in the server (SP4 §4 D-arity).
pub fn byte_diagnostic_to_lsp(text: &str, d: &crate::check::AsDiagnostic) -> Diagnostic {
    let index = LineIndex::new(text);
    Diagnostic {
        range: byte_span_to_range(text, &index, d.range),
        severity: Some(match d.severity {
            crate::check::Severity::Error => DiagnosticSeverity::ERROR,
            crate::check::Severity::Warning => DiagnosticSeverity::WARNING,
            crate::check::Severity::Info => DiagnosticSeverity::INFORMATION,
            crate::check::Severity::Hint => DiagnosticSeverity::HINT,
        }),
        code: Some(NumberOrString::String(d.code.clone())),
        source: Some("ascript".to_string()),
        message: d.message.clone(),
        ..Diagnostic::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::Position;

    #[test]
    fn byte_to_char_handles_multibyte() {
        let src = "héllo";
        assert_eq!(byte_to_char(src, 0), 0);
        assert_eq!(byte_to_char(src, 3), 2);
        assert_eq!(byte_to_char(src, 2), 1);
        assert_eq!(byte_to_char(src, 999), 5);
    }

    #[test]
    fn char_to_byte_handles_multibyte() {
        // "héllo": char 2 is 'l', which begins at byte 3 (é is 2 bytes).
        let src = "héllo";
        assert_eq!(char_to_byte(src, 0), 0);
        assert_eq!(char_to_byte(src, 2), 3);
        // Out-of-range clamps to the byte length.
        assert_eq!(char_to_byte(src, 999), src.len());
    }

    #[test]
    fn byte_diagnostic_to_lsp_carries_code_and_severity() {
        let analysis = crate::check::analyze("let = 1\n");
        let d = &analysis.diagnostics[0];
        let lsp = byte_diagnostic_to_lsp("let = 1\n", d);
        assert_eq!(lsp.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(lsp.source.as_deref(), Some("ascript"));
        assert!(matches!(&lsp.code, Some(NumberOrString::String(c)) if c == "syntax-error"));
    }

    #[test]
    fn blocking_generic_type_mismatch_flows_to_lsp_as_error() {
        // TYPE Task 16: a generic call with an EXPLICIT type arg that conflicts with
        // the argument (`id<string>(5)`) is a BLOCKING `type-mismatch`. It must reach
        // the LSP as an ERROR-severity diagnostic carrying the `type-mismatch` code
        // (the Task-1 severity flip surfaced in the editor).
        let src = "fn id<T>(x: T): T { return x }\nlet r = id<string>(5)\n";
        let analysis = crate::check::analyze(src);
        let hit = analysis
            .diagnostics
            .iter()
            .find(|d| d.code == "type-mismatch")
            .expect("a type-mismatch diagnostic");
        let lsp = byte_diagnostic_to_lsp(src, hit);
        assert_eq!(lsp.severity, Some(DiagnosticSeverity::ERROR));
        assert!(matches!(&lsp.code, Some(NumberOrString::String(c)) if c == "type-mismatch"));
    }

    /// Phase 0 GATE: the LSP must not reference the legacy interpreter front-end.
    /// This guards the unification — any re-introduction of the legacy
    /// ast/lexer/parser/token paths anywhere under `src/lsp/` fails the build.
    ///
    /// The needles are built by concatenation so THIS guard file does not itself
    /// contain the literal banned substrings (which would be a self-false-positive).
    #[test]
    fn lsp_does_not_import_legacy_frontend() {
        let lsp_dir = format!("{}/src/lsp", env!("CARGO_MANIFEST_DIR"));
        let prefix = "crate::";
        let banned = [
            format!("{prefix}ast"),
            format!("{prefix}lexer"),
            format!("{prefix}parser::"),
            format!("{prefix}token"),
        ];
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        collect_rs(std::path::Path::new(&lsp_dir), &mut files);
        assert!(!files.is_empty(), "expected to find .rs files under src/lsp");
        // Phase 1: the three new editing-essentials providers MUST be in scope of
        // this scan so they can never re-introduce a legacy front-end import.
        for required in [
            "providers/formatting.rs",
            "providers/completion.rs",
            "providers/code_action.rs",
            // Phase 2: the semantic-visualization providers must also be in scope so
            // they can never re-introduce a legacy front-end import.
            "providers/token_spans.rs",
            "providers/semantic_tokens.rs",
            "providers/highlight.rs",
            "providers/signature.rs",
            "providers/inlay.rs",
            // Phase 3: the navigation + structure-depth providers must also be in
            // scope so they can never re-introduce a legacy front-end import.
            "providers/navigation.rs",
            "providers/folding.rs",
            "providers/hierarchy.rs",
            "providers/symbols.rs",
        ] {
            let want = std::path::Path::new(&lsp_dir).join(required);
            assert!(
                files.iter().any(|f| f == &want),
                "guard scan must cover {required}"
            );
        }
        for path in files {
            let src = std::fs::read_to_string(&path).unwrap_or_default();
            for b in &banned {
                assert!(
                    !src.contains(b.as_str()),
                    "{} still references legacy {b}",
                    path.display()
                );
            }
        }
    }

    /// Recursively collect every `.rs` file under `dir`.
    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn byte_span_to_range_maps_endpoints() {
        let src = "let x = 1\nprint(x)\n";
        let index = LineIndex::new(src);
        let r = byte_span_to_range(src, &index, ByteSpan { start: 10, end: 15 });
        assert_eq!(r.start, Position::new(1, 0));
        assert_eq!(r.end, Position::new(1, 5));
    }
}
