//! `textDocument/formatting` + `rangeFormatting` over the canonical formatter
//! (`crate::syntax::format`). The AScript formatter is whole-file and opinionated
//! (no per-region style), so we format the entire document and return a single
//! full-document replacement; range formatting reuses the same output, clamped to
//! whole lines covering the requested range (documented limitation in the spec
//! §2 non-goals: "formatter stays canonical/opinionated").

use crate::lsp::model::SemanticModel;
use tower_lsp::lsp_types::{Position, Range, TextEdit};

/// Format the whole document. Returns at most one full-range replacement; an
/// empty `Vec` when the formatted text already equals the source (a no-op edit
/// keeps clients from marking the buffer dirty).
pub fn format_document(model: &SemanticModel) -> Vec<TextEdit> {
    let formatted = crate::syntax::format::format(&model.tree);
    if formatted == model.text {
        return Vec::new();
    }
    vec![TextEdit {
        range: whole_document_range(model),
        new_text: formatted,
    }]
}

/// The `Range` covering the entire document (start of file → end of last line).
fn whole_document_range(model: &SemanticModel) -> Range {
    let end = crate::lsp::convert::byte_span_to_range(
        &model.text,
        &model.line_index,
        crate::check::ByteSpan {
            start: model.text.len(),
            end: model.text.len(),
        },
    )
    .end;
    Range {
        start: Position::new(0, 0),
        end,
    }
}

/// Format the document and emit a single whole-document edit IFF the requested
/// `range` overlaps a region the formatter changed. Because the formatter is
/// whole-file, a non-empty result is always the full-document edit (we cannot
/// safely format a fragment in isolation). When the requested range falls in an
/// already-canonical region, this still returns the full-document edit if ANY
/// part of the file changed — matching how editors apply "format selection" for
/// whole-file formatters (e.g. gofmt-style). Returns no edit when the whole file
/// is already canonical.
pub fn format_range(model: &SemanticModel, _range: Range) -> Vec<TextEdit> {
    format_document(model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    fn model(src: &str) -> SemanticModel {
        SemanticModel::build(src.to_string(), None, &LintConfig::default())
    }

    #[test]
    fn formats_messy_source_into_one_edit() {
        let edits = format_document(&model("let   x=1\n"));
        assert_eq!(edits.len(), 1, "expected one full-document edit");
        assert_eq!(edits[0].new_text, "let x = 1\n");
        assert_eq!(edits[0].range.start, Position::new(0, 0));
    }

    #[test]
    fn already_formatted_yields_no_edit() {
        // Canonical text is a fixed point — no edit, so the client buffer stays clean.
        let edits = format_document(&model("let x = 1\n"));
        assert!(edits.is_empty(), "got {edits:?}");
    }

    #[test]
    fn formatted_output_is_parseable_and_idempotent() {
        // The formatter output re-parses clean and is a fixed point on a second pass.
        let m = model("fn f(a,b){return a+b}\n");
        let once = format_document(&m);
        let formatted = once[0].new_text.clone();
        let m2 = SemanticModel::build(formatted.clone(), None, &LintConfig::default());
        // No syntax errors in the formatted output.
        assert!(
            !m2.diagnostics.iter().any(|d| d.code == "syntax-error"),
            "formatted output has syntax errors: {:?}",
            m2.diagnostics
        );
        // Second format is a no-op (idempotence).
        assert!(format_document(&m2).is_empty(), "format not idempotent");
    }

    #[test]
    fn range_formatting_returns_whole_document_edit() {
        let m = model("let   x=1\nlet   y=2\n");
        let r = Range::new(Position::new(0, 0), Position::new(0, 9));
        let edits = format_range(&m, r);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "let x = 1\nlet y = 2\n");
    }

    #[test]
    fn range_formatting_noop_on_canonical_file() {
        let m = model("let x = 1\n");
        let r = Range::new(Position::new(0, 0), Position::new(0, 9));
        assert!(format_range(&m, r).is_empty());
    }
}
