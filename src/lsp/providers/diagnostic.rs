//! Pull diagnostics: `textDocument/diagnostic` (one document) and
//! `workspace/diagnostic` (project-wide). They return the SAME diagnostics as the
//! push path (`SemanticModel::lsp_diagnostics()`), so the editor sees one truth.

use crate::lsp::model::SemanticModel;
use tower_lsp::lsp_types::{
    Diagnostic, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    FullDocumentDiagnosticReport, RelatedFullDocumentDiagnosticReport,
};

/// The full document diagnostic report for `model` (config-aware, off the cache).
pub fn document_report(model: &SemanticModel) -> DocumentDiagnosticReportResult {
    let items: Vec<Diagnostic> = model.lsp_diagnostics();
    DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
        RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: None,
                items,
            },
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::LintConfig;

    #[test]
    fn document_report_matches_push_diagnostics() {
        let m = SemanticModel::build("let = 1\n".to_string(), None, &LintConfig::default());
        let DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(r)) =
            document_report(&m)
        else {
            panic!("expected a full report");
        };
        assert_eq!(r.full_document_diagnostic_report.items, m.lsp_diagnostics());
        assert!(!r.full_document_diagnostic_report.items.is_empty());
    }
}
