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
fn treesitter_parses_template_escapes_with_interpolation() {
    // A backslash escape (`\n`, `\t`, `\\`, `\``, `\$`) inside a template literal —
    // especially ADJACENT to a `${…}` interpolation — must parse without error
    // nodes. `template_chars` stops at `\`, so the grammar needs a `template_escape`
    // rule; its absence produced ERROR nodes in `examples/blob_basics.as`
    // (`${method}\n${path}` etc.) while the legacy + CST front-ends accepted them.
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    for src in [
        r#"let s = `${a}\n${b}`"#,
        r#"let s = `AWS4-HMAC-SHA256\n${amzDate}\n${scope}`"#,
        r#"let s = `${name}:${value}\n`"#,
        r#"let s = `tab\there and a backtick \` and dollar \$`"#,
        r#"let s = `plain \n no interpolation`"#,
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter error on template-escape snippet: {src}"
        );
    }
}

#[test]
fn treesitter_parses_anonymous_fn_expressions() {
    // Anonymous `fn(params){body}` EXPRESSIONS (LSPEC) — a value-position closure
    // mirroring the block-bodied arrow. The grammar must parse them with NO error
    // nodes in every expression position, while keeping `fn name(…){…}` a
    // DECLARATION (the disambiguation is `fn` followed by `(` vs an identifier).
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    for src in [
        "let f = fn() { return 5 }",
        "let g = fn(x) { return x + 1 }",
        "let r = recover(fn() { return 5 })",
        "let xs = array.map([1,2,3], fn(x) { return x * 2 })",
        "let v = (fn() { return 7 })()",
        "let a = fn(x: int, y = 2) { return x + y }",
        "let b = fn(...xs: array<int>) { return len(xs) }",
        // bare expression statement.
        "fn() { return 1 }",
        // a declaration and an expression coexisting — disambiguation proof.
        "fn named() { return 1 }\nlet h = fn() { return 2 }",
        // async anonymous fn expression.
        "let p = recover(async fn() { return 1 })",
        // BLOCKER 1: a fn-expression as a `return` / `yield` / ternary operand.
        "fn make(n) { return fn() { return n * 2 } }",
        "fn* g() { yield fn() { return 1 } }",
        "let f = (c) => c ? fn() { return 1 } : nil",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter error on anonymous-fn-expression snippet: {src}"
        );
    }
    // BLOCKER 2: a RETURN-type annotation on a fn-expression is NOT allowed (the
    // `function_expression` rule has no return-type slot) — it must produce an
    // error node, mirroring the hand front-ends' clean parse-error rejection.
    for src in [
        "let f = fn(x: int): string { return x }",
        "let g = fn(): int { return 1 }",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            tree.root_node().has_error(),
            "tree-sitter UNEXPECTEDLY accepted a fn-expr return type: {src}"
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
fn treesitter_parses_bitwise_and_wrapping_operators() {
    // NUM §3.2/§3.4: bitwise (`& | ^ ~`), shift (`<< >>`), and wrapping
    // (`+% -% *%`) operators parse. CRITICAL disambiguation (§3.4):
    //   - `1 | 2` in a match pattern is a TWO-ALTERNATIVE or-pattern (not bitwise);
    //   - `a | b` in value position is a SINGLE bitwise-or expression;
    //   - `int | float` in type position is a union type;
    //   - `a >> b` is a shift, while `future<array<int>>` /
    //     `map<int, array<int>>` close two nested generics from one `>>`.
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    for src in [
        // bitwise / shift / wrapping in value position
        "let a = 0xFF & 0b1010",
        "let b = (1 << 16) | 256",
        "let c = ~0",
        "let d = 5 +% 3",
        "let e = x -% y",
        "let f = x *% y",
        "let g = a ^ b",
        "let h = a >> 1",
        // Go precedence footguns parse the intuitive way
        "let i = a & b == c",
        "let j = a | b == c",
        // `a | b` in value position is ONE bitwise-or expression
        "let m = a | b",
        // or-pattern `1 | 2` stays a pattern (NOT bitwise)
        r#"let r = match x { 1 | 2 => "a", _ => "b" }"#,
        // union type stays a union
        "let t: int | float = 1",
        // nested generics: the `>>` splits into two closing `>`
        "let u: future<array<int>> = nil",
        "let v: map<int, array<int>> = #{}",
        // NUM §3.1: octal literals (`0o`/`0O`).
        "let oa = 0o17",
        "let ob = 0O755",
        "let oc = 0o1_7",
        // NUM §6: reserved-type-name `instanceof` RHS parses in expression position.
        "let w = x instanceof int",
        "let y = x instanceof float",
        "let z = x instanceof number",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter error on bitwise/wrapping snippet: {src}\n{}",
            tree.root_node().to_sexp()
        );
    }
}

