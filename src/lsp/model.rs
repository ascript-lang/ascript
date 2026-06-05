//! The per-document semantic model: the single parsed/resolved artifact every
//! LSP provider reads. Built once per document version. Holds only owned,
//! `Send + Sync` data (CST + resolver result + diagnostics + tokens) — never a
//! `Value`/`Rc`/`RefCell`, so the backend stays `Send + Sync`.

use crate::check::{AsDiagnostic, LintConfig};
use crate::lsp::line_index::LineIndex;
use crate::syntax::cst::ResolvedNode;
use crate::syntax::lexer::LexToken;
use crate::syntax::resolve::types::ResolveResult;

/// Everything the LSP knows about one document at one version.
pub struct SemanticModel {
    pub text: String,
    pub version: Option<i32>,
    pub tree: ResolvedNode,
    pub resolved: ResolveResult,
    pub diagnostics: Vec<AsDiagnostic>,
    pub tokens: Vec<LexToken>,
    pub line_index: LineIndex,
}

impl SemanticModel {
    /// Build the model for `text`, applying `config` to diagnostic severities.
    pub fn build(text: String, version: Option<i32>, config: &LintConfig) -> Self {
        let parsed = crate::syntax::parser::parse(&text);
        let tree = crate::syntax::tree_builder::build_tree(parsed);
        let resolved = crate::syntax::resolve::resolve(&tree);
        let diagnostics = crate::check::analyze::analyze_with_config(&text, config).diagnostics;
        let tokens: Vec<LexToken> = crate::syntax::lex(&text);
        let line_index = LineIndex::new(&text);
        SemanticModel { text, version, tree, resolved, diagnostics, tokens, line_index }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_clean_program_has_no_diagnostics() {
        let m = SemanticModel::build("let x = 1\nprint(x)\n".to_string(), Some(1), &LintConfig::default());
        assert!(m.diagnostics.is_empty(), "got {:?}", m.diagnostics);
        assert!(!m.tokens.is_empty());
    }

    #[test]
    fn build_matches_analyze_with_config() {
        let src = "let = 1\nlet = 2\n";
        let m = SemanticModel::build(src.to_string(), None, &LintConfig::default());
        let direct = crate::check::analyze::analyze_with_config(src, &LintConfig::default()).diagnostics;
        assert_eq!(m.diagnostics, direct);
    }
}

use std::collections::HashMap;
use std::path::Path;
use tower_lsp::lsp_types::Url;

/// The per-document model cache. Keyed by `Url`; holds at most one model per open
/// document. Rebuilds on insert (full or post-incremental text).
#[derive(Default)]
pub struct DocumentStore {
    models: HashMap<Url, SemanticModel>,
}

impl DocumentStore {
    pub fn new() -> Self {
        DocumentStore { models: HashMap::new() }
    }

    /// Build and store the model for `uri` at `text`/`version`. The lint config is
    /// discovered from the document's filesystem path (nearest `ascript.toml`); a
    /// non-file URL uses the default config.
    pub fn set(&mut self, uri: Url, text: String, version: Option<i32>) {
        let config = config_for_uri(&uri);
        let model = SemanticModel::build(text, version, &config);
        self.models.insert(uri, model);
    }

    pub fn get(&self, uri: &Url) -> Option<&SemanticModel> {
        self.models.get(uri)
    }

    pub fn remove(&mut self, uri: &Url) {
        self.models.remove(uri);
    }
}

/// Discover the lint config for a document URL (nearest `ascript.toml [lint]`).
fn config_for_uri(uri: &Url) -> LintConfig {
    if let Ok(path) = uri.to_file_path() {
        return config_for_path(&path);
    }
    LintConfig::default()
}

/// Discover the lint config for a filesystem path.
pub fn config_for_path(path: &Path) -> LintConfig {
    crate::check::config_toml::config_for_file(path).unwrap_or_default()
}

#[cfg(test)]
mod store_tests {
    use super::*;

    #[test]
    fn set_get_remove_roundtrip() {
        let mut store = DocumentStore::new();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        store.set(uri.clone(), "let x = 1\n".to_string(), Some(1));
        assert!(store.get(&uri).is_some());
        assert_eq!(store.get(&uri).unwrap().version, Some(1));
        store.remove(&uri);
        assert!(store.get(&uri).is_none());
    }

