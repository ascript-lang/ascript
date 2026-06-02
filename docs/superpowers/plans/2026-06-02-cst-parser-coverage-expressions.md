# CST Parser Coverage — Expressions (Plan 2b-i)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the Plan 2 parser to cover the rest of AScript's **expression** grammar — array/object literals + spread, template strings, optional member `?.`, postfix `?`/`!` (the unwrap tier), the ternary operator (with its disambiguation), `await`, `yield`, compound assignment, and the full operator/binding-power set — all preserving losslessness and error recovery.

**Architecture:** Pure extension of `src/syntax/parser.rs` from Plan 2 — new node `SyntaxKind`s + new parse functions that reuse the existing `start/bump/complete/precede/at/error/at_end` helpers and the precedence-climbing `expr_bp`/`lhs`/`primary`/`postfix` core. No new modules. The tree builder, codegen pipeline, and oracle from Plan 2 are unchanged (the `ascript.ungram` grammar gains rules; the codegen already emits a struct per node kind).

**Tech Stack:** Rust, the Plan 1/Plan 2 `src/syntax/*` machinery.

**Scope note:** This is Plan 2b-i (spec: `docs/superpowers/specs/2026-06-02-cst-frontend-migration-design.md`). It completes **expressions** only. **Plan 2b-ii** covers declarations (destructuring, `for`, full functions + type annotations, `enum`, `class`, `import`/`export`), `match`/patterns, the typed-AST enum codegen, and swaps the oracle to full-corpus new-parser-vs-tree-sitter. Depends on Plan 2.

**Invariant (carried):** losslessness — every token still emitted; the corpus round-trip test stays green after every task.

---

## File Structure

- Modify `src/syntax/kind.rs` — add expression node kinds.
- Modify `src/syntax/parser.rs` — new parse functions + extended `infix_binding_power`, `primary`, `postfix`, and an `expr`-level ternary wrapper.
- Modify `src/syntax/ast/ascript.ungram` — add the new expression node rules.
- Modify `tests/cst_parser_oracle.rs` — extend the accept-list.

---

## Task 1: Add expression node kinds + grammar rules

**Files:**
- Modify: `src/syntax/kind.rs`
- Modify: `src/syntax/ast/ascript.ungram`

- [ ] **Step 1: Add a test for the new kinds**

Add to the `tests` mod in `src/syntax/kind.rs`:

```rust
#[test]
fn expression_node_kinds_exist() {
    for k in [
        SyntaxKind::ArrayExpr, SyntaxKind::ObjectExpr, SyntaxKind::ObjectField,
        SyntaxKind::SpreadElem, SyntaxKind::TemplateExpr, SyntaxKind::OptMemberExpr,
        SyntaxKind::TryExpr, SyntaxKind::UnwrapExpr, SyntaxKind::TernaryExpr,
        SyntaxKind::AwaitExpr, SyntaxKind::YieldExpr,
    ] {
        assert!(!k.is_trivia(), "{k:?}");
    }
}
```

- [ ] **Step 2: Run (expect compile failure)**

Run: `cargo test --lib syntax::kind::tests::expression_node_kinds_exist 2>&1 | tail -10`
Expected: FAIL to compile.

- [ ] **Step 3: Add the variants**

In `src/syntax/kind.rs`, in the `// --- nodes (Plan 2) ---` block, add:

```rust
    // --- expression nodes (Plan 2b-i) ---
    ArrayExpr, ObjectExpr, ObjectField, SpreadElem,
    TemplateExpr, OptMemberExpr, TryExpr, UnwrapExpr, TernaryExpr,
    AwaitExpr, YieldExpr,
```

- [ ] **Step 4: Add grammar rules**

In `src/syntax/ast/ascript.ungram`, extend the `Expr` alternation and add rules:

```
Expr =
    Literal | NameRef | UnaryExpr | BinaryExpr | ParenExpr | CallExpr
  | MemberExpr | IndexExpr | ArrowExpr | AssignExpr
  | ArrayExpr | ObjectExpr | TemplateExpr | OptMemberExpr
  | TryExpr | UnwrapExpr | TernaryExpr | AwaitExpr | YieldExpr

ArrayExpr = '[' (Expr | SpreadElem)* ']'
ObjectExpr = '{' (ObjectField | SpreadElem)* '}'
ObjectField = key:('ident' | 'string') ':' value:Expr
SpreadElem = '...' Expr
TemplateExpr = 'template'
OptMemberExpr = Expr '?.' 'ident'
TryExpr = Expr '?'
UnwrapExpr = Expr '!'
TernaryExpr = cond:Expr '?' then:Expr ':' els:Expr
AwaitExpr = 'await' Expr
YieldExpr = 'yield' Expr?
```

