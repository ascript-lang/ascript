//! Conformance test: the generated Tree-sitter grammar and the interpreter's
//! own parser must both accept every example program with no errors.

use std::fs;
use std::path::PathBuf;

// The generated parser exports `tree_sitter_ascript`, which returns a
// `const TSLanguage *`. `tree_sitter::Language` is a transparent wrapper over
// that pointer, so the extern can return it directly.
extern "C" {
    fn tree_sitter_ascript() -> tree_sitter::Language;
}

fn language() -> tree_sitter::Language {
    unsafe { tree_sitter_ascript() }
}

fn example_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in ["examples", "examples/modules", "examples/app"] {
        let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir}: {e}"));
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) == Some("as") {
                files.push(path);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no example .as files found");
    files
}

#[test]
fn treesitter_parses_all_examples_without_errors() {
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&lang)
        .expect("set_language should accept the generated ABI-14 parser");

    let mut failures = Vec::new();
    for path in example_files() {
        let src = fs::read_to_string(&path).unwrap();
        let tree = parser
            .parse(src.as_bytes(), None)
            .unwrap_or_else(|| panic!("tree-sitter failed to parse {}", path.display()));
        if tree.root_node().has_error() {
            failures.push(format!("{}", path.display()));
        }
    }

    assert!(
        failures.is_empty(),
        "tree-sitter reported error nodes in: {failures:?}"
    );
}

#[test]
fn treesitter_parses_match_guard_ending_in_ident() {
    // A match guard ending in a bare identifier right before `=>` must NOT be
    // mis-parsed as an arrow that swallows the arm separator. The grammar resolves
    // this via its declared pattern-vs-expression GLR conflict at the arm's `=>`,
    // so no regen is needed — this is a standing guard against regressions.
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    for src in [
        r#"let g = match v { n if n == lim => "eq", other => "o" }"#,
        r#"let g = match v { n if n > 0 && n == lim => "a", other => "o" }"#,
        r#"let g = match v { x if (() => true)() => 1, _ => 2 }"#,
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter error on guard snippet: {src}"
        );
    }
}

#[test]
fn treesitter_parses_inclusive_and_step_ranges() {
    // `..=` (inclusive) and a trailing contextual `step <expr>` must parse in
    // for-range, value, and match-pattern position — and `step` must stay an
    // ordinary identifier when not immediately following a range end. Mirrors the
    // hand-written parser's `parses_inclusive_and_step_ranges` test.
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    for src in [
        "for (i in 1..=5) {}",
        "let xs = 1..10 step 2",
        "let m = match n { 1..=10 => 1, _ => 0 }",
        "for (i in 10..1 step -2) {}",
        "let xs = 1..=5",
        // `step` is contextual, NOT reserved: usable as an ordinary identifier.
        "let step = 1",
        "fn step(n) { return n }",
        "let r = f(step)",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter error on range/step snippet: {src}"
        );
    }
}

#[test]
fn treesitter_parses_static_methods() {
    // `static` is a class-member modifier on `fn`/`async fn`/`fn*` (SP1 §3) and a
    // contextual soft keyword — usable as an ordinary identifier elsewhere.
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    for src in [
        "class C { static fn make() { return C() } }",
        "class C { static async fn create() { return C() } }",
        "class C { static fn* gen() { yield 1 } }",
        "class C { fn m() { return 1 }\n static fn s() { return 2 } }",
        // `static` is contextual, NOT reserved: usable as an ordinary identifier
        // everywhere except as a class-member modifier.
        "let static = 1",
        "fn static(n) { return n }",
        "let r = f(static)",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter error on static snippet: {src}"
        );
    }
}

#[test]
fn interpreter_parser_accepts_all_examples() {
    let mut failures = Vec::new();
    for path in example_files() {
        let src = fs::read_to_string(&path).unwrap();
        let result = ascript::lexer::lex(&src).and_then(|tokens| ascript::parser::parse(&tokens));
        if let Err(e) = result {
            failures.push(format!("{}: {e:?}", path.display()));
        }
    }

    assert!(
        failures.is_empty(),
        "interpreter parser rejected: {failures:?}"
    );
}
