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
fn formats_adt_payload_enums_and_variant_patterns() {
    // ADT Task 10 (CST formatter): payload variant DECLARATIONS canonicalize their
    // field-list spacing (`Circle(radius: float)`, `Pair(int, int)`, unit `Point`);
    // variant PATTERNS re-emit compactly via the pattern's source text (the same
    // behavior every match pattern uses in the CST formatter — `{a,b}`, `[x,y]`).
    // The whole thing must be idempotent and reparse cleanly.
    let src = "enum Shape{Circle(radius:float),Rect(w:float,h:float),Pair(int,int),Point}\n\
fn area(s:Shape):float{return match s{Circle(r)=>3.14159*r*r,Rect(w: ww, h: hh)=>ww*hh,Pair(a, b)=>float(a)*float(b),Shape.Point=>0.0}}\n";
    let want = "enum Shape {\n  \
Circle(radius: float),\n  \
Rect(w: float, h: float),\n  \
Pair(int, int),\n  \
Point,\n}\n\
fn area(s: Shape): float {\n  \
return match s {\n    \
Circle(r) => 3.14159 * r * r,\n    \
Rect(w: ww, h: hh) => ww * hh,\n    \
Pair(a, b) => float(a) * float(b),\n    \
Shape.Point => 0.0,\n  }\n}\n";
    let once = ascript::syntax::format_tree(src);
    assert_eq!(once, want, "unexpected ADT format");
    let twice = ascript::syntax::format_tree(&once);
    assert_eq!(once, twice, "ADT format not idempotent");
    assert!(
        ascript::syntax::parser::parse(&once).errors.is_empty(),
        "formatted ADT output does not reparse: {once:?}"
    );
}

#[test]
fn formats_interface_decls_and_implements_clause() {
    // IFACE Task 10 (CST formatter): an `interface` declaration renders its method
    // REQUIREMENTS one-per-line (signature, NO body), the `extends` composition list
    // comma-joined, and an empty body collapses to `{\n}`. A class renders its
    // `implements A, B` clause in canonical order (after `extends`, before the body).
    // Idempotent + reparses.
    let src = "interface Reader{fn read(b:bytes):int}\n\
interface Writer{fn write(b:bytes):int;fn flush():int}\n\
interface ReadWriter extends Reader,Writer{}\n\
class File implements Reader{fn read(b:bytes):int{return 0}}\n\
class Socket extends Base implements Reader,Writer{fn read(b:bytes):int{return 0}\nfn write(b:bytes):int{return 0}}\n";
    let want = "interface Reader {\n  \
fn read(b: bytes): int\n}\n\
interface Writer {\n  \
fn write(b: bytes): int\n  \
fn flush(): int\n}\n\
interface ReadWriter extends Reader, Writer {\n}\n\
class File implements Reader {\n  \
fn read(b: bytes): int {\n    \
return 0\n  }\n}\n\
class Socket extends Base implements Reader, Writer {\n  \
fn read(b: bytes): int {\n    \
return 0\n  }\n  \
fn write(b: bytes): int {\n    \
return 0\n  }\n}\n";
    let once = ascript::syntax::format_tree(src);
    assert_eq!(once, want, "unexpected interface/implements format");
    let twice = ascript::syntax::format_tree(&once);
    assert_eq!(once, twice, "interface format not idempotent");
    assert!(
        ascript::syntax::parser::parse(&once).errors.is_empty(),
        "formatted interface output does not reparse: {once:?}"
    );
}

