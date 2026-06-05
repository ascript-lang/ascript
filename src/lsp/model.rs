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