fn query_files() -> Vec<PathBuf> {
    // Resolve relative to the crate manifest so the test is cwd-independent.
    let query_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tree-sitter-ascript/queries");
    let mut files = Vec::new();
    let entries = fs::read_dir(&query_dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", query_dir.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) == Some("scm") {
            files.push(path);
        }
    }
    files.sort();
    assert!(
        !files.is_empty(),
        "no queries/*.scm files found in {}",
        query_dir.display()
    );
    files
}

/// Drift guard: every `queries/*.scm` must compile against the grammar. A grammar
/// change that renames/removes a node or field (without updating the queries) makes
/// `Query::new` fail here — so query drift breaks CI, not an editor at runtime.
///
/// The set is enumerated dynamically so any newly added query file is auto-covered.
#[test]
fn queries_compile_against_grammar() {
    let lang = language();

    let mut failed = Vec::new();
    let files = query_files();
    for path in &files {
        let src = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        if let Err(e) = tree_sitter::Query::new(&lang, &src) {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
            failed.push(format!("{name}: {e:?}"));
        }
    }

    assert!(
        failed.is_empty(),
        "queries failed to compile against the grammar: {failed:?}"
    );
}

fn parse_has_error(src: &str) -> bool {
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    tree.root_node().has_error()
}

#[test]
fn treesitter_parses_worker_decls() {
    for src in [
        "worker fn f() { return 1 }",
        // Canonical modifier order is `worker async fn` (worker BEFORE async),
        // matching both Rust front-ends, the formatter, and `method_definition`.
        "worker async fn f() { return 1 }",
        "class C { static worker fn h(x) { return x } }",
        "class C { worker fn m(x) { return x } }",
    ] {
        assert!(!parse_has_error(src), "tree-sitter ERROR node in: {src}");
    }
}

#[test]
fn treesitter_parses_worker_class_and_worker_generator() {
    // `worker class C {}` must parse without ERROR node — the grammar needs
    // `optional($.worker_keyword)` on `class_declaration` (Task 2, Spec B).
    // `worker fn* g() {}` must also be error-free — `worker?` and `*` are
    // independent optionals on `function_declaration`, already in the grammar
    // from Plan A (verify here as a standing regression guard).
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    for src in [
        "worker class C { fn m(x) { return x } }",
        "worker fn* g() { yield 1 }",
        // `worker` is contextual — still an ordinary identifier elsewhere.
        "let worker = 5",
        "fn worker() { return 1 }",
        "let r = f(worker)",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter ERROR node in: {src}"
        );
    }
}

#[test]
fn treesitter_parses_adt_payload_enums_and_variant_patterns() {
    // ADT (Task 5): payload variant declarations (named/positional) and variant
    // destructuring patterns (positional via call-recovery, named via variant_pattern)
    // must parse without an ERROR node on the tree-sitter grammar.
    for src in [
        // Payload declarations.
        "enum Shape { Circle(radius: float), Rect(w: float, h: float), Pair(int, int), Point }",
        "enum Status { Active, Inactive = 0, Pending = 1 }",
        "enum Json { Null, Bool(value: bool), Arr(items: array<Json>) }",
        // Positional variant patterns (ride call_expression).
        "fn f(s) { return match s { Circle(r) => r, Pair(a, b) => a, Point => 0 } }",
        "fn f(s) { return match s { Shape.Circle(r) => r, _ => 0 } }",
        // Named variant patterns (variant_pattern node).
        "fn f(s) { return match s { Rect(w: ww, h: hh) => ww, _ => 0 } }",
        "fn f(s) { return match s { Shape.Rect(w: a, h: b) => a, _ => 0 } }",
        // Nested + guard + or-pattern.
        "fn f(s) { return match s { Circle(0.0) => 1, Pair(a, b) if a == b => 2, Circle(_) | Rect(_, _) => 3, _ => 0 } }",
        // ADT §3.2: named call arguments for variant construction (named_argument node).
        "let r = Shape.Rect(w: 3.0, h: 4.0)",
        "let r = Shape.Rect(h: 4.0, w: 3.0)",
        "let c = Shape.Circle(radius: 2.0)",
        "let r = Shape.Rect(w: 1.0 + 2.0, h: g(3.0))",
        "let mk = Shape.Rect\nlet r = mk(w: 1.0, h: 2.0)",
        // Named args must not disturb positional / spread calls or ternaries.
        "f(1, 2, 3)",
        "f(...xs, 1)",
        "let z = cond ? a : b",
    ] {
        assert!(!parse_has_error(src), "tree-sitter ERROR node in: {src}");
    }
}

