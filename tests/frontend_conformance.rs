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
use ascript::syntax::kind::SyntaxKind;
use ascript::syntax::parse_to_tree;
use ascript::syntax::parser as cst_parser;

/// Assert that `src` lexes and parses without error under the interpreter's
/// front end.
fn accepts(src: &str) {
    let toks = lex(src).unwrap_or_else(|e| panic!("lex failed for {src:?}: {e:?}"));
    parse(&toks).unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"));
}

/// Assert that `src` parses without error (no `ParseError`, no `Error` node)
/// under the CST front-end (`src/syntax/`).
fn cst_accepts(src: &str) {
    let parsed = cst_parser::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "CST parse errors for {src:?}: {:?}",
        parsed.errors
    );
    let has_error_node = parse_to_tree(src)
        .descendants()
        .any(|n| n.kind() == SyntaxKind::Error);
    assert!(!has_error_node, "CST produced an Error node for {src:?}");
}

/// Differential: BOTH the legacy parser and the CST parser must accept `src`
/// with no error.
fn both_accept(src: &str) {
    accepts(src);
    cst_accepts(src);
}

/// `..=` (inclusive) and `step` parse on BOTH front-ends, in for-range and
/// value position. Phase 1 is PARSE-only; semantics land in Phase 2/3.
#[test]
fn both_frontends_accept_inclusive_and_step_ranges() {
    both_accept("for (i in 1..=5) {}");
    both_accept("for (i in 1..10 step 2) {}");
    both_accept("for (i in 10..1 step -2) {}");
    both_accept("for (i in 1..=10 step 2) {}");
    both_accept("let xs = 1..=5");
    both_accept("let ys = 1..10 step 2");
    both_accept("let zs = 1..=10 step 2");
}

/// `step` is a CONTEXTUAL keyword — it must remain usable as an ordinary
/// identifier (fn name, variable, member) on the CST front-end.
#[test]
fn cst_keeps_step_as_a_plain_identifier() {
    cst_accepts("fn step(n) { n }");
    cst_accepts("let step = 1");
    cst_accepts("let x = step + 1");
    cst_accepts("step(3)");
}

/// `static` is a member modifier on `fn` / `async fn` / `fn*` inside a class
/// body — both front-ends must accept it (SP1 Phase C).
#[test]
fn both_frontends_accept_static_methods() {
    both_accept("class C { static fn make() { return C() } }");
    both_accept("class C { static async fn create() { return C() } }");
    both_accept("class C { static fn* gen() { yield 1 } }");
    both_accept("class C { fn m() { return 1 }\n static fn s() { return 2 } }");
}

/// `;` is an OPTIONAL separator between class members on BOTH front-ends (the
/// legacy parser + the tree-sitter `class_body` rule already allow it; this guards
/// the CST hand-parser, which used to reject it — a legacy-vs-CST divergence where
/// the tree-walker ran a program the VM refused to compile). The catalog above only
/// exercised the LEGACY parser via `accepts`, so this `both_accept` battery is the
/// real cross-front-end guard.
#[test]
fn both_frontends_accept_semicolon_between_class_members() {
    both_accept("class P { x: number; y: number }");
    both_accept("class Q { x: number = 1; y: number = 2 }");
    both_accept("class R { x: number = 5; fn f() { return self.x } }");
    both_accept("class S { fn f() { return 1 }; fn g() { return 2 } }");
    // leading / trailing / doubled separators are all tolerated (repeat(';'))
    both_accept("class T { ; x: number; }");
    both_accept("class U { x: number;; y: number }");
}

/// `static` is a CONTEXTUAL/soft keyword — it must remain usable as an ordinary
/// identifier where unambiguous (variable, fn name, member, field name).
#[test]
fn cst_keeps_static_as_a_plain_identifier() {
    cst_accepts("let static = 1");
    cst_accepts("fn static(n) { n }");
    cst_accepts("let x = static + 1");
    cst_accepts("static(3)");
    cst_accepts("class C { static: number = 0 }");
}