- [ ] **Step 5: Run the kind test + build (codegen picks up new rules)**

Run: `cargo test --lib syntax::kind 2>&1 | tail -10`
Expected: PASS.
Run: `cargo build 2>&1 | tail -5`
Expected: builds (codegen emits structs for the new node kinds).

- [ ] **Step 6: Commit**

```bash
git add src/syntax/kind.rs src/syntax/ast/ascript.ungram
git commit -m "feat(syntax): expression node kinds + grammar rules (Plan 2b-i)"
```

---

## Task 2: Array & object literals with spread

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write shape tests**

Add to the `tests` mod in `src/syntax/parser.rs`:

```rust
    #[test]
    fn array_literal_with_spread() {
        let shape = tree_shape("[1, ...xs, 2]");
        assert!(shape.contains(&SyntaxKind::ArrayExpr));
        assert!(shape.contains(&SyntaxKind::SpreadElem));
        assert!(parse("[1, ...xs, 2]").errors.is_empty());
    }

    #[test]
    fn object_literal_with_spread() {
        let shape = tree_shape(r#"{ a: 1, "k": 2, ...rest }"#);
        assert!(shape.contains(&SyntaxKind::ObjectExpr));
        assert!(shape.contains(&SyntaxKind::ObjectField));
        assert!(shape.contains(&SyntaxKind::SpreadElem));
        assert!(parse(r#"{ a: 1, "k": 2, ...rest }"#).errors.is_empty());
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::array_literal_with_spread 2>&1 | tail -15`
Expected: FAIL — `[` / `{` not handled in `primary`.

- [ ] **Step 3: Add array/object parsing to `primary`**

In `src/syntax/parser.rs`, add two arms to the `match p.current()` in `primary` (before the `_ =>` fallback), and add the helper functions. New arms:

```rust
        LBracket => array_expr(p),
        LBrace => object_expr(p),
```

New functions (add near `arg_list`):

```rust
/// `...expr` spread element, used in arrays, objects, and call args.
fn spread_elem(p: &mut Parser) {
    let m = p.start();
    p.bump(); // ...
    expr(p);
    p.complete(m, SyntaxKind::SpreadElem);
}

fn array_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // [
    while !p.at(RBracket) && !p.at_end() {
        if p.at(DotDotDot) {
            spread_elem(p);
        } else {
            expr(p);
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
        p.error("expected ']' to close array");
    }
    p.complete(m, ArrayExpr)
}

fn object_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // {
    while !p.at(RBrace) && !p.at_end() {
        if p.at(DotDotDot) {
            spread_elem(p);
        } else {
            let fm = p.start();
            // key: ident or string literal
            if p.at(Ident) || p.at(Str) {
                p.bump();
            } else {
                p.error("expected object key");
            }
            if p.at(Colon) {
                p.bump();
                expr(p);
            } else {
                p.error("expected ':' after object key");
            }
            p.complete(fm, ObjectField);
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
        p.error("expected '}' to close object");
    }
    p.complete(m, ObjectExpr)
}
```

Also extend `arg_list` to allow spread args — replace its `expr(p);` line with:

```rust
        if p.at(SyntaxKind::DotDotDot) {
            spread_elem(p);
        } else {
            expr(p);
        }
```