#[test]
fn treesitter_parses_interface_decls_and_implements() {
    // IFACE (Task 7): structural interface declarations (0/1/N method
    // requirements, `extends` composition, `;`-separated requirements) and the
    // class `implements` clause must parse without an ERROR node.
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    for src in [
        // 0 / 1 / N requirements (return annotation is `: T`, like fns).
        "interface Empty {}",
        "interface Reader { fn read(b: bytes): int }",
        "interface RW { fn read(b: bytes): int\n fn write(b: bytes): int }",
        // `;`-separated requirements (skip_semicolons rule).
        "interface R { fn read(b): int; fn close(); }",
        "interface R { ;; fn read(b): int; }",
        // `extends` composition (single + multiple).
        "interface ReadWriter extends Reader, Writer {}",
        "interface Closer extends Reader { fn close() }",
        // Untyped params + no return annotation.
        "interface Sink { fn write(b) }",
        // `implements` on a class (with and without `extends`).
        "class File implements Reader { fn read(b) { return 0 } }",
        "class Socket extends Base implements Reader, Writer { fn read(b) { return 0 } fn write(b) { return 0 } }",
        // export interface.
        "export interface Reader { fn read(b): int }",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter ERROR node in: {src}"
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

#[test]
fn treesitter_parses_generics_surface() {
    // TYPE §6: type-param lists, `fn(A)->B`, user generic application, and bounds
    // all parse with no error nodes.
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");
    for src in [
        "fn id<T>(x: T): T { return x }",
        "fn map<A, B>(xs: array<A>, f: fn(A) -> B): array<B> { return [] }",
        "fn first<T, C: Container<T>>(c: C): T { return c.at(0) }",
        "class Box<T> { value: T\n fn get(): T { return self.value } }",
        "class Pair<A, B> { a: A\n b: B }",
        "enum Option<T> { Some(value: T), None }",
        // NOTE: a SECOND named-payload variant trips a PRE-EXISTING
        // variant-pattern-vs-call GLR limitation in the grammar (independent of
        // generics — `enum E { Ok(value:int), Err(error:int) }` errors on the OLD
        // grammar too), so the two-named-payload `Result2<T,E>` form is exercised by
        // the legacy/CST front-ends (frontend_conformance) rather than here.
        "enum Tagged<T> { Wrap(value: T), Empty }",
        "interface Container<T> { fn len(): int\n fn at(i: int): T }",
        "let f: fn(int) -> string = g",
        "let z: fn() -> bool = g",
        "let b: Box<int> = make()",
        "fn h(m: map<int, array<int>>) {}",
        "let bb: Box<Box<int>> = make()",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter error on generics snippet: {src}"
        );
    }
}

#[test]
fn treesitter_disambiguates_explicit_type_args_vs_comparison() {
    // TYPE §6 (Task 5/7) — the NEW expression-level `<` ambiguity. The paired
    // battery: type-arg CALLS parse with `type_arguments`; comparison chains parse
    // as comparisons (no `type_arguments`). The trailing `(` after `>` decides.
    let lang = language();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).expect("set_language");

    // TYPE-ARG-CALL readings — every one parses cleanly AND carries a
    // `type_arguments` node.
    for src in [
        "let b = Box<int>(5)",
        "let xs = map<string, number>(items, f)",
        "Box<Box<int>>(5)",
        "foo<int>(1)",
        // FnSig type argument — the `->` arrow's `>` must NOT close the angle list.
        "f<fn(int) -> string>(5)",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter error on type-arg-call: {src}"
        );
        assert!(
            tree_contains_kind(&tree, "type_arguments"),
            "expected a type_arguments node in {src:?}"
        );
    }

    // COMPARISON readings — every one parses cleanly AND has NO `type_arguments`
    // (the `<`/`>` stay comparison operators).
    for src in [
        "let _ = a < b",
        "let _ = a > b",
        "let _ = a << b",
        "let _ = a >> b",
        "let _ = a < b && c > d",
        "f(a < b, c > d)",
        "let _ = x < y ? a : b",
        "let _ = a < b > c",
        // A `(` inside the angle span that is NOT a `fn(...)` type → comparison, not a
        // generic call (`b()` is not a type).
        "let _ = a < b() > (c)",
    ] {
        let tree = parser.parse(src.as_bytes(), None).expect("parse");
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter error on comparison: {src}"
        );
        assert!(
            !tree_contains_kind(&tree, "type_arguments"),
            "comparison {src:?} wrongly parsed a type_arguments node"
        );
    }
}

