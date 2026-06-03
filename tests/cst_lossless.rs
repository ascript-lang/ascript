//! Every example program must round-trip through the new lexer byte-for-byte,
//! and the new lexer's non-trivia token count must match the legacy lexer's
//! token count (a differential guard that the two agree on tokenization).

use std::fs;
use std::path::Path;

fn as_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            as_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("as") {
            out.push(p);
        }
    }
}

fn corpus() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    as_files(Path::new("examples"), &mut v);
    v.sort();
    assert!(!v.is_empty(), "no example .as files found");
    v
}

#[test]
fn lexer_is_lossless_over_corpus() {
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let toks = ascript::syntax::lex(&src);
        let rendered = ascript::syntax::render(&toks);
        assert_eq!(rendered, src, "lexer not lossless for {}", path.display());
    }
}

#[test]
fn no_error_tokens_over_corpus() {
    use ascript::syntax::SyntaxKind;
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        for t in ascript::syntax::lex(&src) {
            assert_ne!(
                t.kind,
                SyntaxKind::Error,
                "unexpected Error token {:?} in {}",
                t.text,
                path.display()
            );
        }
    }
}

#[test]
fn flat_tree_is_lossless_over_corpus() {
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let node = ascript::syntax::build_flat_tree(&src);
        assert_eq!(node.text().to_string(), src, "tree not lossless for {}", path.display());
    }
}

#[test]
fn nontrivia_token_count_matches_legacy() {
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let legacy = ascript::lexer::lex(&src).expect("legacy lex");
        let legacy_count = legacy
            .iter()
            .filter(|t| !matches!(t.tok, ascript::token::Tok::Eof))
            .count();
        let new_count = ascript::syntax::lex(&src)
            .into_iter()
            .filter(|t| !t.kind.is_trivia())
            .count();
        assert_eq!(
            new_count, legacy_count,
            "token count mismatch for {} (new={}, legacy={})",
            path.display(), new_count, legacy_count
        );
    }
}

#[test]
fn structured_tree_is_lossless_over_corpus() {
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let node = ascript::syntax::parse_to_tree(&src);
        assert_eq!(
            node.text().to_string(),
            src,
            "structured tree not lossless for {}",
            path.display()
        );
    }
}
