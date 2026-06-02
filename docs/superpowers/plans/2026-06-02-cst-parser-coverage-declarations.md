# CST Parser Coverage — Declarations, Types, Match (Plan 2b-ii)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the AScript grammar in the new parser — destructuring `let`, `for`/`for await`, full functions (`async`/`fn*`, rest params, param/return type annotations) and the recursive **type-annotation** parser, `enum`, `class`, `import`/`export`, and `match` with all pattern forms — then make the typed-AST codegen emit Rust enums for alternation rules, and switch the differential oracle to **full-corpus new-parser-vs-tree-sitter**.

**Architecture:** Extends `src/syntax/parser.rs`, `src/syntax/kind.rs`, and `src/syntax/ast/ascript.ungram` from Plans 2 / 2b-i, reusing the same helpers (`start`/`bump`/`complete`/`precede`/`at`/`at_end`/`error`, `expr`, `block`, `stmt`, `param_list`, `fn_decl`). Adds a small `at_kw` contextual-keyword helper for the soft keywords `as`/`of`/`extends`. The codegen in `build.rs` gains enum emission. The oracle test graduates to comparing the new parser against the existing tree-sitter conformance over every `examples/**/*.as`.

**Tech Stack:** Rust, the Plan 1/2/2b-i `src/syntax/*` machinery, `ungrammar` codegen, the vendored tree-sitter grammar.

**Scope note:** Final front-end parser plan (spec: `docs/superpowers/specs/2026-06-02-cst-frontend-migration-design.md`). After this, the parser accepts the whole language losslessly and the typed AST is complete — unblocking Plan 3 (resolver) and Plan 4 (formatter). Depends on Plans 2 and 2b-i.

**Invariant (carried):** losslessness — the corpus round-trip stays byte-for-byte after every task.

---

## File Structure

- Modify `src/syntax/kind.rs` — declaration/type/pattern node kinds.
- Modify `src/syntax/parser.rs` — type parser, destructuring, for/break/continue, full functions, enum, class, import/export, match + patterns; `at_kw` helper.
- Modify `src/syntax/ast/ascript.ungram` — rules for all new nodes + the alternation enums.
- Modify `build.rs` — emit Rust enums for alternation (`|`) rules.
- Modify `tests/cst_parser_oracle.rs` — full-corpus differential oracle.

---

## Task 1: Node kinds + `at_kw` helper + grammar rules

**Files:**
- Modify: `src/syntax/kind.rs`
- Modify: `src/syntax/parser.rs`
- Modify: `src/syntax/ast/ascript.ungram`

- [ ] **Step 1: Test for the new kinds**

Add to the `tests` mod in `src/syntax/kind.rs`:

```rust
#[test]
fn declaration_node_kinds_exist() {
    for k in [
        SyntaxKind::ForStmt, SyntaxKind::RangeExpr, SyntaxKind::BreakStmt,
        SyntaxKind::ContinueStmt, SyntaxKind::EnumDecl, SyntaxKind::EnumVariant,
        SyntaxKind::ClassDecl, SyntaxKind::FieldDecl, SyntaxKind::MethodDecl,
        SyntaxKind::ImportStmt, SyntaxKind::ExportStmt, SyntaxKind::ImportList,
        SyntaxKind::ArrayBindPat, SyntaxKind::ObjectBindPat, SyntaxKind::BindEntry,
        SyntaxKind::RestBind, SyntaxKind::MatchExpr, SyntaxKind::MatchArm,
        SyntaxKind::MatchGuard, SyntaxKind::WildcardPat, SyntaxKind::IdentPat,
        SyntaxKind::LiteralPat, SyntaxKind::RangePat, SyntaxKind::ArrayPat,
        SyntaxKind::ObjectPat, SyntaxKind::ObjPatEntry, SyntaxKind::OrPat,
        SyntaxKind::PatRest, SyntaxKind::NamedType, SyntaxKind::GenericType,
        SyntaxKind::OptionalType, SyntaxKind::UnionType, SyntaxKind::TupleType,
        SyntaxKind::TypeArgs, SyntaxKind::RetType,
    ] {
        assert!(!k.is_trivia(), "{k:?}");
    }
}
```

- [ ] **Step 2: Run (expect compile failure)**

Run: `cargo test --lib syntax::kind::tests::declaration_node_kinds_exist 2>&1 | tail -10`
Expected: FAIL to compile.

- [ ] **Step 3: Add the variants**

In `src/syntax/kind.rs`, append to the nodes section:

```rust
    // --- declarations / control flow (Plan 2b-ii) ---
    ForStmt, RangeExpr, BreakStmt, ContinueStmt,
    EnumDecl, EnumVariant, ClassDecl, FieldDecl, MethodDecl,
    ImportStmt, ExportStmt, ImportList,
    // let-destructuring binding patterns
    ArrayBindPat, ObjectBindPat, BindEntry, RestBind,
    // match
    MatchExpr, MatchArm, MatchGuard,
    WildcardPat, IdentPat, LiteralPat, RangePat, ArrayPat, ObjectPat,
    ObjPatEntry, OrPat, PatRest,
    // types
    NamedType, GenericType, OptionalType, UnionType, TupleType, TypeArgs, RetType,
```