- [ ] **Step 4: Run tests + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: array/object tests + all prior PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): array/object literals + spread (incl. call-arg spread)"
```

---

## Task 3: Template strings

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write template tests**

Add to the `tests` mod:

```rust
    #[test]
    fn plain_template() {
        let shape = tree_shape("`hello`");
        assert!(shape.contains(&SyntaxKind::TemplateExpr));
        assert!(parse("`hello`").errors.is_empty());
    }

    #[test]
    fn interpolated_template() {
        // `a${x}b${y}c` => TemplateStart, expr, TemplateMiddle, expr, TemplateEnd
        let p = parse("`a${x}b${y}c`");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert!(tree_shape("`a${x}b${y}c`").contains(&SyntaxKind::TemplateExpr));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::plain_template 2>&1 | tail -15`
Expected: FAIL — template tokens not handled.

- [ ] **Step 3: Add template parsing to `primary`**

Add arms to `primary`'s match (before `_ =>`):

```rust
        TemplateStr => {
            let m = p.start();
            p.bump();
            p.complete(m, TemplateExpr)
        }
        TemplateStart => template_expr(p),
```

Add the function:

```rust
/// Parse an interpolated template: TemplateStart (expr TemplateMiddle)* expr
/// TemplateEnd. Each `${...}` slot holds a full expression.
fn template_expr(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // TemplateStart  (`...${)
    loop {
        expr(p); // interpolated expression
        if p.at(TemplateMiddle) {
            p.bump(); // }...${  → another interpolation follows
            continue;
        }
        if p.at(TemplateEnd) {
            p.bump(); // }...`
            break;
        }
        p.error("unterminated template interpolation");
        break;
    }
    p.complete(m, TemplateExpr)
}
```

- [ ] **Step 4: Run tests + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): template strings (plain + interpolated)"
```

---

## Task 4: Optional member `?.` and the unwrap tier (`?` / `!`)

This is the precedence-sensitive part. Per CLAUDE.md: postfix `?` (`TryExpr`) and `!` (`UnwrapExpr`) live in a tier **between exponent and unary**, looser than `await`/prefix-unary, so `await x!` is `(await x)!`. They are parsed as postfix in the `postfix` chain. The ternary `?` (Task 5) is disambiguated from the propagate `?` by whether a `:` follows at bracket-depth 0.

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write tests**

Add to the `tests` mod:

```rust
    #[test]
    fn optional_member() {
        let shape = tree_shape("a?.b");
        assert!(shape.contains(&SyntaxKind::OptMemberExpr));
        assert!(parse("a?.b").errors.is_empty());
    }

    #[test]
    fn try_and_unwrap_postfix() {
        assert!(tree_shape("f()?").contains(&SyntaxKind::TryExpr));
        assert!(tree_shape("g()!").contains(&SyntaxKind::UnwrapExpr));
        assert!(parse("f()?").errors.is_empty());
        assert!(parse("g()!").errors.is_empty());
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::optional_member 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Add `?.` to `postfix`, and introduce a proper unwrap tier for `?`/`!`**

`?.` is a *tight* primary-postfix (same level as `.`/call/index), so it goes in `postfix`. But `?` (propagate) and `!` (unwrap) are a tier **looser than unary** (CLAUDE.md: between exponent and unary; `await x!` = `(await x)!`), so they must wrap the *whole* unary layer, **not** sit in the primary postfix chain. This requires splitting Plan 2's `lhs` into `unary` (prefix `await`/`-`/`!x` + primary) and `lhs` (= `unwrap_tier` over `unary`).

First, add only `?.` to the `postfix` loop (before `_ => break`):

```rust
            QuestionDot => {
                let m = p.precede(&cm);
                p.bump(); // ?.
                if p.at(Ident) {
                    p.bump();
                } else {
                    p.error("expected property name after '?.'");
                }
                cm = p.complete(m, OptMemberExpr);
            }
```

Then **rename** Plan 2's `fn lhs` to `fn unary`, change its internal recursion from `lhs(p)` to `unary(p)` (so prefix operators nest within the unary layer, *below* the unwrap tier), and add a new `lhs` plus `unwrap_tier`:

```rust
/// Unary/primary layer: prefix `-`/`!x` (and, after Task 6, `await`/`yield`),
/// then a primary with its tight postfix chain (call/member/index/?.).
fn unary(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    match p.current() {
        Minus | Bang => {
            let m = p.start();
            p.bump();
            let _operand = unary(p);
            p.complete(m, UnaryExpr)
        }
        _ => primary(p),
    }
}

/// The unwrap tier — looser than unary, tighter than binary `**`. Applies the
/// postfix propagate `?` (when not a ternary) and force-unwrap `!` over the whole
/// unary expression, so `await x!` parses as `(await x)!`.
fn unwrap_tier(p: &mut Parser, mut cm: CompletedMarker) -> CompletedMarker {
    use SyntaxKind::*;
    loop {
        match p.current() {
            Question if !ternary_ahead(p) => {
                let m = p.precede(&cm);
                p.bump(); // ?
                cm = p.complete(m, TryExpr);
            }
            Bang => {
                let m = p.precede(&cm);
                p.bump(); // !
                cm = p.complete(m, UnwrapExpr);
            }
            _ => break,
        }
    }
    cm
}

/// Operand of the binary precedence-climb: unary, then the unwrap tier.
fn lhs(p: &mut Parser) -> CompletedMarker {
    let u = unary(p);
    unwrap_tier(p, u)
}
```

> Plan 2's `expr_bp`/`expr_returning` call `lhs(p)`; that still holds — `lhs` now means "unary + unwrap tier", which is the correct binary operand. The `Minus | Bang` arm formerly in `lhs` now lives in `unary`.

Add the disambiguation helper (used here and in Task 5). It scans forward from the current `?` and returns true if a `:` appears at bracket-depth 0 before the statement ends (a `;`, a closing `}` of the enclosing block, or EOF):

```rust
/// True if the `?` at the cursor is a ternary `?` (a `:` follows at bracket-depth
/// 0 before the statement ends), false if it is a postfix propagate `?`.
/// Mirrors the legacy `is_ternary_question`.
fn ternary_ahead(p: &Parser) -> bool {
    use SyntaxKind::*;
    let mut depth = 0i32;
    let mut i = p.pos; // currently AT the `?`
    // start scanning AFTER the `?`
    i += 1;
    while i < p.nontrivia.len() {
        match p.tokens[p.nontrivia[i]].kind {
            LParen | LBracket | LBrace => depth += 1,
            RParen | RBracket => depth -= 1,
            RBrace => {
                if depth == 0 {
                    return false; // closes the enclosing block before any `:`
                }
                depth -= 1;
            }
            Semicolon if depth == 0 => return false,
            Colon if depth == 0 => return true,
            Question if depth == 0 => {
                // a nested ternary `?` — its matching `:` is consumed first;
                // a propagate `?` here doesn't change our search. Keep scanning.
            }
            _ => {}
        }
        i += 1;
    }
    false
}
```

> Postfix `Question` is gated by `!ternary_ahead(p)` so a ternary `?` is left for the expression-level handler in Task 5. `Bang` is always postfix unwrap (no ambiguity).

- [ ] **Step 4: Run tests + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): optional member ?. and unwrap tier (? / !)"
```

