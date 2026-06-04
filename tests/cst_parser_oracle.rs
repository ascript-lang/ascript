//! Differential oracle: the new parser's "no errors" verdict must match the
//! legacy parser's acceptance for core-slice snippets AND for the full
//! examples/**/*.as corpus. Guards that the hand-written CST parser and the
//! language grammar stay in lock-step.

use std::fs;
use std::path::{Path, PathBuf};

fn as_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            as_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("as") {
            out.push(p);
        }
    }
}

fn corpus() -> Vec<PathBuf> {
    let mut v = Vec::new();
    as_files(Path::new("examples"), &mut v);
    v.sort();
    assert!(!v.is_empty());
    v
}

#[test]
fn new_parser_accepts_entire_corpus() {
    let mut failures = Vec::new();
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let p = ascript::syntax::parser::parse(&src);
        if !p.errors.is_empty() {
            failures.push(format!("{}: {:?}", path.display(), p.errors));
        }
    }
    assert!(
        failures.is_empty(),
        "new parser rejected files:\n{}",
        failures.join("\n")
    );
}

#[test]
fn new_parser_agrees_with_legacy_over_corpus() {
    let mut failures = Vec::new();
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let new_ok = ascript::syntax::parser::parse(&src).errors.is_empty();
        let legacy_ok = match ascript::lexer::lex(&src) {
            Ok(toks) => ascript::parser::parse(&toks).is_ok(),
            Err(_) => false,
        };
        if new_ok != legacy_ok {
            failures.push(format!(
                "{}: new={new_ok} legacy={legacy_ok}",
                path.display()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "parser disagreements:\n{}",
        failures.join("\n")
    );
}

/// Snippets that BOTH parsers must accept (core slice only).
const ACCEPT: &[&str] = &[
    "1 + 2 * 3",
    "-(1)",
    "let x = 1",
    "const y = x + 1",
    "if (x) { return 1 } else { return 2 }",
    "while (x) { x = 0 }",
    "fn add(a, b) { return a + b }",
    "let f = (x) => x + 1",
    "foo(1, 2)",
    "a.b[c]",
    "[1, ...xs, 2]",
    r#"let o = { a: 1, "k": 2, ...rest }"#,
    "`hello`",
    "`a${x}b${y}c`",
    "a?.b",
    "f()?",
    "g()!",
    "a ? b : c",
    "f()? - 1",
    "await f()",
    "yield x",
    "a ?? b",
    "x += 1",
    "foo(...args)",
    // Assignment as a low-precedence expression (valid anywhere an expression is).
    "print(x = 5)",
    "f(a, b = 2, c)",
    "[x = 1]",
    "let r = (x = 5)",
    "a = b = c",
    // `nil` is a valid type (Type::Nil): return type, let annotation, union member.
    "fn f(): nil {}",
    "let x: nil = nil",
    "fn g(): number | nil { return nil }",
    // `fn` is a valid type (Type::Fn): let annotation + param annotation.
    "let f: fn = x",
    "fn apply(g: fn, x) { return g(x) }",
    // Match guard ENDING in a bare identifier before `=>` (must not be parsed as an
    // arrow that swallows the arm's `=>`). Closes the V10 differential blind spot:
    // guards were previously only tested ending in literals.
    "let g1 = match v { n if n == lim => \"eq\", other => \"o\" }",
    "let g2 = match v { n if n > 0 && n == lim => \"a\", other => \"o\" }",
    "let g3 = match v { x if (() => true)() => 1, _ => 2 }",
];

#[test]
fn new_parser_accepts_core_slice() {
    for src in ACCEPT {
        let p = ascript::syntax::parser::parse(src);
        assert!(
            p.errors.is_empty(),
            "new parser rejected {src:?}: {:?}",
            p.errors
        );
    }
}

#[test]
fn legacy_parser_also_accepts_core_slice() {
    for src in ACCEPT {
        let toks = ascript::lexer::lex(src).expect("legacy lex");
        assert!(
            ascript::parser::parse(&toks).is_ok(),
            "legacy rejected {src:?}"
        );
    }
}