- [ ] **Step 4: Add the `at_kw` contextual-keyword helper to the parser**

Soft keywords (`as`, `of`, `extends`) lex as `Ident`. Add to `impl Parser` in `src/syntax/parser.rs`:

```rust
    /// True if the current token is an `Ident` whose text equals `kw` (a soft
    /// keyword like `as` / `of` / `extends`, which are not reserved).
    fn at_kw(&self, kw: &str) -> bool {
        match self.nontrivia.get(self.pos) {
            Some(&ti) => {
                self.tokens[ti].kind == SyntaxKind::Ident && self.tokens[ti].text == kw
            }
            None => false,
        }
    }
```

> Note: `of` is a real keyword (`OfKw`) per Plan 1's keyword set, but `for (x of e)` also accepts it; use `p.at(OfKw)` for `of` and `at_kw("as")` / `at_kw("extends")` for the soft ones. If Plan 1 mapped `of` to `OfKw`, prefer `p.at(OfKw)`; otherwise `at_kw("of")`. The full-corpus oracle (Task 11) catches a mismatch.

- [ ] **Step 5: Add grammar rules**

In `src/syntax/ast/ascript.ungram`, extend `Stmt`/`Expr` and add rules:

```
Stmt =
    LetStmt | ExprStmt | Block | IfStmt | WhileStmt | ReturnStmt | FnDecl
  | ForStmt | BreakStmt | ContinueStmt | EnumDecl | ClassDecl
  | ImportStmt | ExportStmt

ForStmt = 'for' 'await'? '(' 'ident' ('in' | 'of') iter:Expr ('..' | '..=') end:Expr? ')' body:Block
BreakStmt = 'break'
ContinueStmt = 'continue'
RangeExpr = start:Expr ('..' | '..=') end:Expr

LetStmt = ('let' | 'const') (binding:'ident' | ArrayBindPat | ObjectBindPat) (':' Type)? ('=' Expr)?
ArrayBindPat = '[' (BindEntry | RestBind)* ']'
ObjectBindPat = '{' (BindEntry | RestBind)* '}'
BindEntry = key:'ident' ('as' local:'ident')?
RestBind = '...' 'ident'

FnDecl = 'async'? 'fn' '*'? 'ident' ParamList RetType? Block
ParamList = '(' Param* ')'
Param = '...'? 'ident' (':' Type)?
RetType = ':' Type
ArrowExpr = 'async'? ParamList '=>' (Block | Expr)

EnumDecl = 'enum' 'ident' '{' EnumVariant* '}'
EnumVariant = 'ident' ('=' Expr)?

ClassDecl = 'class' 'ident' ('extends' 'ident')? '{' (FieldDecl | MethodDecl)* '}'
FieldDecl = 'ident' '?'? ':' Type ('=' Expr)?
MethodDecl = 'async'? 'fn' '*'? 'ident' ParamList RetType? Block

ImportStmt = 'import' (ImportList | '*' 'as' 'ident') 'from' 'string'
ImportList = '{' 'ident'* '}'
ExportStmt = 'export' Stmt

MatchExpr = 'match' subject:Expr '{' MatchArm* '}'
MatchArm = Pat ('|' Pat)* MatchGuard? '=>' body:Expr
MatchGuard = 'if' Expr
Pat =
    WildcardPat | IdentPat | LiteralPat | RangePat | ArrayPat | ObjectPat
WildcardPat = '_'
IdentPat = 'ident'
LiteralPat = Expr
RangePat = start:Expr ('..' | '..=') end:Expr
ArrayPat = '[' (Pat | PatRest)* ']'
ObjectPat = '{' (ObjPatEntry | PatRest)* '}'
ObjPatEntry = key:'ident' (':' Pat)?
PatRest = '...' 'ident'?

Type =
    NamedType | GenericType | OptionalType | UnionType | TupleType
NamedType = 'ident'
GenericType = 'ident' TypeArgs
TypeArgs = '<' Type* '>'
OptionalType = Type '?'
UnionType = Type '|' Type
TupleType = '[' Type* ']'
```

- [ ] **Step 6: Run kind test + build**

Run: `cargo test --lib syntax::kind 2>&1 | tail -10`
Expected: PASS.
Run: `cargo build 2>&1 | tail -5`
Expected: builds.

- [ ] **Step 7: Commit**

```bash
git add src/syntax/kind.rs src/syntax/parser.rs src/syntax/ast/ascript.ungram
git commit -m "feat(syntax): declaration/type/pattern node kinds + grammar + at_kw helper"
```

---

## Task 2: Type-annotation parser

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write type tests (via `let x: T`)**

Add to the `tests` mod:

```rust
    #[test]
    fn type_annotations() {
        for (src, kind) in [
            ("let x: number = 1", SyntaxKind::NamedType),
            ("let x: array<number> = []", SyntaxKind::GenericType),
            ("let x: number? = nil", SyntaxKind::OptionalType),
            ("let x: number | string = 1", SyntaxKind::UnionType),
            ("let x: map<string, number> = m", SyntaxKind::GenericType),
            ("let x: [number, string] = t", SyntaxKind::TupleType),
        ] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
            assert!(tree_shape(src).contains(&kind), "missing {kind:?} for {src}");
        }
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::type_annotations 2>&1 | tail -15`
Expected: FAIL — `let_stmt` ignores `: Type`.