---

## Task 5: Ternary operator

The ternary binds just above assignment and is right-associative. It's parsed at the expression-statement / expression top level, after the precedence-climbing expression yields a condition.

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write ternary tests**

Add to the `tests` mod:

```rust
    #[test]
    fn ternary_basic() {
        let shape = tree_shape("a ? b : c");
        assert!(shape.contains(&SyntaxKind::TernaryExpr));
        assert!(parse("a ? b : c").errors.is_empty());
    }

    #[test]
    fn ternary_vs_propagate_disambiguation() {
        // `f()? - 1` is propagate-then-subtract (NOT a ternary): no `:` follows.
        let p = parse("f()? - 1");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        assert!(tree_shape("f()? - 1").contains(&SyntaxKind::TryExpr));
        assert!(!tree_shape("f()? - 1").contains(&SyntaxKind::TernaryExpr));
        // `a ? -b : c` IS a ternary.
        assert!(tree_shape("a ? -b : c").contains(&SyntaxKind::TernaryExpr));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::ternary_basic 2>&1 | tail -15`
Expected: FAIL — ternary not produced (the `?` with a `:` ahead is currently left unconsumed by `postfix`, causing an error or stray token).

- [ ] **Step 3: Add a ternary wrapper at the expression top level**

The expression entry points (`expr` and `expr_returning`) must, after parsing the condition, check for a ternary `?`. Replace `fn expr` and add a shared ternary tail. Replace:

```rust
fn expr(p: &mut Parser) {
    expr_bp(p, 0);
}
```

with:

```rust
fn expr(p: &mut Parser) {
    let cm = expr_returning(p);
    let _ = cm;
}
```

and extend `expr_returning` so that after the precedence climb it folds a ternary if a ternary `?` is present:

