//! Formatter acceptance gates over the whole example corpus:
//! (1) every source comment survives formatting (no data loss — the original bug);
//! (2) formatting is idempotent (`fmt(fmt(x)) == fmt(x)`).

use std::fs;
use std::path::{Path, PathBuf};

fn corpus() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { walk(&p, out); }
            else if p.extension().and_then(|x| x.to_str()) == Some("as") { out.push(p); }
        }
    }
    let mut v = Vec::new();
    walk(Path::new("examples"), &mut v);
    v.sort();
    assert!(!v.is_empty());
    v
}

/// Multiset of comment texts in source (via the trivia-emitting lexer).
fn comments_of(src: &str) -> Vec<String> {
    use ascript::syntax::SyntaxKind;
    let mut v: Vec<String> = ascript::syntax::lex(src).into_iter()
        .filter(|t| matches!(t.kind, SyntaxKind::LineComment | SyntaxKind::BlockComment))
        .map(|t| t.text.trim_end().to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn every_comment_survives_formatting() {
    let mut failures = Vec::new();
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let before = comments_of(&src);
        if before.is_empty() { continue; }
        let formatted = ascript::syntax::format_tree(&src);
        let after = comments_of(&formatted);
        if before != after {
            failures.push(format!("{}: comments changed\n  before={before:?}\n  after ={after:?}", path.display()));
        }
    }
    assert!(failures.is_empty(), "comment preservation failures:\n{}", failures.join("\n\n"));
}

#[test]
fn formatting_is_idempotent_over_corpus() {
    let mut failures = Vec::new();
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let once = ascript::syntax::format_tree(&src);
        let twice = ascript::syntax::format_tree(&once);
        if once != twice {
            failures.push(format!("{} not idempotent", path.display()));
        }
    }
    assert!(failures.is_empty(), "idempotence failures:\n{}", failures.join("\n"));
}