#[test]
fn interpreter_parses_each_grammar_construct() {
    // Each snippet must parse on BOTH front-ends (legacy precedence-climbing + CST
    // hand-parser). Originally this checked only the legacy parser (`accepts`),
    // which let a CST-only divergence slip through (`;` between class members — the
    // CST rejected what the legacy parser + tree-sitter accept). `both_accept` makes
    // this the standing cross-front-end parse guard for every catalogued construct.
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
        // Or-pattern alternatives that BIND the same name (must parse on both
        // front-ends; the same-name-set rule is a SEMANTIC resolver check, not a
        // parse error — see `cli::match_or_pattern_*`):
        "let mor = match a { Shape.Circle(r) | Shape.Square(r) => r, _ => 0 }",
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
        // Propagate-`?` followed by an infix op then a REAL ternary: the first `?`
        // is postfix propagate (next token `>` cannot begin an expression), the
        // later `:` belongs to the trailing ternary. (FUZZ Unit-2 repro A — the CST
        // token-scan once fused these into one ternary and rejected the program.)
        "fn a() { return g(5)? > 0 ? \"pos\" : \"neg\" }",
        // Propagate-`?` INSIDE a ternary then-branch: the inner `?` is propagate
        // (next token is the OUTER ternary's `:`, not an expression start).
        "fn b(c) { return c ? g(5)? : 0 }",
        // A ternary whose then-branch is an expression-introducing keyword — these
        // MUST still parse as ternaries (the next-token guard accepts `match`/`async`).
        "let tm = c ? match x { 1 => 2, _ => 3 } : y",
        "let ta = c ? async () => 5 : z",
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
        both_accept(s);
    }
}