```rust
fn expr_returning(p: &mut Parser) -> CompletedMarker {
    let cm = lhs(p);
    let mut lhs_cm = cm;
    loop {
        let op = p.current();
        let Some((_l_bp, r_bp)) = infix_binding_power(op) else { break };
        let m = p.precede(&lhs_cm);
        p.bump();
        expr_bp(p, r_bp);
        lhs_cm = p.complete(m, SyntaxKind::BinaryExpr);
    }
    // Ternary tail: cond ? then : els  (right-assoc; `then`/`els` are full exprs).
    if p.at(SyntaxKind::Question) && ternary_ahead(p) {
        let m = p.precede(&lhs_cm);
        p.bump(); // ?
        expr(p); // then
        if p.at(SyntaxKind::Colon) {
            p.bump();
            expr(p); // els
        } else {
            p.error("expected ':' in ternary");
        }
        lhs_cm = p.complete(m, SyntaxKind::TernaryExpr);
    }
    lhs_cm
}
```

> Because `postfix` only treats `?` as a `TryExpr` when `!ternary_ahead(p)`, a ternary `?` reaches here unconsumed and is folded into a `TernaryExpr`. `expr` now delegates to `expr_returning`, so every expression position gets ternary support.

- [ ] **Step 4: Run tests + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -20`
Expected: ternary tests + all prior PASS.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): ternary operator with propagate-? disambiguation"
```

---

## Task 6: `await` and `yield`

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write tests**

Add to the `tests` mod:

```rust
    #[test]
    fn await_expression() {
        assert!(tree_shape("await f()").contains(&SyntaxKind::AwaitExpr));
        assert!(parse("await f()").errors.is_empty());
        // The unwrap tier is looser than unary, so `await x?` = `(await x)?`:
        // in pre-order the TryExpr must appear BEFORE (wrap) the AwaitExpr.
        let shape = tree_shape("await x?");
        let try_idx = shape.iter().position(|k| *k == SyntaxKind::TryExpr)
            .expect("TryExpr present");
        let await_idx = shape.iter().position(|k| *k == SyntaxKind::AwaitExpr)
            .expect("AwaitExpr present");
        assert!(try_idx < await_idx, "expected (await x)? — TryExpr should wrap AwaitExpr");
    }

    #[test]
    fn yield_expression() {
        assert!(tree_shape("yield x").contains(&SyntaxKind::YieldExpr));
        assert!(tree_shape("yield").contains(&SyntaxKind::YieldExpr));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::await_expression 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Add `await`/`yield` as prefix forms in `unary`**

`await`/`yield` are prefix operators in the **unary** layer (Task 4 split `lhs` into `unary` + `unwrap_tier`). Because the unwrap tier wraps the whole unary, `await x?` correctly parses as `(await x)?` with no extra work. Add arms to the `match p.current()` in `unary` (before the `_ => primary(p)`):

```rust
        AwaitKw => {
            let m = p.start();
            p.bump(); // await
            let _operand = unary(p); // binds to the unary/primary that follows
            p.complete(m, AwaitExpr)
        }
        YieldKw => {
            let m = p.start();
            p.bump(); // yield
            // Optional operand: parse one if an expression can start here.
            if can_start_expr(p) {
                let _ = unary(p);
            }
            p.complete(m, YieldExpr)
        }