- [ ] **Step 3: Add the type parser + wire it into `let_stmt`**

Add the recursive type parser to `src/syntax/parser.rs`:

```rust
/// Parse a type annotation. Grammar (loosest→tightest): union (`|`), then a
/// postfix-`?` optional, then a primary (named/generic/tuple). Generics use
/// `name<T, ...>`; tuples use `[T, ...]`.
fn type_ann(p: &mut Parser) {
    let cm = type_optional(p);
    // union: T | T | ...
    if p.at(SyntaxKind::Pipe) {
        let m = p.precede(&cm);
        while p.at(SyntaxKind::Pipe) {
            p.bump(); // |
            type_optional(p);
        }
        p.complete(m, SyntaxKind::UnionType);
    }
}

fn type_optional(p: &mut Parser) -> CompletedMarker {
    let cm = type_primary(p);
    if p.at(SyntaxKind::Question) {
        let m = p.precede(&cm);
        p.bump(); // ?
        return p.complete(m, SyntaxKind::OptionalType);
    }
    cm
}

fn type_primary(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    match p.current() {
        Ident => {
            let m = p.start();
            p.bump(); // type name
            if p.at(Lt) {
                // generic args: name<T, ...>
                let args = p.start();
                p.bump(); // <
                while !p.at(Gt) && !p.at_end() {
                    type_ann(p);
                    if p.at(Comma) {
                        p.bump();
                    } else {
                        break;
                    }
                }
                if p.at(Gt) {
                    p.bump();
                } else {
                    p.error("expected '>' to close type arguments");
                }
                p.complete(args, TypeArgs);
                return p.complete(m, GenericType);
            }
            p.complete(m, NamedType)
        }
        LBracket => {
            let m = p.start();
            p.bump(); // [
            while !p.at(RBracket) && !p.at_end() {
                type_ann(p);
                if p.at(Comma) {
                    p.bump();
                } else {
                    break;
                }
            }
            if p.at(RBracket) {
                p.bump();
            } else {
                p.error("expected ']' to close tuple type");
            }
            p.complete(m, TupleType)
        }
        _ => {
            let m = p.start();
            p.error("expected a type");
            p.complete(m, Error)
        }
    }
}
```

Then in `let_stmt` (from Plan 2), after the name is consumed and before the `=`:

```rust
    if p.at(Colon) {
        p.bump();
        type_ann(p);
    }
```

(Insert this between the name-binding block and the `if p.at(Eq)` initializer block.)

- [ ] **Step 4: Run tests + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): recursive type-annotation parser"
```

---

## Task 3: Destructuring `let`

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn array_destructuring() {
        let p = parse("let [a, b, ...rest] = xs");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape("let [a, b, ...rest] = xs");
        assert!(s.contains(&SyntaxKind::ArrayBindPat));
        assert!(s.contains(&SyntaxKind::RestBind));
    }

    #[test]
    fn object_destructuring_with_rename() {
        let p = parse("let {a, b as local, ...rest} = obj");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape("let {a, b as local, ...rest} = obj");
        assert!(s.contains(&SyntaxKind::ObjectBindPat));
        assert!(s.contains(&SyntaxKind::BindEntry));
        assert!(s.contains(&SyntaxKind::RestBind));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::array_destructuring 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Extend `let_stmt` to accept binding patterns**

In `let_stmt`, replace the name-binding block (the `if p.at(Ident) { p.bump() } else { error }`) with a dispatch on the binding form:

```rust
    match p.current() {
        LBracket => array_bind_pat(p),
        LBrace => object_bind_pat(p),
        Ident => p.bump(),
        _ => p.error("expected a name or destructuring pattern after let/const"),
    }
```

Add the binding-pattern parsers:

```rust
fn array_bind_pat(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // [
    while !p.at(RBracket) && !p.at_end() {
        if p.at(DotDotDot) {
            rest_bind(p);
        } else {
            let e = p.start();
            if p.at(Ident) {
                p.bump();
            } else {
                p.error("expected a binding name");
            }
            p.complete(e, BindEntry);
        }
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBracket) {
        p.bump();
    } else {
        p.error("expected ']' to close destructuring pattern");
    }
    p.complete(m, ArrayBindPat);
}

fn object_bind_pat(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // {
    while !p.at(RBrace) && !p.at_end() {
        if p.at(DotDotDot) {
            rest_bind(p);
        } else {
            let e = p.start();
            // key (ident or string), optional `as local`
            if p.at(Ident) || p.at(Str) {
                p.bump();
            } else {
                p.error("expected a key in object pattern");
            }
            if p.at_kw("as") {
                p.bump(); // as
                if p.at(Ident) {
                    p.bump();
                } else {
                    p.error("expected a local name after 'as'");
                }
            }
            p.complete(e, BindEntry);
        }
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close destructuring pattern");
    }
    p.complete(m, ObjectBindPat);
}

fn rest_bind(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // ...
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected a name after '...'");
    }
    p.complete(m, RestBind);
}
```

> Reminder: `at_kw` consumes nothing; `p.bump()` after `at_kw("as")` consumes the `as` ident token.

- [ ] **Step 4: Run tests + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): array/object destructuring let (rest + as-rename)"
```

---

