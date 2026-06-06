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
    /// The monotone generation this model was built at, stamped by
    /// `DocumentStore::set_versioned` (0 for a model built directly via `build`).
    /// Handlers capture it at entry to detect supersession by a newer edit.
    pub generation: u64,
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
        SemanticModel {
            text,
            version,
            tree,
            resolved,
            diagnostics,
            tokens,
            line_index,
            generation: 0,
        }
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

use crate::lsp::perf::{HUGE_FILE_BYTES, LARGE_FILE_BYTES};

/// How expensive providers should treat a document, by source size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeClass {
    /// Normal: every provider runs at full fidelity.
    Normal,
    /// Large: semantic-tokens FULL degrades to range-only; inlay hints skipped.
    Large,
    /// Huge: only diagnostics + navigation; token/inlay/folding/color return empty.
    Huge,
}

impl SemanticModel {
    /// The document's size class (drives provider degradation).
    pub fn size_class(&self) -> SizeClass {
        match self.text.len() {
            n if n >= HUGE_FILE_BYTES => SizeClass::Huge,
            n if n >= LARGE_FILE_BYTES => SizeClass::Large,
            _ => SizeClass::Normal,
        }
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn small_file_is_normal() {
        let m = SemanticModel::build("let x = 1\n".to_string(), None, &LintConfig::default());
        assert_eq!(m.size_class(), SizeClass::Normal);
    }

    #[test]
    fn threshold_classifies_large_and_huge() {
        let large = "a".repeat(LARGE_FILE_BYTES);
        let m = SemanticModel::build(large, None, &LintConfig::default());
        assert_eq!(m.size_class(), SizeClass::Large);

        let huge = "a".repeat(HUGE_FILE_BYTES);
        let m = SemanticModel::build(huge, None, &LintConfig::default());
        assert_eq!(m.size_class(), SizeClass::Huge);
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
    /// Monotone generation counter, bumped on every `set_versioned`. The stored
    /// model carries the generation it was built at, so a handler that captured a
    /// generation can detect that a newer edit superseded it.
    next_gen: u64,
}

impl DocumentStore {
    pub fn new() -> Self {
        DocumentStore { models: HashMap::new(), next_gen: 0 }
    }

    /// Build + store the model for `uri`, stamping it with a fresh monotone
    /// generation, which is returned so the caller can later detect supersession.
    /// The lint config is discovered from the document's filesystem path (nearest
    /// `ascript.toml`); a non-file URL uses the default config.
    pub fn set_versioned(&mut self, uri: Url, text: String, version: Option<i32>) -> u64 {
        self.next_gen += 1;
        let gen = self.next_gen;
        let config = config_for_uri(&uri);
        let mut model = SemanticModel::build(text, version, &config);
        model.generation = gen;
        self.models.insert(uri, model);
        gen
    }

    /// Build and store the model for `uri` at `text`/`version`, discarding the
    /// generation (back-compat shim for callers that do not track supersession).
    pub fn set(&mut self, uri: Url, text: String, version: Option<i32>) {
        let _ = self.set_versioned(uri, text, version);
    }

    /// The generation currently stored for `uri` (`None` if not open).
    pub fn current_gen(&self, uri: &Url) -> Option<u64> {
        self.models.get(uri).map(|m| m.generation)
    }

    pub fn get(&self, uri: &Url) -> Option<&SemanticModel> {
        self.models.get(uri)
    }

    pub fn remove(&mut self, uri: &Url) {
        self.models.remove(uri);
    }

    /// The URIs of every currently-open document (republish on config change).
    pub fn uris(&self) -> Vec<Url> {
        self.models.keys().cloned().collect()
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

#[cfg(test)]
mod gen_tests {
    use super::*;

    #[test]
    fn set_bumps_generation_monotonically() {
        let mut store = DocumentStore::new();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        let g1 = store.set_versioned(uri.clone(), "let x = 1\n".to_string(), Some(1));
        let g2 = store.set_versioned(uri.clone(), "let x = 2\n".to_string(), Some(2));
        assert!(g2 > g1, "generation must increase per edit: {g1} -> {g2}");
        assert_eq!(store.current_gen(&uri), Some(g2));
        // The stored model carries its generation.
        assert_eq!(store.get(&uri).unwrap().generation, g2);
    }

    #[test]
    fn stale_generation_is_detectable() {
        let mut store = DocumentStore::new();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        let g1 = store.set_versioned(uri.clone(), "let x = 1\n".to_string(), Some(1));
        let _g2 = store.set_versioned(uri.clone(), "let x = 2\n".to_string(), Some(2));
        // A handler holding g1 sees it is no longer current.
        assert!(store.current_gen(&uri) != Some(g1), "g1 should be stale");
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

    #[test]
    fn coalesced_edits_fold_forward() {
        // Two ranged inserts applied in sequence equal one apply over the folded text
        // — proving a debounce that only analyzes the LATEST folded text is correct.
        let start = "let x = 1\n";
        let e1 = TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(0, 8), Position::new(0, 9))),
            range_length: None,
            text: "2".to_string(),
        };
        let after1 = apply_changes(start, &[e1]);
        let e2 = TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(0, 8), Position::new(0, 9))),
            range_length: None,
            text: "3".to_string(),
        };
        let after2 = apply_changes(&after1, &[e2]);
        assert_eq!(after2, "let x = 3\n");
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
