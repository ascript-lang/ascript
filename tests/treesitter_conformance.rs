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
    for dir in ["examples", "examples/modules"] {
        let entries = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read_dir {dir}: {e}"));
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
fn interpreter_parser_accepts_all_examples() {
    let mut failures = Vec::new();
    for path in example_files() {
        let src = fs::read_to_string(&path).unwrap();
        let result = ascript::lexer::lex(&src)
            .and_then(|tokens| ascript::parser::parse(&tokens));
        if let Err(e) = result {
            failures.push(format!("{}: {e:?}", path.display()));
        }
    }

    assert!(
        failures.is_empty(),
        "interpreter parser rejected: {failures:?}"
    );
}