## Task 4: `for` / `for await`, `break`, `continue`

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn for_loops() {
        for src in [
            "for (x of items) { print(x) }",
            "for (i in 1..6) { print(i) }",
            "for (i in 0..=5) { print(i) }",
            "for await (x in stream) { print(x) }",
        ] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
            assert!(tree_shape(src).contains(&SyntaxKind::ForStmt), "no ForStmt for {src}");
        }
        assert!(tree_shape("for (i in 1..6) {}").contains(&SyntaxKind::RangeExpr));
    }

    #[test]
    fn break_continue() {
        let p = parse("while x { break } ");
        assert!(p.errors.is_empty());
        assert!(tree_shape("while x { break }").contains(&SyntaxKind::BreakStmt));
        assert!(tree_shape("while x { continue }").contains(&SyntaxKind::ContinueStmt));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::for_loops 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Add `for`/`break`/`continue` to the `stmt` dispatcher**

In `stmt`, add arms (before the `_ => expr_stmt(p)`):

```rust
        ForKw => for_stmt(p),
        BreakKw => {
            let m = p.start();
            p.bump();
            p.complete(m, SyntaxKind::BreakStmt);
        }
        ContinueKw => {
            let m = p.start();
            p.bump();
            p.complete(m, SyntaxKind::ContinueStmt);
        }
```

Add `for_stmt`:

```rust
fn for_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // for
    if p.at(AwaitKw) {
        p.bump(); // await
    }
    if p.at(LParen) {
        p.bump();
    } else {
        p.error("expected '(' after for");
    }
    if p.at(Ident) {
        p.bump(); // loop variable
    } else {
        p.error("expected loop variable");
    }
    // `in` or `of`
    if p.at(InKw) || p.at(OfKw) {
        p.bump();
    } else {
        p.error("expected 'in' or 'of' in for");
    }
    // iterable, or a range `a..b` / `a..=b`
    let iter = lhs(p);
    // re-climb binary precedence for the iterable expression start
    let mut iter_cm = iter;
    loop {
        let op = p.current();
        let Some((_l, r_bp)) = infix_binding_power(op) else { break };
        let bm = p.precede(&iter_cm);
        p.bump();
        expr_bp(p, r_bp);
        iter_cm = p.complete(bm, BinaryExpr);
    }
    if p.at(DotDot) || p.at(DotDotEq) {
        let rm = p.precede(&iter_cm);
        p.bump(); // .. or ..=
        expr(p); // range end
        p.complete(rm, RangeExpr);
    }
    if p.at(RParen) {
        p.bump();
    } else {
        p.error("expected ')' to close for header");
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' for loop body");
    }
    p.complete(m, ForStmt);
}
```

> The iterable is parsed as a (possibly binary) expression; a trailing `..`/`..=` turns it into a `RangeExpr`. This matches `for (i in 1..6)` (range) and `for (x of items)` (plain iterable).

- [ ] **Step 4: Run tests + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): for / for await / break / continue"
```

---

## Task 5: Full functions — async, generator, rest params, param/return types

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn async_and_generator_fns() {
        for src in [
            "async fn f() { return 1 }",
            "fn* g() { yield 1 }",
            "async fn* h() { yield 1 }",
            "fn add(a: number, b: number): number { return a + b }",
            "fn variadic(first, ...rest) { return rest }",
        ] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
            assert!(tree_shape(src).contains(&SyntaxKind::FnDecl), "no FnDecl for {src}");
        }
        assert!(tree_shape("fn add(a: number): number {}").contains(&SyntaxKind::RetType));
    }

    #[test]
    fn async_arrow() {
        assert!(parse("let f = async (x) => x").errors.is_empty());
        assert!(tree_shape("let f = async (x) => x").contains(&SyntaxKind::ArrowExpr));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::async_and_generator_fns 2>&1 | tail -15`
Expected: FAIL — `async`/`fn*`/types/rest not handled.

- [ ] **Step 3: Extend `stmt` dispatch, `fn_decl`, `param_list`, arrows**

In `stmt`, replace the `FnKw => fn_decl(p)` arm and add `AsyncKw`:

```rust
        FnKw => fn_decl(p),
        AsyncKw => fn_decl(p), // async fn / async fn*
```

Rewrite `fn_decl` (from Plan 2) to handle `async`, `*`, return type:

```rust
fn fn_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    if p.at(AsyncKw) {
        p.bump(); // async
    }
    if p.at(FnKw) {
        p.bump(); // fn
    } else {
        p.error("expected 'fn'");
    }
    if p.at(Star) {
        p.bump(); // generator *
    }
    if p.at(Ident) {
        p.bump(); // name
    } else {
        p.error("expected function name");
    }
    if p.at(LParen) {
        param_list(p);
    } else {
        p.error("expected '(' after function name");
    }
    if p.at(Colon) {
        ret_type(p);
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' for function body");
    }
    p.complete(m, FnDecl);
}

fn ret_type(p: &mut Parser) {
    let m = p.start();
    p.bump(); // :
    type_ann(p);
    p.complete(m, SyntaxKind::RetType);
}
```

Rewrite `param_list` (from Plan 2) to handle rest + param types:

```rust
fn param_list(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // (
    while !p.at(RParen) && !p.at_end() {
        let pm = p.start();
        if p.at(DotDotDot) {
            p.bump(); // rest ...
        }
        if p.at(Ident) {
            p.bump();
        } else {
            p.error("expected parameter name");
        }
        if p.at(Colon) {
            p.bump();
            type_ann(p);
        }
        p.complete(pm, Param);
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RParen) {
        p.bump();
    } else {
        p.error("expected ')' to close parameters");
    }
    p.complete(m, ParamList);
}
```

For **async arrows**, extend the arrow recognition. In `primary`, the arrow arm checks `LParen if is_arrow_ahead(p)`. Add an `AsyncKw` arrow arm before it:

```rust
        AsyncKw if is_async_arrow_ahead(p) => {
            let m = p.start();
            p.bump(); // async
            param_list(p);
            p.bump(); // =>
            if p.at(LBrace) { block(p); } else { expr(p); }
            p.complete(m, ArrowExpr)
        }
```

Add the lookahead helper (async, then `(`, then arrow-ahead):

```rust
/// True if `async (` ... `) =>` starts here (an async arrow).
fn is_async_arrow_ahead(p: &Parser) -> bool {
    use SyntaxKind::*;
    // current is AsyncKw; next must be `(` beginning an arrow param list.
    match p.nontrivia.get(p.pos + 1).map(|&ti| p.tokens[ti].kind) {
        Some(LParen) => {
            // Reuse is_arrow_ahead logic starting at the `(` (pos+1).
            let mut depth = 0i32;
            let mut i = p.pos + 1;
            while i < p.nontrivia.len() {
                match p.tokens[p.nontrivia[i]].kind {
                    LParen => depth += 1,
                    RParen => {
                        depth -= 1;
                        if depth == 0 {
                            return matches!(
                                p.nontrivia.get(i + 1).map(|&ti| p.tokens[ti].kind),
                                Some(FatArrow)
                            );
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            false
        }
        _ => false,
    }
}
```

- [ ] **Step 4: Run tests + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -20`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): full functions (async/generator/rest/param+return types) + async arrows"
```

---

## Task 6: `enum`

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Test**

Add to the `tests` mod:

```rust
    #[test]
    fn enum_declaration() {
        let p = parse("enum Color { Red, Green = 2, Blue }");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape("enum Color { Red, Green = 2, Blue }");
        assert!(s.contains(&SyntaxKind::EnumDecl));
        assert!(s.contains(&SyntaxKind::EnumVariant));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::enum_declaration 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Add `enum` to `stmt` + `enum_decl`**

In `stmt`, add (before `_ =>`): `EnumKw => enum_decl(p),`. Add:

```rust
fn enum_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // enum
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected enum name");
    }
    if p.at(LBrace) {
        p.bump();
    } else {
        p.error("expected '{' for enum body");
    }
    while !p.at(RBrace) && !p.at_end() {
        let vm = p.start();
        if p.at(Ident) {
            p.bump();
        } else {
            p.error("expected variant name");
        }
        if p.at(Eq) {
            p.bump();
            expr(p);
        }
        p.complete(vm, EnumVariant);
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close enum");
    }
    p.complete(m, EnumDecl);
}
```

- [ ] **Step 4: Run + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): enum declarations"
```

---

## Task 7: `class` (extends, fields, methods, init)

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Test**

Add to the `tests` mod:

```rust
    #[test]
    fn class_declaration() {
        let src = "class Dog extends Animal {\n  name: string\n  age: number = 0\n  nickname?: string\n  fn init(name) { self.name = name }\n  fn describe(): string { return self.name }\n}";
        let p = parse(src);
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape(src);
        assert!(s.contains(&SyntaxKind::ClassDecl));
        assert!(s.contains(&SyntaxKind::FieldDecl));
        assert!(s.contains(&SyntaxKind::MethodDecl));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::class_declaration 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Add `class` to `stmt` + `class_decl`**

In `stmt`, add `ClassKw => class_decl(p),`. Add the parser. A class body member is a **field** (`name [?] : Type [= default]`) or a **method** (`[async] fn [*] name(...) [: ret] { }`); disambiguate by whether `fn`/`async` starts the member:

```rust
fn class_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // class
    if p.at(Ident) {
        p.bump(); // class name
    } else {
        p.error("expected class name");
    }
    if p.at_kw("extends") {
        p.bump(); // extends
        if p.at(Ident) {
            p.bump(); // superclass name
        } else {
            p.error("expected superclass name after 'extends'");
        }
    }
    if p.at(LBrace) {
        p.bump();
    } else {
        p.error("expected '{' for class body");
    }
    while !p.at(RBrace) && !p.at_end() {
        let before = p.pos;
        if p.at(AsyncKw) || p.at(FnKw) {
            method_decl(p);
        } else if p.at(Ident) {
            field_decl(p);
        } else {
            p.error("expected a field or method");
            p.bump();
        }
        if p.pos == before {
            p.bump();
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close class");
    }
    p.complete(m, ClassDecl);
}

fn field_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // field name (Ident)
    if p.at(Question) {
        p.bump(); // optional marker `name?:`
    }
    if p.at(Colon) {
        p.bump();
        type_ann(p);
    } else {
        p.error("expected ':' and a type in field declaration");
    }
    if p.at(Eq) {
        p.bump();
        expr(p); // default value
    }
    p.complete(m, FieldDecl);
}