```

Add the `can_start_expr` helper (used to decide whether `yield` has an operand):

```rust
/// True if the current token can begin an expression (used for optional operands
/// like `yield` / `return`).
fn can_start_expr(p: &Parser) -> bool {
    use SyntaxKind::*;
    matches!(
        p.current(),
        Number | Str | TrueKw | FalseKw | NilKw | Ident | LParen | LBracket
            | LBrace | Minus | Bang | TemplateStr | TemplateStart | AwaitKw | YieldKw
    )
}
```

> Postfix `?`/`!` already live in `postfix`, which runs on whatever `primary` returns. Since `await x` is built in `lhs` and `postfix` is applied within `primary` to the *operand*, ensure the await wraps before postfix by having `postfix` apply at the `lhs` level. If `await x?` does not yield `(await x)?` in the test, move the `postfix(p, cm)` call so it wraps the completed `AwaitExpr` (apply `postfix` to the await's CompletedMarker). The test `await x?` pins this behavior.

- [ ] **Step 4: Run tests + losslessness**

Run: `cargo test --lib syntax::parser 2>&1 | tail -20`
Expected: PASS — including the `await x?` → `(await x)?` nesting assertion.
Run: `cargo test --test cst_lossless 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): await and yield expressions"
```

---

## Task 7: Compound assignment + remaining operators (`??`)

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write tests**

Add to the `tests` mod:

```rust
    #[test]
    fn nullish_coalescing() {
        assert!(tree_shape("a ?? b").contains(&SyntaxKind::BinaryExpr));
        assert!(parse("a ?? b").errors.is_empty());
    }

    #[test]
    fn compound_assignment() {
        for src in ["x += 1", "x -= 1", "x *= 2", "x /= 2"] {
            let p = parse(src);
            assert!(p.errors.is_empty(), "errors for {src}: {:?}", p.errors);
            assert!(tree_shape(src).contains(&SyntaxKind::AssignExpr), "no assign for {src}");
        }
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::compound_assignment 2>&1 | tail -15`
Expected: FAIL — compound ops not handled; `??` missing from binding powers.

- [ ] **Step 3: Add `??` to binding powers; handle compound-assign in `expr_stmt`**

In `infix_binding_power`, add `QuestionQuestion` at a low precedence (just above `||`):

```rust
        QuestionQuestion => (1, 2),
        PipePipe => (1, 2),
```

> Note: give `??` and `||` distinct levels if the legacy parser distinguishes them; the differential oracle (Plan 2b-ii) will catch a mismatch. A safe choice matching common precedence is `QuestionQuestion => (1, 2)` and bumping `||` to `(2, 3)` etc. Keep it consistent and let the oracle verify.

In `expr_stmt`, replace the `if p.at(SyntaxKind::Eq)` assignment check with one that also accepts compound-assign operators:

```rust
fn expr_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    let lhs = expr_returning(p);
    if matches!(p.current(), Eq | PlusEq | MinusEq | StarEq | SlashEq) {
        let am = p.precede(&lhs);
        p.bump(); // = or += etc.
        expr(p);
        p.complete(am, AssignExpr);
    }
    p.complete(m, ExprStmt);
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
git commit -m "feat(syntax): nullish coalescing + compound assignment"
```

---

## Task 8: Extend the differential oracle's accept-list

**Files:**
- Modify: `tests/cst_parser_oracle.rs`

- [ ] **Step 1: Add the new expression forms to `ACCEPT`**

In `tests/cst_parser_oracle.rs`, extend the `ACCEPT` array with:

```rust
    "[1, ...xs, 2]",
    r#"{ a: 1, "k": 2, ...rest }"#,
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
```

- [ ] **Step 2: Run the oracle + full suite + clippy both configs**

Run: `cargo test --test cst_parser_oracle 2>&1 | tail -15`
Expected: both tests PASS (new parser and legacy parser agree on all).
Run: `cargo test 2>&1 | tail -15`
Expected: full suite green.
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

> If the legacy parser rejects a snippet the new parser accepts (or vice-versa), that is a real divergence — reconcile the new parser to the legacy behavior (the legacy parser mirrors the tree-sitter grammar). Most likely culprits: `??` vs `||` precedence, or `await`/unwrap-tier nesting.

- [ ] **Step 3: Commit**

```bash
git add tests/cst_parser_oracle.rs
git commit -m "test(syntax): oracle accepts the full expression slice"
```

---

## Done criteria for Plan 2b-i

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs.
- [ ] Array/object literals + spread (incl. call-arg spread), templates (plain + interpolated), `?.`, postfix `?`/`!`, ternary (with propagate-`?` disambiguation), `await`, `yield`, `??`, and compound assignment all parse to correct node shapes with error recovery.
- [ ] Losslessness holds over the corpus.
- [ ] The differential oracle agrees with the legacy parser on the full expression slice.
- [ ] The interpreter and binary remain unchanged.

**Next plan:** `cst-parser-coverage-declarations.md` (Plan 2b-ii) — destructuring (`let [a, ...rest]`, `let {a, b as c, ...rest}`), `for`/`for await`/`for..in` range + `of`, `break`/`continue`, full functions (`async`/`fn*`/`async fn*`, rest params, param/return type annotations) + the recursive **type-annotation** parser (`T?`, `array<T>`, `map<K,V>`, `future<T>`, unions, tuples, `Result<T>`), `enum`, `class` (superclass, typed/optional/defaulted fields, methods, `init`), `import`/`export`, and `match` with all pattern forms (wildcard/ident/value/range/array/object/or-patterns/guards). Then: extend the codegen to emit Rust **enums** for the `Stmt`/`Expr`/`Pattern`/`Type` alternation rules, and swap the oracle to full-corpus **new-parser-vs-tree-sitter** over every `examples/**/*.as`.