/// Find the first node of a given kind, optionally requiring a named field child.
fn find_node_with_field<'a>(
    tree: &'a tree_sitter::Tree,
    kind: &str,
    field_name: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = tree.walk();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == kind && node.child_by_field_name(field_name).is_some() {
            return Some(node);
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

#[test]
fn treesitter_parses_defer_statement() {
    // DEFER §2.1 / §2.4: accepted forms parse with zero ERROR nodes AND produce a
    // `defer_statement` node containing a `call` field.
    let accepted = [
        "fn f() { defer g() }",
        "fn f() { defer obj.close() }",
        "fn f() { defer a?.flush() }",
        "fn f() { defer (cond ? a : b)() }",
        "fn f() { defer (() => { print(1) })() }",
        "fn f() { defer g(...xs) }",
        "fn f() { defer await g() }",
        "defer g()",
    ];
    for src in &accepted {
        let tree = {
            let lang = language();
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&lang).expect("set_language");
            parser.parse(src.as_bytes(), None).expect("parse")
        };
        assert!(
            !tree.root_node().has_error(),
            "tree-sitter ERROR node in accepted defer: {src}\n{}",
            tree.root_node().to_sexp()
        );
        assert!(
            find_node_with_field(&tree, "defer_statement", "call").is_some(),
            "no defer_statement[call] in: {src}\n{}",
            tree.root_node().to_sexp()
        );
    }

    // Rejected forms: non-call operands must produce an ERROR node (the grammar
    // restricts the operand to `call_expression`). `defer x` and `defer a + b`
    // are not call expressions so they cannot parse as a `defer_statement`.
    //
    // NOTE: `let defer = 5` is rejected by both hand-written parsers (legacy and
    // CST) because `defer` is a reserved keyword there. Tree-sitter's keyword
    // extraction (via `word: $ => $.identifier`) reserves `defer` in parse states
    // where the keyword and identifier lexings conflict, but tree-sitter's
    // error-recovery engine can nonetheless accept `let defer = 5` by
    // re-interpreting the token. This is a known tree-sitter limitation shared
    // with ALL reserved keywords in this grammar (e.g. `let class = 5`,
    // `let return = 5`). The hand-parser conformance in
    // `interpreter_parser_accepts_all_examples` covers the full reservation
    // check; here we only assert what tree-sitter's GLR actually enforces.
    let rejected = [
        "fn f() { defer x }",
        "fn f() { defer a + b }",
    ];
    for src in &rejected {
        let has_error = parse_has_error(src);
        assert!(
            has_error,
            "tree-sitter did NOT produce an ERROR node for rejected defer: {src}"
        );
    }
}

/// Whether any node in `tree` has the given kind.
fn tree_contains_kind(tree: &tree_sitter::Tree, kind: &str) -> bool {
    let mut cursor = tree.walk();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            return true;
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    false
}