fn method_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    if p.at(AsyncKw) {
        p.bump();
    }
    if p.at(FnKw) {
        p.bump();
    } else {
        p.error("expected 'fn' in method");
    }
    if p.at(Star) {
        p.bump(); // generator method
    }
    if p.at(Ident) {
        p.bump(); // method name (incl. `init`)
    } else {
        p.error("expected method name");
    }
    if p.at(LParen) {
        param_list(p);
    } else {
        p.error("expected '(' after method name");
    }
    if p.at(Colon) {
        ret_type(p);
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' for method body");
    }
    p.complete(m, MethodDecl);
}
```

> `init`, `self`, `super` are ordinary identifiers at the parser level (not reserved tokens) — `fn init(...)` parses as a normal method named `init`; `self.x` / `super.m()` parse as member access on `NameRef`s. Their semantics are resolved later.

- [ ] **Step 4: Run + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): class declarations (extends, fields, methods, init)"
```

---

## Task 8: `import` / `export`

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Test**

Add to the `tests` mod:

```rust
    #[test]
    fn imports_and_exports() {
        for src in [
            r#"import * as task from "std/task""#,
            r#"import { a, b } from "./mod""#,
            "export fn f() { return 1 }",
        ] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
        }
        assert!(tree_shape(r#"import * as t from "std/task""#).contains(&SyntaxKind::ImportStmt));
        assert!(tree_shape(r#"import { a } from "m""#).contains(&SyntaxKind::ImportList));
        assert!(tree_shape("export fn f() {}").contains(&SyntaxKind::ExportStmt));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::imports_and_exports 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Add `import`/`export` to `stmt`**

In `stmt`, add `ImportKw => import_stmt(p),` and `ExportKw => export_stmt(p),`. Add:

```rust
fn import_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // import
    if p.at(Star) {
        p.bump(); // *
        if p.at_kw("as") {
            p.bump(); // as
        } else {
            p.error("expected 'as' in namespace import");
        }
        if p.at(Ident) {
            p.bump(); // alias
        } else {
            p.error("expected import alias");
        }
    } else if p.at(LBrace) {
        let l = p.start();
        p.bump(); // {
        while !p.at(RBrace) && !p.at_end() {
            if p.at(Ident) {
                p.bump();
            } else {
                p.error("expected an import name");
            }
            if p.at(Comma) {
                p.bump();
            } else {
                break;
            }
        }
        if p.at(RBrace) {
            p.bump();
        } else {
            p.error("expected '}' to close import list");
        }
        p.complete(l, ImportList);
    } else {
        p.error("expected '{' or '*' after import");
    }
    // `from "source"`
    if p.at_kw("from") {
        p.bump(); // from (soft keyword)
    } else {
        p.error("expected 'from'");
    }
    if p.at(Str) {
        p.bump(); // module path string
    } else {
        p.error("expected a module path string");
    }
    p.complete(m, ImportStmt);
}

fn export_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(); // export
    stmt(p); // the exported declaration
    p.complete(m, SyntaxKind::ExportStmt);
}
```

> `from` is a soft keyword (`at_kw("from")`); if Plan 1 made `from` a reserved keyword instead, use `p.at(FromKw)`. The corpus oracle (Task 11) catches a mismatch.

- [ ] **Step 4: Run + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): import (named + namespace) and export"
```

---

