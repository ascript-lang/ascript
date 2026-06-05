//! Phase 2 gates: semantic-token classification + inlay hints produce NO crashes
//! and no contradictory output over the whole `examples/**` corpus, and the four
//! new capabilities are advertised.
//!
//! Gated on the `lsp` feature; under `--no-default-features` the whole `lsp`
//! module (and these providers) compiles out, so the file is empty there.

#![cfg(feature = "lsp")]

use ascript::check::LintConfig;
use ascript::lsp::model::SemanticModel;
use ascript::lsp::providers::{inlay, semantic_tokens, signature};
use ascript::lsp::server::server_capabilities;
use tower_lsp::lsp_types::{Position, Range};

fn corpus_files() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for dir in ["examples", "examples/advanced"] {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("as") {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn semantic_tokens_classify_every_corpus_file_without_panic() {
    for path in corpus_files() {
        let src = std::fs::read_to_string(&path).unwrap();
        let m = SemanticModel::build(src.clone(), None, &LintConfig::default());
        let st = semantic_tokens::semantic_tokens_full(&m);
        // Every emitted token's length is non-zero and its type is in legend range.
        let legend_len = semantic_tokens::legend().token_types.len() as u32;
        for t in &st.data {
            assert!(t.length > 0, "{}: zero-length token", path.display());
            assert!(
                t.token_type < legend_len,
                "{}: type out of legend",
                path.display()
            );
        }
    }
}

#[test]
fn inlay_hints_are_consistent_over_the_corpus() {
    for path in corpus_files() {
        let src = std::fs::read_to_string(&path).unwrap();
        let m = SemanticModel::build(src.clone(), None, &LintConfig::default());
        let end = m.line_index.position(m.text.chars().count());
        let hints = inlay::inlay_hints(&m, Range::new(Position::new(0, 0), end));
        // No two hints occupy the exact same position with conflicting labels.
        let mut seen: std::collections::HashMap<(u32, u32), String> =
            std::collections::HashMap::new();
        for h in &hints {
            let key = (h.position.line, h.position.character);
            if let tower_lsp::lsp_types::InlayHintLabel::String(s) = &h.label {
                if let Some(prev) = seen.get(&key) {
                    assert_eq!(
                        prev,
                        s,
                        "{}: contradictory inlay hints at {key:?}",
                        path.display()
                    );
                } else {
                    seen.insert(key, s.clone());
                }
            }
        }
    }
}

#[test]
fn signature_help_never_panics_over_corpus_offsets() {
    for path in corpus_files() {
        let src = std::fs::read_to_string(&path).unwrap();
        let m = SemanticModel::build(src.clone(), None, &LintConfig::default());
        // Probe every byte offset that is a char boundary; must not panic.
        for (off, _) in src.char_indices() {
            let _ = signature::signature_help(&m, off);
        }
    }
}

#[test]
fn phase2_capabilities_advertised() {
    let caps = server_capabilities();
    assert!(caps.semantic_tokens_provider.is_some());
    assert!(caps.inlay_hint_provider.is_some());
    assert!(caps.document_highlight_provider.is_some());
    assert!(caps.signature_help_provider.is_some());
}
