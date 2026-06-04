//! Formatter acceptance gates over the whole example corpus:
//! (1) every source comment survives formatting (no data loss — the original bug);
//! (2) formatting is idempotent (`fmt(fmt(x)) == fmt(x)`).

use std::fs;
use std::path::{Path, PathBuf};

fn corpus() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("as") {
                out.push(p);
            }
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
    let mut v: Vec<String> = ascript::syntax::lex(src)
        .into_iter()
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
        if before.is_empty() {
            continue;
        }
        let formatted = ascript::syntax::format_tree(&src);
        let after = comments_of(&formatted);
        if before != after {
            failures.push(format!(
                "{}: comments changed\n  before={before:?}\n  after ={after:?}",
                path.display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "comment preservation failures:\n{}",
        failures.join("\n\n")
    );
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
    assert!(
        failures.is_empty(),
        "idempotence failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn cli_formatter_preserves_comments_end_to_end() {
    // The library entry the CLI uses keeps comments — the original bug, fixed.
    let src = "let x = 1 // keep me\n";
    assert_eq!(ascript::syntax::format_tree(src), "let x = 1 // keep me\n");
}

#[test]
fn formats_inclusive_and_step_ranges() {
    // The CST formatter must preserve `..=` (inclusive) and the trailing
    // contextual `step <expr>` in both value and for-range position, and stay
    // idempotent. Regression for the formatter silently dropping `step`.
    for (src, want) in [
        ("let xs = 1..=10 step 2\n", "let xs = 1..=10 step 2\n"),
        ("let ys = 1..10 step 2\n", "let ys = 1..10 step 2\n"),
        ("let zs = 1..=5\n", "let zs = 1..=5\n"),
        ("for (i in 1..=5) {\n}\n", "for (i in 1..=5) {\n}\n"),
        (
            "for (i in 10..1 step -2) {\n}\n",
            "for (i in 10..1 step -2) {\n}\n",
        ),
        // `step` stays an ordinary identifier away from a range end.
        ("let step = 1\n", "let step = 1\n"),
    ] {
        let once = ascript::syntax::format_tree(src);
        assert_eq!(once, want, "unexpected format for {src:?}");
        let twice = ascript::syntax::format_tree(&once);
        assert_eq!(once, twice, "not idempotent for {src:?}");
        assert!(
            ascript::syntax::parser::parse(&once).errors.is_empty(),
            "formatted range/step output does not reparse: {once:?}"
        );
    }
}

#[test]
fn formats_static_methods() {
    // SP1 §3: `static fn` / `static async fn` / `static fn*` format with the
    // `static` modifier first, then `async`, then `fn`; statics sit with the other
    // methods (after fields); idempotent + reparses.
    for (src, want) in [
        (
            "class C { static fn make() { return C() } }\n",
            "class C {\n  static fn make() {\n    return C()\n  }\n}\n",
        ),
        (
            "class C { static async fn create() { return C() } }\n",
            "class C {\n  static async fn create() {\n    return C()\n  }\n}\n",
        ),
        (
            "class C { static fn* gen() { yield 1 } }\n",
            "class C {\n  static fn* gen() {\n    yield 1\n  }\n}\n",
        ),
        // fields before methods; a static method sits with the instance methods.
        (
            "class C { static fn s() { return 1 }\n x: number = 0\n fn m() { return self.x } }\n",
            "class C {\n  x: number = 0\n  static fn s() {\n    return 1\n  }\n  fn m() {\n    return self.x\n  }\n}\n",
        ),
    ] {
        let once = ascript::syntax::format_tree(src);
        assert_eq!(once, want, "unexpected format for {src:?}");
        let twice = ascript::syntax::format_tree(&once);
        assert_eq!(once, twice, "not idempotent for {src:?}");
        assert!(
            ascript::syntax::parser::parse(&once).errors.is_empty(),
            "formatted static-method output does not reparse: {once:?}"
        );
    }
}

#[test]
fn formats_nil_type_idempotently() {
    // `nil` as a type formats via the NamedType path (first non-trivia token
    // text) and round-trips. Regression for the missing `NilKw` arm in the CST
    // type parser.
    for src in [
        "fn f(): nil {\n}\n",
        "let x: nil = nil\n",
        "fn g(): number | nil {\n  return nil\n}\n",
    ] {
        let once = ascript::syntax::format_tree(src);
        assert!(
            once.contains("nil"),
            "lost `nil` type formatting {src:?} -> {once:?}"
        );
        let twice = ascript::syntax::format_tree(&once);
        assert_eq!(once, twice, "not idempotent for {src:?}");
        assert!(
            ascript::syntax::parser::parse(&once).errors.is_empty(),
            "formatted `nil`-type output does not reparse: {once:?}"
        );
    }
}

#[test]
fn formats_fn_type_idempotently() {
    // `fn` as a type formats via the NamedType path (first non-trivia token text)
    // and round-trips. Regression for the missing `FnKw` arm in the CST type parser.
    for src in [
        "let f: fn = g\n",
        "fn apply(g: fn, x) {\n  return g(x)\n}\n",
        "fn h(): fn {\n  return g\n}\n",
    ] {
        let once = ascript::syntax::format_tree(src);
        assert!(
            once.contains("fn"),
            "lost `fn` type formatting {src:?} -> {once:?}"
        );
        let twice = ascript::syntax::format_tree(&once);
        assert_eq!(once, twice, "not idempotent for {src:?}");
        assert!(
            ascript::syntax::parser::parse(&once).errors.is_empty(),
            "formatted `fn`-type output does not reparse: {once:?}"
        );
    }
}

#[test]
fn formatted_corpus_reparses_without_errors() {
    let mut failures = Vec::new();
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        // only check files that parse cleanly to begin with
        if !ascript::syntax::parser::parse(&src).errors.is_empty() {
            continue;
        }
        let formatted = ascript::syntax::format_tree(&src);
        let errs = ascript::syntax::parser::parse(&formatted).errors;
        if !errs.is_empty() {
            failures.push(format!(
                "{}: formatted output has {} parse error(s) (content loss?): {:?}",
                path.display(),
                errs.len(),
                errs
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "formatter produced unparseable output:\n{}",
        failures.join("\n")
    );
}