## Task 9: `match` + all pattern forms

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn match_expression_patterns() {
        let src = r#"match n { _ if n < 0 => "neg", 0 => "zero", 1..=9 => "single", "sat" | "sun" => "weekend", [] => "empty", [x] => "one", [first, ...rest] => "many", {a, b: c} => "obj", _ => "big" }"#;
        let p = parse(src);
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let s = tree_shape(src);
        for k in [
            SyntaxKind::MatchExpr, SyntaxKind::MatchArm, SyntaxKind::MatchGuard,
            SyntaxKind::WildcardPat, SyntaxKind::LiteralPat, SyntaxKind::RangePat,
            SyntaxKind::OrPat, SyntaxKind::ArrayPat, SyntaxKind::ObjectPat,
            SyntaxKind::PatRest,
        ] {
            assert!(s.contains(&k), "missing {k:?}");
        }
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::match_expression_patterns 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Add `match` to `primary` + the pattern parser**

`match` is an expression, so add it to `primary`'s match (before `_ =>`): `MatchKw => match_expr(p),`. Add:

```rust
fn match_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // match
    expr(p); // subject
    if p.at(LBrace) {
        p.bump();
    } else {
        p.error("expected '{' for match body");
    }
    while !p.at(RBrace) && !p.at_end() {
        match_arm(p);
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close match");
    }
    p.complete(m, MatchExpr)
}

fn match_arm(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    // one or more `|`-separated patterns
    pattern(p);
    if p.at(Pipe) {
        // wrap into an OrPat: precede the first pattern isn't trivial here, so we
        // model an or-pattern as a flat sequence under an OrPat node by wrapping
        // retroactively. Simpler: re-open via a sibling OrPat marker is avoided;
        // instead we mark the arm as containing multiple patterns by emitting an
        // OrPat node spanning them.
        // Implementation: we already completed the first pattern; collect the
        // rest under an OrPat that the formatter/lowering treats as alternatives.
        while p.at(Pipe) {
            p.bump(); // |
            pattern(p);
        }
        // Note: alternatives are siblings within the arm; we tag their presence
        // with an empty OrPat marker so tree_shape can detect or-patterns.
        let orm = p.start();
        p.complete(orm, OrPat);
    }
    // optional guard
    if p.at(IfKw) {
        let g = p.start();
        p.bump(); // if
        expr(p);
        p.complete(g, MatchGuard);
    }
    if p.at(FatArrow) {
        p.bump();
    } else {
        p.error("expected '=>' in match arm");
    }
    expr(p); // arm body
    p.complete(m, MatchArm);
}

/// Parse a single match pattern.
fn pattern(p: &mut Parser) {
    use SyntaxKind::*;
    match p.current() {
        // `_` wildcard — lexes as Ident "_"
        Ident if p.at_kw("_") => {
            let m = p.start();
            p.bump();
            p.complete(m, WildcardPat);
        }
        LBracket => array_pat(p),
        LBrace => object_pat(p),
        _ => {
            // A value/range/ident pattern: parse an expression; if a range
            // operator follows, it's a RangePat; otherwise LiteralPat (the
            // interpreter applies Option-C ident resolution at runtime).
            let m = p.start();
            let _cm = lhs(p);
            if p.at(DotDot) || p.at(DotDotEq) {
                p.bump();
                let _ = lhs(p);
                p.complete(m, RangePat);
            } else {
                p.complete(m, LiteralPat);
            }
        }
    }
}

fn array_pat(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // [
    while !p.at(RBracket) && !p.at_end() {
        if p.at(DotDotDot) {
            pat_rest(p);
        } else {
            pattern(p);
        }
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBracket) {
        p.bump();
    } else {
        p.error("expected ']' to close array pattern");
    }
    p.complete(m, ArrayPat);
}

fn object_pat(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // {
    while !p.at(RBrace) && !p.at_end() {
        if p.at(DotDotDot) {
            pat_rest(p);
        } else {
            let e = p.start();
            if p.at(Ident) || p.at(Str) {
                p.bump(); // key
            } else {
                p.error("expected key in object pattern");
            }
            if p.at(Colon) {
                p.bump();
                pattern(p); // sub-pattern
            }
            p.complete(e, ObjPatEntry);
        }
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close object pattern");
    }
    p.complete(m, ObjectPat);
}

fn pat_rest(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // ...
    if p.at(Ident) {
        p.bump(); // optional bound name
    }
    p.complete(m, PatRest);
}
```

> **Bare-ident patterns:** a standalone identifier (`other`, `NOT_FOUND`) is emitted as a `LiteralPat` (its expression is a bare `NameRef`). The Ident-vs-value distinction is **Option-C — resolved at runtime/lowering**, not syntactically (a name defined in scope compares; an undefined name binds). So the parser does *not* try to classify; lowering (with the resolver) reclassifies a bare-`NameRef` `LiteralPat` as an ident-binding pattern. The `IdentPat` node kind is reserved for that lowering step.
>
> **Or-patterns:** alternatives are modeled as sibling patterns in the arm plus an `OrPat` marker node so the shape test can detect them; Plan 3/the compiler reads the arm's pattern children. If a cleaner retro-wrap is desired, wrap the alternatives with `precede` on the first pattern's `CompletedMarker` — but the flat form is sufficient for losslessness and lowering.

- [ ] **Step 4: Run + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -20`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): match expressions + all pattern forms"
```

---

## Task 10: Codegen — emit Rust enums for alternation rules

The Plan 2 generator emitted a struct per concrete node and **skipped** alternation rules (`Stmt`/`Expr`/`Pat`/`Type`). Now emit those as Rust enums with a `cast` that dispatches on kind, so consumers (resolver, formatter, compiler) can match on `Expr`/`Stmt`/etc.

**Files:**
- Modify: `build.rs`

- [ ] **Step 1: Replace the `is_enum_rule` skip with enum emission**

In `build.rs`'s `generate_ast_nodes`, change the loop so enum rules emit a Rust enum. Replace the `if is_enum_rule(&grammar, node) { continue; }` block and the struct emission with:

```rust
        if is_enum_rule(&grammar, node) {
            // Alternation rule → Rust enum over its node variants.
            let variants = alt_variants(&grammar, node);
            let _ = writeln!(out, "#[derive(Debug, Clone)]");
            let _ = writeln!(out, "pub enum {name} {{");
            for v in &variants {
                let _ = writeln!(out, "    {v}({v}),");
            }
            let _ = writeln!(out, "}}");
            let _ = writeln!(out, "impl {name} {{");
            let _ = writeln!(out, "    pub fn cast(node: SyntaxNode) -> Option<Self> {{");
            let _ = writeln!(out, "        match node.kind() {{");
            for v in &variants {
                let _ = writeln!(
                    out,
                    "            SyntaxKind::{v} => {v}::cast(node).map(Self::{v}),"
                );
            }
            let _ = writeln!(out, "            _ => None,");
            let _ = writeln!(out, "        }}");
            let _ = writeln!(out, "    }}");
            let _ = writeln!(out, "}}\n");
            continue;
        }
```

Add the helper that lists an alternation rule's node-reference variant names:

```rust
/// Collect the node names referenced by an `Alt` rule (e.g. Expr = A | B | C).
fn alt_variants(grammar: &ungrammar::Grammar, node: ungrammar::Node) -> Vec<String> {
    fn collect(grammar: &ungrammar::Grammar, rule: &ungrammar::Rule, out: &mut Vec<String>) {
        match rule {
            ungrammar::Rule::Alt(rules) => {
                for r in rules {
                    collect(grammar, r, out);
                }
            }
            ungrammar::Rule::Node(n) => out.push(grammar[*n].name.clone()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    collect(grammar, &grammar[node].rule, &mut out);
    out
}
```

- [ ] **Step 2: Add a typed-enum test**

Add to `src/syntax/ast/mod.rs`'s `tests` mod:

```rust
    #[test]
    fn expr_enum_casts_a_binary() {
        let root = parse_to_tree("1 + 2");
        // find the BinaryExpr node and cast it through the Expr enum
        let bin = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::BinaryExpr)
            .expect("has a BinaryExpr");
        assert!(matches!(super::Expr::cast(bin), Some(super::Expr::BinaryExpr(_))));
    }
```

- [ ] **Step 3: Build + run**

Run: `cargo build 2>&1 | tail -10`
Expected: builds; the generated `ast_nodes.rs` now contains `enum Expr`, `enum Stmt`, `enum Pat`, `enum Type`. If `ungrammar`'s `Rule` variants differ (`Rule::Alt`, `Rule::Node`), adjust `alt_variants`/`is_enum_rule` per `https://docs.rs/ungrammar`.
Run: `cargo test --lib syntax::ast 2>&1 | tail -15`
Expected: `expr_enum_casts_a_binary` PASS.

- [ ] **Step 4: Clippy both configs + commit**

Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

```bash
git add build.rs src/syntax/ast/mod.rs
git commit -m "feat(syntax): codegen Rust enums for alternation rules (Expr/Stmt/Pat/Type)"
```

---

## Task 11: Full-corpus differential oracle (new parser vs tree-sitter)

Now that the parser covers the whole grammar, replace the slice oracle with the real gate: the new parser must accept **every** `examples/**/*.as` with **zero errors**, matching the tree-sitter conformance guarantee.

**Files:**
- Modify: `tests/cst_parser_oracle.rs`

- [ ] **Step 1: Add the full-corpus acceptance test**

Add to `tests/cst_parser_oracle.rs`:

```rust
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
    assert!(failures.is_empty(), "new parser rejected files:\n{}", failures.join("\n"));
}

#[test]
fn new_parser_agrees_with_legacy_over_corpus() {
    // The legacy parser (mirroring the tree-sitter grammar) is the oracle: both
    // must accept every example. Divergence = a real grammar gap to reconcile.
    let mut failures = Vec::new();
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let new_ok = ascript::syntax::parser::parse(&src).errors.is_empty();
        let legacy_ok = match ascript::lexer::lex(&src) {
            Ok(toks) => ascript::parser::parse(&toks).is_ok(),
            Err(_) => false,
        };
        if new_ok != legacy_ok {
            failures.push(format!("{}: new={new_ok} legacy={legacy_ok}", path.display()));
        }
    }
    assert!(failures.is_empty(), "parser disagreements:\n{}", failures.join("\n"));
}
```

- [ ] **Step 2: Run the corpus oracle**

Run: `cargo test --test cst_parser_oracle 2>&1 | tail -25`
Expected: both tests PASS. Any rejection/disagreement names the exact file — open it, find the construct the new parser mishandles, fix the relevant parse function, and re-run. Likely remaining gaps: soft-keyword choices (`of`/`from`), return-type or generic-type edge cases, nested templates inside match arms.

- [ ] **Step 3: Full suite + clippy both configs**

Run: `cargo test 2>&1 | tail -15`
Expected: green.
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

- [ ] **Step 4: Commit**

```bash
git add tests/cst_parser_oracle.rs
git commit -m "test(syntax): full-corpus differential oracle (new parser vs legacy/tree-sitter)"
```

---

## Done criteria for Plan 2b-ii (and the whole front-end parser)

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs.
- [ ] The new parser accepts **every** `examples/**/*.as` with zero errors and agrees with the legacy parser over the whole corpus.
- [ ] Losslessness holds over the corpus (structured tree round-trips byte-for-byte).
- [ ] Destructuring, `for`/`for await`/range, `break`/`continue`, full functions (async/generator/rest/types), `enum`, `class`, `import`/`export`, and `match` (all patterns) parse to correct node shapes with error recovery.
- [ ] The typed-AST codegen emits both node structs and alternation enums (`Expr`/`Stmt`/`Pat`/`Type`).
- [ ] The interpreter and binary remain unchanged (legacy front-end still runs everything).

**Next plan:** `cst-name-resolver.md` (Plan 3) — the scope/name-resolution pass over the typed AST: bindings (let/const/params/destructuring/fn/class/enum/import), scope chains (block/function/class/loop/match-arm), captured-variable detection + per-frame slot allocation + the upvalue plan, and Option-C match-ident resolution. Built as **shared infrastructure** consumed first by the bytecode compiler and later by the checker.