#[test]
fn interpreter_parses_examples_dir() {
    // Mirror the tree-sitter conformance discovery: every committed example
    // must parse under the interpreter's own front end too. This in particular
    // covers `examples/ranges.as` (range-as-expression + let-without-init).
    use std::fs;
    let mut count = 0;
    for dir in ["examples", "examples/modules", "examples/app"] {
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

// ---- SP2 Phase C: `#{…}` map literals -------------------------------------

/// Assert `src` is REJECTED by the legacy front-end (a lex OR parse error).
fn legacy_rejects(src: &str) {
    let toks = match lex(src) {
        Ok(t) => t,
        Err(_) => return, // lex error is a valid rejection
    };
    assert!(
        parse(&toks).is_err(),
        "legacy front-end should REJECT {src:?}, but it parsed"
    );
}

/// Assert `src` is REJECTED by the CST front-end (a parse error OR an Error node).
fn cst_rejects(src: &str) {
    let parsed = cst_parser::parse(src);
    let has_error_node = parse_to_tree(src)
        .descendants()
        .any(|n| n.kind() == SyntaxKind::Error);
    assert!(
        !parsed.errors.is_empty() || has_error_node,
        "CST front-end should REJECT {src:?}, but it parsed clean"
    );
}

#[test]
fn both_frontends_accept_map_literals() {
    both_accept("let m = #{}");
    both_accept("let m = #{ \"a\": 1, \"b\": 2 }");
    both_accept("let m = #{ 1: \"x\", 2: \"y\" }");
    both_accept("let k = \"x\"\nlet m = #{ k: 1, true: 2, nil: 3 }");
    both_accept("let m = #{ 1 + 2: \"three\", }"); // trailing comma
    both_accept("fn f() { return #{ 1: \"x\" } }");
}

#[test]
fn both_frontends_reject_map_spread() {
    // D4: a `...` spread element inside `#{}` is a clean parse error on BOTH
    // front-ends (no panic). The two parsers word it differently; we only assert
    // that each rejects.
    legacy_rejects("let n = #{}\nlet m = #{ ...n }");
    cst_rejects("let n = #{}\nlet m = #{ ...n }");
}

#[test]
fn both_frontends_reject_bare_hash() {
    // `#` not followed by `{` is a lex error on the legacy lexer and an Error
    // token on the CST lexer — both front-ends reject it.
    legacy_rejects("let x = # 5");
    cst_rejects("let x = # 5");
}

// ---- Spec B Task 1: `worker class` + `worker fn*` parsing -----------------

/// Both front-ends accept `worker class C { … }` and produce a class AST node
/// (legacy: `Stmt::Class { is_worker: true, .. }`; CST: `ClassDecl` with a
/// `WorkerKw` child). The minimal worker class from the Spec B plan.
#[test]
fn both_frontends_accept_worker_class() {
    let src = "worker class Db { fn query(sql) { return sql } }";
    both_accept(src);

    // Legacy: assert Stmt::Class { is_worker: true }
    let toks = ascript::lexer::lex(src).unwrap();
    let stmts = ascript::parser::parse(&toks).unwrap();
    match &stmts[0] {
        ascript::ast::Stmt::Class { name, is_worker, .. } => {
            assert_eq!(name, "Db");
            assert!(*is_worker, "expected is_worker=true for 'worker class Db'");
        }
        other => panic!("expected Stmt::Class, got {other:?}"),
    }

    // CST: assert ClassDecl has a WorkerKw child token
    let tree = ascript::syntax::parse_to_tree(src);
    let has_worker_kw = tree
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .any(|t| t.kind() == ascript::syntax::kind::SyntaxKind::WorkerKw);
    assert!(has_worker_kw, "CST ClassDecl should have a WorkerKw token for 'worker class Db'");
}

/// Both front-ends accept the full worker class from the plan (field + init + method).
#[test]
fn both_frontends_accept_worker_class_with_field_init_method() {
    both_accept(
        "worker class Db { conn: any = nil fn init(url) { self.conn = url } fn query(sql) { return sql } }",
    );
}

/// Both front-ends accept `worker fn* records(path) { yield path }` and the
/// legacy parser sets both `is_worker=true` and `is_generator=true` on the fn.
#[test]
fn both_frontends_accept_worker_generator() {
    let src = "worker fn* records(path) { yield path }";
    both_accept(src);

    // Legacy: assert Stmt::Fn { is_worker: true, is_generator: true }
    let toks = ascript::lexer::lex(src).unwrap();
    let stmts = ascript::parser::parse(&toks).unwrap();
    match &stmts[0] {
        ascript::ast::Stmt::Fn { name, is_worker, is_generator, .. } => {
            assert_eq!(name, "records");
            assert!(*is_worker, "expected is_worker=true");
            assert!(*is_generator, "expected is_generator=true");
        }
        other => panic!("expected Stmt::Fn, got {other:?}"),
    }
}

/// `worker` remains a plain identifier where it is NOT immediately followed by
/// `class`, `fn`, or `async` — it must not be reserved.
#[test]
fn worker_stays_contextual_not_reserved_for_class() {
    both_accept("let worker = 5");
    both_accept("fn worker() { return 1 }");
    // `worker` as a variable followed by something that's not `class`/`fn`/`async`
    both_accept("let x = worker + 1");
    both_accept("worker(1)");
}

/// NUM §3.4 — both front-ends accept the bitwise / shift / wrapping operators and
/// agree on the Go-style precedence. PARSE-acceptance baseline (the structural
/// disambiguation is asserted in the dedicated tests below).
#[test]
fn both_frontends_accept_bitwise_and_wrapping_operators() {
    both_accept("let a = 0xFF & 0b1010");
    both_accept("let b = (1 << 16) | 256");
    both_accept("let c = ~0");
    both_accept("let d = 5 +% 3");
    both_accept("let e = x -% y");
    both_accept("let f = x *% y");
    both_accept("let g = a ^ b >> 1");
    both_accept("let h = a & b == c");
    both_accept("let i = a | b == c");
    // Nested generics: a single `>>` closes two type-argument lists.
    both_accept("let u: future<array<int>> = nil");
    both_accept("let v: map<int, array<int>> = #{}");
    // A shift in expression position is NOT a nested generic.
    both_accept("let w = a >> b");
}

/// [NUM §3.1/§6] Octal literals and the reserved-type-name `instanceof` RHS parse
/// on BOTH front-ends.
#[test]
fn both_frontends_accept_octal_and_reserved_instanceof() {
    // Octal literals (`0o`/`0O`, underscores allowed).
    both_accept("let oa = 0o17");
    both_accept("let ob = 0O755");
    both_accept("let oc = 0o1_7");
    // `x instanceof int|float|number|string|bool` parses in expression position.
    both_accept("let w = x instanceof int");
    both_accept("let y = x instanceof float");
    both_accept("let z = x instanceof number");
    both_accept("let s = x instanceof string");
    both_accept("let b = x instanceof bool");
    // and still works as a class check.
    both_accept("class Foo {}\nlet f = x instanceof Foo");
}

/// [CRITICAL, NUM §3.4] The `1 | 2`-pattern vs `a | b`-value vs `A | B`-type
/// disambiguation, asserted STRUCTURALLY on BOTH front-ends.
///
/// (1) `match x { 1 | 2 => … }` is a single arm with a TWO-alternative pattern
///     (legacy `MatchArm.patterns.len() == 2`; CST an `OrPat` with two children) —
///     NOT a bitwise `1 | 2`.
/// (2) value-position `a | b` is ONE bitwise-or expression (legacy `BinOp::BitOr`;
///     CST a single `BinaryExpr`) — NOT a pattern.
/// (3) type-position `int | float` is a `UnionType`.
#[test]
fn pipe_disambiguates_pattern_vs_value_vs_type_on_both_frontends() {
    use ascript::ast::{BinOp, ExprKind, Stmt};
    use ascript::syntax::kind::SyntaxKind;

    // ---- (1) or-pattern `1 | 2` is a two-alternative pattern, not bitwise -----
    let pat_src = r#"let r = match x { 1 | 2 => "a", _ => "b" }"#;
    both_accept(pat_src);
    // Legacy: the first arm's pattern list has two alternatives.
    let toks = ascript::lexer::lex(pat_src).unwrap();
    let stmts = ascript::parser::parse(&toks).unwrap();
    let arms = match &stmts[0] {
        Stmt::Let { value: Some(e), .. } => match &e.kind {
            ExprKind::Match { arms, .. } => arms,
            other => panic!("expected ExprKind::Match, got {other:?}"),
        },
        other => panic!("expected Stmt::Let with a match initializer, got {other:?}"),
    };
    assert_eq!(
        arms[0].patterns.len(),
        2,
        "legacy: `1 | 2` must be a TWO-alternative or-pattern, not a bitwise expr"
    );
    // CST: there is an OrPat node with two child patterns.
    let tree = ascript::syntax::parse_to_tree(pat_src);
    let or_pat = tree
        .descendants()
        .find(|n| n.kind() == SyntaxKind::OrPat)
        .expect("CST: `1 | 2` must produce an OrPat node");
    let alt_count = or_pat
        .children()
        .filter(|c| c.kind() == SyntaxKind::LiteralPat)
        .count();
    assert_eq!(alt_count, 2, "CST OrPat must have two LiteralPat children");

    // ---- (2) value-position `a | b` is ONE bitwise-or expression --------------
    let val_src = "let m = a | b";
    both_accept(val_src);
    // Legacy: the initializer is a single `BinOp::BitOr`.
    let toks = ascript::lexer::lex(val_src).unwrap();
    let stmts = ascript::parser::parse(&toks).unwrap();
    match &stmts[0] {
        Stmt::Let { value: Some(e), .. } => match &e.kind {
            ExprKind::Binary { op: BinOp::BitOr, .. } => {}
            other => panic!("legacy: expected a BinOp::BitOr binary, got {other:?}"),
        },
        other => panic!("expected Stmt::Let with an initializer, got {other:?}"),
    }
    // CST: exactly one BinaryExpr whose operator token is `Pipe`, and NO OrPat.
    let tree = ascript::syntax::parse_to_tree(val_src);
    assert!(
        tree.descendants().all(|n| n.kind() != SyntaxKind::OrPat),
        "CST: value-position `a | b` must NOT be an or-pattern"
    );
    let has_pipe_binary = tree
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::BinaryExpr)
        .any(|bin| {
            bin.children_with_tokens()
                .filter_map(|el| el.into_token())
                .any(|t| t.kind() == SyntaxKind::Pipe)
        });
    assert!(
        has_pipe_binary,
        "CST: value-position `a | b` must be a BinaryExpr with a `Pipe` operator"
    );

    // ---- (3) type-position `int | float` is a UnionType -----------------------
    let ty_src = "let t: int | float = 1";
    both_accept(ty_src);
    let tree = ascript::syntax::parse_to_tree(ty_src);
    assert!(
        tree.descendants()
            .any(|n| n.kind() == SyntaxKind::UnionType),
        "CST: `int | float` in type position must be a UnionType"
    );
}

/// `a & b == c` and `a | b == c` parse with Go's binding `(a&b)==c` / `(a|b)==c`
/// on BOTH front-ends (the structural shape, not just acceptance). Legacy: the
/// top-level operator is `==` (Eq) whose left operand is the bitwise op.
#[test]
fn go_bitwise_precedence_agrees_on_both_frontends() {
    use ascript::ast::{BinOp, ExprKind, Stmt};

    for (src, inner) in [
        ("let r = a & b == c", BinOp::BitAnd),
        ("let r = a | b == c", BinOp::BitOr),
    ] {
        both_accept(src);
        let toks = ascript::lexer::lex(src).unwrap();
        let stmts = ascript::parser::parse(&toks).unwrap();
        match &stmts[0] {
            Stmt::Let { value: Some(e), .. } => match &e.kind {
                // Top operator is `==`; its LEFT operand is the bitwise op.
                ExprKind::Binary { op: BinOp::Eq, lhs, .. } => match &lhs.kind {
                    ExprKind::Binary { op, .. } => assert!(
                        matches!((op, inner),
                            (BinOp::BitAnd, BinOp::BitAnd) | (BinOp::BitOr, BinOp::BitOr)),
                        "left operand of `==` must be the bitwise op for {src:?}"
                    ),
                    other => panic!("expected a bitwise binary on the left of `==`, got {other:?}"),
                },
                other => panic!("expected top-level `==` for {src:?}, got {other:?}"),
            },
            other => panic!("expected Stmt::Let, got {other:?}"),
        }
    }
}

/// A plain (non-worker) class still parses cleanly — no regression.
#[test]
fn both_frontends_accept_plain_class_unchanged() {
    both_accept("class Point { x: number\n y: number\n fn init(x, y) { self.x = x; self.y = y } }");

    let src = "class Foo {}";
    let toks = ascript::lexer::lex(src).unwrap();
    let stmts = ascript::parser::parse(&toks).unwrap();
    match &stmts[0] {
        ascript::ast::Stmt::Class { name, is_worker, .. } => {
            assert_eq!(name, "Foo");
            assert!(!*is_worker, "plain class must have is_worker=false");
        }
        other => panic!("expected Stmt::Class, got {other:?}"),
    }
}

// ─────────────────────────────── ADT (Task 4) ───────────────────────────────

/// Differential: BOTH front-ends must REJECT `src` (parse error / Error node).
fn both_reject(src: &str) {
    legacy_rejects(src);
    cst_rejects(src);
}

#[test]
fn both_frontends_accept_payload_enums() {
    both_accept("enum Shape { Circle(radius: float), Rect(w: float, h: float), Pair(int, int), Point }");
    both_accept("enum Status { Active, Inactive = 0, Pending = 1 }");
    both_accept("enum Json { Null, Bool(value: bool), Num(value: float), Arr(items: array<Json>) }");
    both_accept("enum E { Opt(x: int?) }");
}

#[test]
fn both_frontends_accept_variant_patterns() {
    // Positional (call-recovery on both front-ends).
    both_accept("fn f(s) { return match s { Circle(r) => r, Pair(a, b) => a, Point => 0 } }");
    both_accept("fn f(s) { return match s { Shape.Circle(r) => r, _ => 0 } }");
    // Named (variant_pattern node).
    both_accept("fn f(s) { return match s { Rect(w: ww, h: hh) => ww, _ => 0 } }");
    both_accept("fn f(s) { return match s { Shape.Rect(w: a, h: b) => a, _ => 0 } }");
    // Nested literal + guard + or-pattern.
    both_accept("fn f(s) { return match s { Circle(0.0) => 1, Pair(a, b) if a == b => 2, Circle(_) | Rect(_, _) => 3, _ => 0 } }");
}

#[test]
fn both_frontends_accept_named_variant_construction() {
    // ADT §3.2: named call arguments (`Shape.Rect(w: 3.0, h: 4.0)`) parse on BOTH
    // front-ends — qualified and bare, single- and multi-field, order-independent,
    // and first-class (`mk(w: 1.0, h: 2.0)`).
    both_accept("let r = Shape.Rect(w: 3.0, h: 4.0)");
    both_accept("let r = Shape.Rect(h: 4.0, w: 3.0)");
    both_accept("let c = Shape.Circle(radius: 2.0)");
    both_accept("let c = Shape.Circle(2.0)");
    both_accept("let mk = Shape.Rect\nlet r = mk(w: 1.0, h: 2.0)");
    // A named arg whose value is itself an expression (nested construction).
    both_accept("let r = Shape.Rect(w: 1.0 + 2.0, h: f(3.0))");
    // Named args do not interfere with ordinary positional / spread calls.
    both_accept("f(1, 2, 3)");
    both_accept("f(...xs, 1)");
    // A bare `x: y` is a named arg only at argument position — a ternary still parses.
    both_accept("let z = cond ? a : b");
}

#[test]
fn both_frontends_reject_mixed_and_both_payload_variants() {
    // Mixed named+positional fields in one variant.
    both_reject("enum E { Pair(int, h: float) }");
    // Both a `= scalar` backing AND a `(…)` payload.
    both_reject("enum E { Foo = 2(int) }");
}

// ---- IFACE Task 6: interface declarations parse on BOTH front-ends ----

#[test]
fn both_frontends_accept_interface_declarations() {
    both_accept("interface Empty {}");
    both_accept("interface Reader { fn read(b): int }");
    both_accept("interface RW { fn read(b): int; fn write(b): int }");
    both_accept("interface RW {\n fn read(b)\n fn write(b)\n}");
    both_accept("interface RW extends Reader, Writer {}");
    both_accept("export interface R { fn read(b): int }");
    // A class with an `implements` clause (with and without `extends`).
    both_accept("class C extends Super implements A, B { fn read(b) { return 0 } }");
    both_accept("class C implements A { fn read(b) {} }");
}

#[test]
fn both_frontends_reject_interface_modifiers_on_requirement() {
    both_reject("interface R { async fn read(b) }");
    both_reject("interface R { static fn read(b) }");
    both_reject("interface R { worker fn read(b) }");
    both_reject("interface R { fn* read(b) }");
}

// ---- TYPE Task 6: generics surface — BOTH front-ends agree ----

#[test]
fn both_frontends_accept_generic_decls() {
    // Type-param lists on every decl kind.
    both_accept("fn id<T>(x: T): T { return x }");
    both_accept("fn map<A, B>(xs: array<A>, f: fn(A) -> B): array<B> { return [] }");
    both_accept("fn first<T, C: Container<T>>(c: C): T { return c.at(0) }");
    both_accept("class Box<T> { value: T\n fn get(): T { return self.value } }");
    both_accept("class Pair<A, B> { a: A\n b: B }");
    both_accept("enum Option<T> { Some(value: T), None }");
    both_accept("enum Result2<T, E> { Ok(value: T), Err(error: E) }");
    both_accept("interface Container<T> { fn len(): int\n fn at(i: int): T }");
    both_accept("export fn id<T>(x: T): T { return x }");
}

#[test]
fn both_frontends_accept_fnsig_and_generic_application_types() {
    // `fn(A) -> B` function types in param/return/field/let position.
    both_accept("fn apply<A, B>(f: fn(A) -> B, x: A): B { return f(x) }");
    both_accept("fn z(cb: fn() -> bool) {}");
    both_accept("fn multi(cb: fn(int, string) -> array<int>) {}");
    // User generic application in TYPE position (args parsed-then-erased).
    both_accept("let b: Box<int> = make()");
    both_accept("let m: Map<string, int> = make()");
    // Nested generic application closes via the `>>`-split.
    both_accept("fn h(m: map<int, array<int>>) {}");
    both_accept("let bb: Box<Box<int>> = make()");
}

#[test]
fn both_frontends_accept_explicit_type_arg_calls() {
    // The NEW expression-level disambiguation — these are generic-instantiation
    // CALLS (the trailing `(` after `>` selects the type-arg reading).
    both_accept("let b = Box<int>(5)");
    both_accept("let xs = map<string, number>(items, f)");
    both_accept("Box<Box<int>>(5)");
    both_accept("foo<int>(1)");
}

#[test]
fn both_frontends_keep_comparison_when_not_a_type_arg_call() {
    // The paired COMPARISON battery — none of these flip to a type-arg call (no `>`
    // immediately followed by `(`), so both front-ends keep them as comparisons.
    both_accept("let _ = a < b");
    both_accept("let _ = a > b");
    both_accept("let _ = a << b");
    both_accept("let _ = a >> b");
    both_accept("let _ = a < b && c > d");
    both_accept("f(a < b, c > d)");
    both_accept("let _ = x < y ? a : b");
    both_accept("let _ = a < b > c");
}

// ---- DEFER Task 1.2: `defer [await] <call>` — both front-ends agree ----

/// Both front-ends accept every legal `defer` form from §2.1.
#[test]
fn both_frontends_accept_defer_stmt() {
    both_accept("fn f() { defer g() }");
    both_accept("fn f() { defer obj.close() }");
    both_accept("fn f() { defer a?.flush() }");
    both_accept("fn f() { defer (cond ? a : b)() }");
    both_accept("fn f() { defer (() => { print(1) })() }");
    both_accept("fn f() { defer g(...xs) }");
    both_accept("fn f() { defer await g() }");
    // top-level defer is legal (module body)
    both_accept("defer g()");
    // parenthesized callee is still a call
    both_accept("fn f() { defer (f)() }");
}

/// Both front-ends reject non-call expressions after `defer`.
#[test]
fn both_frontends_reject_defer_non_call() {
    both_reject("fn f() { defer x }");
    both_reject("fn f() { defer a + b }");
    both_reject("fn f() { defer g }");
    both_reject("fn f() { defer g()? }");
    both_reject("fn f() { defer g()! }");
}

/// Both front-ends reject named-argument calls after `defer` (§2.1 v1 Tier-1).
#[test]
fn both_frontends_reject_defer_named_args() {
    both_reject("fn f() { defer g(x: 1) }");
}

/// Only the DEFERRED call's OWN named args are a Tier-1 error (§2.1). A NESTED
/// call with named args (ADT-variant-style construction) inside a deferred call
/// whose own args are positional is ACCEPTED on BOTH front-ends — the named-arg
/// rejection must NOT fire on nested calls.
#[test]
fn both_frontends_accept_defer_with_nested_named_args() {
    // The deferred call `g(...)` has a single POSITIONAL arg; `x:` belongs to
    // the nested `inner(...)`.
    both_accept("fn f() { defer g(inner(x: 1)) }");
    // ADT-variant-style named construction nested inside a positional deferred call.
    both_accept("fn f() { defer cleanup(Rect(w: 1, h: 2)) }");
}

/// `defer` is RESERVED — both front-ends reject its use as an identifier.
#[test]
fn both_frontends_reject_defer_as_identifier() {
    both_reject("let defer = 5");
}
