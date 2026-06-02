//! Differential oracle: for core-slice snippets, the new parser's "no errors"
//! verdict must match the legacy parser's acceptance. Guards that the
//! hand-written parser and the language's grammar stay in agreement as the parser
//! grows. (Full-corpus differential testing arrives once grammar coverage is
//! complete in a later plan.)

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
];

#[test]
fn new_parser_accepts_core_slice() {
    for src in ACCEPT {
        let p = ascript::syntax::parser::parse(src);
        assert!(p.errors.is_empty(), "new parser rejected {src:?}: {:?}", p.errors);
    }
}

#[test]
fn legacy_parser_also_accepts_core_slice() {
    for src in ACCEPT {
        let toks = ascript::lexer::lex(src).expect("legacy lex");
        assert!(ascript::parser::parse(&toks).is_ok(), "legacy rejected {src:?}");
    }
}