#[test]
fn formats_generic_type_param_lists() {
    // TYPE Task 13 (CST formatter): decl-level type-parameter lists `<T, U>` on
    // `fn`/`class`/`enum`/`interface` declarations must SURVIVE formatting (the
    // Unit-B carried-over bug dropped them — `fn id<T>(x: T)` became `fn id(x: T)`,
    // a lossy, non-idempotent rewrite). Bounds (`C: Container<T>`) render too. The
    // `fn(A) -> B` function-type and `Box<int>` generic application already round-
    // trip; this test pins them alongside the new decl-list rendering AND the
    // expression-level explicit type-arg form (`Box<string>("hi")`), which also
    // dropped its type args before. Idempotent + reparses cleanly.
    let src = "fn map<A,B>(xs:array<A>,f:fn(A)->B):array<B>{return []}\n\
class Box<T>{value:T\nfn get():T{return self.value}}\n\
enum Option<T>{Some(value:T),None}\n\
interface Container<T>{fn at(i:number):T}\n\
fn first<T,C:Container<T>>(c:C):T{return c.at(0)}\n\
let b:Box<int> = Box(5)\n\
let c = Box<string>(\"hi\")\n";
    let want = "fn map<A, B>(xs: array<A>, f: fn(A) -> B): array<B> {\n  \
return []\n}\n\
class Box<T> {\n  \
value: T\n  \
fn get(): T {\n    \
return self.value\n  }\n}\n\
enum Option<T> {\n  \
Some(value: T),\n  \
None,\n}\n\
interface Container<T> {\n  \
fn at(i: number): T\n}\n\
fn first<T, C: Container<T>>(c: C): T {\n  \
return c.at(0)\n}\n\
let b: Box<int> = Box(5)\n\
let c = Box<string>(\"hi\")\n";
    let once = ascript::syntax::format_tree(src);
    assert_eq!(once, want, "unexpected generics format");
    let twice = ascript::syntax::format_tree(&once);
    assert_eq!(once, twice, "generics format not idempotent");
    assert!(
        ascript::syntax::parser::parse(&once).errors.is_empty(),
        "formatted generics output does not reparse: {once:?}"
    );
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

#[test]
fn consecutive_comments_stay_together_no_inserted_blank() {
    // Regression (SP1 BUGFIX 1): the formatter wrongly inserted a blank line
    // between consecutive `//` comment lines (the comment line's terminating
    // newline was double-counted toward the next comment's `blank_before`).
    // Consecutive comments — including a run that precedes a declaration — must
    // stay together with NO inserted blank; genuine author blanks are preserved.
    let cases: &[(&str, &str)] = &[
        // The exact repro from the bug report: 3 comments + a decl, no blanks.
        (
            "// a\n// b\n// c\nlet x = 1\n",
            "// a\n// b\n// c\nlet x = 1\n",
        ),
        // A genuine author blank between two comments is preserved.
        ("// a\n\n// c\nlet x = 1\n", "// a\n\n// c\nlet x = 1\n"),
        // Consecutive comments leading a function declaration (the body is
        // canonicalized to multiline, but the comments stay together, no blank).
        (
            "// one\n// two\nfn f() { return 1 }\n",
            "// one\n// two\nfn f() {\n  return 1\n}\n",
        ),
        // Mixed: a blank then a run of consecutive comments.
        (
            "let a = 1\n\n// x\n// y\nlet b = 2\n",
            "let a = 1\n\n// x\n// y\nlet b = 2\n",
        ),
    ];
    for (src, expected) in cases {
        let once = ascript::syntax::format_tree(src);
        assert_eq!(&once, expected, "fmt mismatch for {src:?}");
        // …and idempotent.
        let twice = ascript::syntax::format_tree(&once);
        assert_eq!(once, twice, "not idempotent for {src:?}");
    }
}

#[test]
fn formats_defer_statement_canonical_and_idempotent() {
    // DEFER Task 4.2 (CST formatter): `defer <call>` and `defer await <call>` render
    // with a single space, args canonicalized by the existing expression renderer.
    // Idempotent + reparses cleanly.
    let cases: &[(&str, &str)] = &[
        // Excess spaces in args are normalized.
        (
            "fn f() {\ndefer   g( 1,2 )\n}\n",
            "fn f() {\n  defer g(1, 2)\n}\n",
        ),
        // `defer await` form: spaces canonicalized.
        (
            "fn f() {\ndefer  await  h()\n}\n",
            "fn f() {\n  defer await h()\n}\n",
        ),
        // Already canonical — idempotent.
        (
            "fn f() {\n  defer g()\n}\n",
            "fn f() {\n  defer g()\n}\n",
        ),
        // Member call form.
        (
            "fn f() {\ndefer src.close()\n}\n",
            "fn f() {\n  defer src.close()\n}\n",
        ),
    ];
    for (src, want) in cases {
        let once = ascript::syntax::format_tree(src);
        assert_eq!(&once, want, "unexpected defer format for {src:?}");
        let twice = ascript::syntax::format_tree(&once);
        assert_eq!(once, twice, "defer format not idempotent for {src:?}");
        assert!(
            ascript::syntax::parser::parse(&once).errors.is_empty(),
            "formatted defer output does not reparse: {once:?}"
        );
    }
}

#[test]
fn defer_with_leading_comment_survives_round_trip() {
    // DEFER Task 4.2: a comment attached to a defer statement must survive
    // formatting (the IFACE comment-attachment lesson — leading comments must
    // be explicitly tested).
    let src = "fn f() {\n  // cleanup on exit\n  defer src.close()\n}\n";
    let once = ascript::syntax::format_tree(src);
    // The comment line must be preserved.
    assert!(
        once.contains("// cleanup on exit"),
        "leading comment lost after fmt: {once:?}"
    );
    // The defer statement must still be present.
    assert!(
        once.contains("defer src.close()"),
        "defer statement lost after fmt: {once:?}"
    );
    // Idempotent.
    let twice = ascript::syntax::format_tree(&once);
    assert_eq!(once, twice, "defer+comment not idempotent: {once:?}");
    // Reparses cleanly.
    assert!(
        ascript::syntax::parser::parse(&once).errors.is_empty(),
        "formatted defer+comment does not reparse: {once:?}"
    );
}