    #[test]
    fn set_rebuilds_on_new_version() {
        let mut store = DocumentStore::new();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        store.set(uri.clone(), "let x = 1\n".to_string(), Some(1));
        store.set(uri.clone(), "let = bad\n".to_string(), Some(2));
        let m = store.get(&uri).unwrap();
        assert_eq!(m.version, Some(2));
        assert!(!m.diagnostics.is_empty(), "v2 has a syntax error");
    }
}

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString};

impl SemanticModel {
    /// The model's diagnostics as LSP `Diagnostic`s (severity + code + range).
    pub fn lsp_diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .map(|d| Diagnostic {
                range: crate::lsp::convert::byte_span_to_range(&self.text, &self.line_index, d.range),
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
            })
            .collect()
    }
}

use tower_lsp::lsp_types::TextDocumentContentChangeEvent;

/// Apply LSP incremental content changes to `text` in order, returning the new
/// text. A change with `range == None` replaces the whole document. Ranges are
/// UTF-16 line/char positions resolved against the CURRENT text before each edit.
pub fn apply_changes(text: &str, changes: &[TextDocumentContentChangeEvent]) -> String {
    let mut out = text.to_string();
    for change in changes {
        match change.range {
            None => out = change.text.clone(),
            Some(range) => {
                let index = LineIndex::new(&out);
                let start = char_offset(&out, index.offset(range.start));
                let end = char_offset(&out, index.offset(range.end));
                // A protocol-violating client may send an inverted range (end before
                // start); `replace_range` would panic, so skip such a change and leave
                // the text unchanged rather than crash the server.
                if start > end {
                    continue;
                }
                out.replace_range(start..end, &change.text);
            }
        }
    }
    out
}

/// Byte offset of char offset `chars` in `s`.
fn char_offset(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(b, _)| b).unwrap_or(s.len())
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    use tower_lsp::lsp_types::{Position, Range};

    #[test]
    fn incremental_edits_equal_full_reparse() {
        // Start, then apply a ranged insert, and assert the text equals what a
        // client would have sent as a full document.
        let start = "let x = 1\nprint(x)\n";
        let change = TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(0, 8), Position::new(0, 9))), // the "1"
            range_length: None,
            text: "42".to_string(),
        };
        let got = apply_changes(start, &[change]);
        assert_eq!(got, "let x = 42\nprint(x)\n");
        // And a model built from the incremental result equals one built from the
        // equivalent full text (diagnostics identical).
        let inc = SemanticModel::build(got.clone(), Some(2), &LintConfig::default());
        let full = SemanticModel::build("let x = 42\nprint(x)\n".to_string(), Some(2), &LintConfig::default());
        assert_eq!(inc.diagnostics, full.diagnostics);
    }

    #[test]
    fn inverted_range_is_skipped_not_panicked() {
        // A protocol-violating client sends a range whose end PRECEDES its start.
        // `replace_range(start..end)` would panic; the guard must skip the change,
        // leaving the document unchanged.
        let start = "let x = 1\n";
        let change = TextDocumentContentChangeEvent {
            // end (0,2) is before start (0,8) → inverted.
            range: Some(Range::new(Position::new(0, 8), Position::new(0, 2))),
            range_length: None,
            text: "BOOM".to_string(),
        };
        let got = apply_changes(start, &[change]);
        assert_eq!(got, start, "inverted-range change is skipped, text unchanged");
    }

    #[test]
    fn full_replace_change_replaces() {
        let change = TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "new".to_string(),
        };
        assert_eq!(apply_changes("old", &[change]), "new");
    }
}

#[cfg(test)]
mod diag_tests {
    use super::*;

    #[test]
    fn lsp_diagnostics_carry_code_and_severity() {
        let m = SemanticModel::build("let = 1\n".to_string(), None, &LintConfig::default());
        let ds = m.lsp_diagnostics();
        assert!(!ds.is_empty());
        let d = &ds[0];
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert!(matches!(&d.code, Some(NumberOrString::String(c)) if c == "syntax-error"));
    }
}
