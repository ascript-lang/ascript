//! Front-end conformance guardrail.
//!
//! Feeds a catalog of representative snippets — one (or more) per grammar
//! construct, operator, literal form, statement, expression, and type form —
//! through the interpreter's own `lex` + `parse` and asserts each is ACCEPTED
//! (no parse error). This turns "discover front-end gaps one at a time" into a
//! standing regression guardrail: any construct the grammar admits but the
//! interpreter's parser rejects fails here.
//!
//! Companion to `treesitter_conformance.rs`, which checks the generated
//! Tree-sitter grammar against the example programs.

use ascript::lexer::lex;
use ascript::parser::parse;

/// Assert that `src` lexes and parses without error under the interpreter's
/// front end.
fn accepts(src: &str) {
    let toks = lex(src).unwrap_or_else(|e| panic!("lex failed for {src:?}: {e:?}"));
    parse(&toks).unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"));
}

#[test]
fn interpreter_parses_each_grammar_construct() {
    let snippets = [
        // --- let / const declarations (with and without init, typed) ---
        "let a = 1",
        "let b",
        "let c: number",
        "let ct: number = 7",
        "const d = 2",
        "const dt: string = \"x\"",
        "let [da, db] = pair",
        // --- number literals: hex / binary / scientific / underscore ---
        "let h = 0xFF",
        "let hu = 0xFF_FF",
        "let bi = 0b1010",
        "let sc = 1e9",
        "let scn = 1.5e-3",
        "let u = 1_000",
        "let fl = 3.14",
        // --- string literals: single / double / template, with escapes ---
        "let s1 = 'single'",
        "let s2 = \"esc \\\"q\\\" \\n\\t\\\\\"",
        "let s3 = 'it\\'s'",
        "let t = `hi ${a}`",
        "let t2 = `a ${a} b ${b} c`",
        "let t3 = `no interp`",
        // --- ranges ---
        "let r = 0..5",
        "let r2 = (1 + 1)..n",
        "let rr = f()..g()",
        "for (i in 0..5) { print(i) }",
        "for (i in r) { print(i) }",
        "for (i in items()) { print(i) }",
        "for (x of [1, 2]) { print(x) }",
        "for (ch of \"abc\") { print(ch) }",
        // --- array / object literals (incl. quoted keys, trailing commas) ---
        "let arr = [1, 2, 3]",
        "let arr2 = [1, 2, 3,]",
        "let empty = []",
        "let o = { k: 1, \"q\": 2 }",
        "let o2 = { a: 1, b: 2, }",
        "let oe = {}",
        // --- bool / nil literals ---
        "let bt = true",
        "let bf = false",
        "let bn = nil",
        // --- functions: sync / async / typed / return ---
        "fn f(x: number): number { return x ** 2 }",
        "fn noargs() { return }",
        "async fn g() { await h() }",
        "fn variadic(a, b, c) { return a }",
        // --- control flow: if/else if/else, while, break/continue ---
        "if (a) { } else if (b) { } else { }",
        "if (a) { print(1) }",
        "while (a) { break }",
        "while (a) { continue }",
        // --- match statement form ---
        "let m = match a { 1 => \"one\", 2 | 3 => \"few\", _ => \"many\" }",
        "let m2 = match a { x => 1, _ => 2 }",
        // --- match patterns (Phase 8): array / object / range / guard ---
        "let m3 = match a { [x] => x, [first, ...rest] => first, [] => 0 }",
        "let m4 = match a { [u, nil] => u, [_, e] => e, _ => 0 }",
        "let m5 = match a { {method, path} => path, _ => \"?\" }",
        "let m6 = match a { {role: \"admin\"} => 1, {role: r, ...rest} => 2, _ => 0 }",
        "let m7 = match a { 1..=9 => \"lo\", 10..100 => \"hi\", _ => \"x\" }",
        "let m8 = match a { _ if a < 0 => \"neg\", 0 => \"z\", n if n > 0 => \"pos\" }",
        // Guard ENDING in a bare identifier right before `=>` (must not be parsed as
        // an arrow that swallows the `=>` / arm body):
        "let m9 = match a { n if n == lim => \"eq\", other => \"o\" }",
        "let m10 = match a { n if n > 0 && n == lim => \"a\", other => \"o\" }",
        // --- classes / inheritance / enums ---
        "class C extends B { fn init() { super() } }",
        "class D { fn m(x) { return x } }",
        "class P { x: number; y: number }", // `;` optional separator between class members
        "enum E { A, B = 2 }",
        "enum E2 { A, B, C }",
        // --- import / export ---
        "import { foo, bar } from \"std/array\"",
        "import * as array from \"std/array\"",
        "export fn pub() { return 1 }",
        "export let exposed = 1",
        "export const k = 2",
        "export class Pub { fn m() { return 1 } }",
        "export enum Color { Red, Green }",
        // --- expressions: call / index / member / optional chain ---
        "let cl = f(1, 2, 3)",
        "let ix = arr[0]",
        "let mem = o.a.b",
        "let ch = o?.a?.b",
        "let tryp = readFile(p)?",
        // --- ternary conditional (and its disambiguation from postfix `?`) ---
        "let tern = a ? b : c",
        "let chain = a ? b : c ? d : e",
        "let neg = cond ? -1 : 1",
        "let mix = (x > 0 ? \"pos\" : \"neg\")",
        "o.method(1)",
        "fn m() { return self.x }",
        // --- arrow functions: single / multi / async ---
        "let sa = x => x + 1",
        "let ma = (a, b) => a + b",
        "let asa = async x => x + 1",
        "let ama = async (a, b) => a + b",
        "let ba = (a, b) => { return a + b }",
        // --- await / match expression ---
        "fn aw() { let v = await fetch() }",
        "let me = match a { 1 => f(1), _ => 0 }",
        // --- every binary / unary operator ---
        "let add = a + b",
        "let sub = a - b",
        "let mul = a * b",
        "let dvd = a / b",
        "let md = a % b",
        "let pw = a ** b",
        "let eq = a == b",
        "let ne = a != b",
        "let lt = a < b",
        "let le = a <= b",
        "let gt = a > b",
        "let ge = a >= b",
        "let an = a && b",
        "let orr = a || b",
        "let nt = !a",
        "let q = a ?? b",
        "let ng = -a",
        // --- precedence-sensitive forms ---
        "let pow = 2 ** 3 ** 2",
        "let neg = -2 ** 2",
        "let cmp = 1 < 2 == true",
        "let rprec = 1 + 1..5",
        // --- compound assignments ---
        "a = 1",
        "a += 1",
        "a -= 1",
        "a *= 2",
        "a /= 2",
        // --- type forms ---
        "fn typed(p: array<number>): map<string, number> { return p }",
        "fn ret_result(): Result<object> { return Ok(1) }",
        "let tup: [number, string] = [1, \"a\"]",
        "let un: number | nil = nil",
        "let named: Foo = bar",
        "let fnty: fn = f",
        "let anyty: any = a",
        "let boolty: bool = true",
        "let errty: error = nil",
        "let nilty: nil = nil",
        // --- destructuring / spread / rest ---
        "let [a, ...rest] = xs",
        "let {a, b as local} = obj",
        "let {\"k\" as v} = obj",
        "let {a, ...rest} = obj",
        "let a = [...x, 1]",
        "let o = {...x, k: 1}",
        "f(...args)",
        "fn f(a, ...rest) { return rest }",
        "fn f(...rest: array<number>) { return rest }",
    ];
    for s in snippets {
        accepts(s);
    }
}

#[test]
fn interpreter_parses_examples_dir() {
    // Mirror the tree-sitter conformance discovery: every committed example
    // must parse under the interpreter's own front end too. This in particular
    // covers `examples/ranges.as` (range-as-expression + let-without-init).
    use std::fs;
    let mut count = 0;
    for dir in ["examples", "examples/modules"] {
        for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir}: {e}")) {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) == Some("as") {
                let src = fs::read_to_string(&path).unwrap();
                let toks = lex(&src).unwrap_or_else(|e| panic!("lex failed for {path:?}: {e:?}"));
                parse(&toks).unwrap_or_else(|e| panic!("parse failed for {path:?}: {e:?}"));
                count += 1;
            }
        }
    }
    assert!(count > 0, "no example .as files found");
}
