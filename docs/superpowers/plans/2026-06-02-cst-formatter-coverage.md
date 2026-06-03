# CST Formatter — Full Coverage, Gates & CLI Cutover (Plan 4b)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the CST formatter — canonical rules for every expression, statement, declaration, and type; the `name?: T` → `name: T?` and quote/escape normalizations — then enforce the acceptance gates (**comment-preservation + idempotence over the whole corpus**) and **wire `ascript fmt` to the new formatter**, retiring the comment-dropping legacy path.

**Architecture:** Extends `src/syntax/format/mod.rs` from Plan 4a — fills out the `expr`/`stmt`/`member` match arms for the full grammar, adds a `type_ann` pretty-printer and string/key normalization helpers (ported from the legacy `fmt.rs` rules, incl. the shared `is_ident_like` from `token.rs`). Then a corpus test asserts every source comment survives and `fmt(fmt(x)) == fmt(x)`, and `src/main.rs`'s `Fmt` command switches to `ascript::syntax::format_tree`.

**Tech Stack:** Rust, the Plan 4a formatter, `cstree`, the existing `is_ident_like` helper.

**Scope note:** Final front-end plan (spec: `docs/superpowers/specs/2026-06-02-cst-frontend-migration-design.md`). After this, the comment-preserving formatter is the shipped `ascript fmt`. The legacy `src/fmt.rs` is removed only in the migration's final merge (per the front-end spec's OPEN DECISION); this plan **redirects the CLI** to the new formatter and may leave `src/fmt.rs` in place (dead) until that merge. Depends on Plan 4a.

**Layout note:** AScript's canonical style is compact (expressions inline; blocks/class/match multi-line) — matching the legacy `fmt.rs` and the existing `examples/`. Where a specific layout decision (multi-line thresholds, match-arm style) is ambiguous, **match the legacy formatter**; the idempotence gate + the legacy-anchor test (Task 6) verify it.

---

## File Structure

- Modify `src/syntax/format/mod.rs` — complete `expr`/`stmt`/`member` arms; add `type_ann`, string/key normalization.
- Modify `src/main.rs` — `Command::Fmt` → new formatter.
- Create `tests/cst_format.rs` — corpus comment-preservation + idempotence gates + legacy-anchor.

---

## Task 1: Complete expression formatting

**Files:**
- Modify: `src/syntax/format/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod in `src/syntax/format/mod.rs`:

```rust
    #[test]
    fn formats_expressions() {
        assert_eq!(fmt("f( 1,2 )\n"), "f(1, 2)\n");
        assert_eq!(fmt("a . b [ c ]\n"), "a.b[c]\n");
        assert_eq!(fmt("a?.b\n"), "a?.b\n");
        assert_eq!(fmt("[ 1 ,2, 3 ]\n"), "[1, 2, 3]\n");
        assert_eq!(fmt("{ a:1 , b: 2 }\n"), "{a: 1, b: 2}\n");
        assert_eq!(fmt("- x\n"), "-x\n");
        assert_eq!(fmt("a ?b: c\n"), "a ? b : c\n");
        assert_eq!(fmt("f()?\n"), "f()?\n");
        assert_eq!(fmt("g()!\n"), "g()!\n");
        assert_eq!(fmt("await  f()\n"), "await f()\n");
        assert_eq!(fmt("...xs\n"), "...xs\n"); // spread element (inside a call/array)
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::format::tests::formats_expressions 2>&1 | tail -15`
Expected: FAIL — most hit the verbatim fallback.

- [ ] **Step 3: Fill out `expr`**

Replace `Printer::expr` with full coverage:

```rust
    fn expr(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        match node.kind() {
            Literal => self.out.text(&self.literal_text(node)),
            NameRef => self.out.text(&node.text().to_string()),
            UnaryExpr => {
                let op = leading_op(node);
                self.out.text(&op);
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(&e);
                }
            }
            BinaryExpr => {
                let kids: Vec<_> = node.children().filter(|c| is_expr_kind(c.kind())).collect();
                let op = binary_op(node);
                if let Some(l) = kids.first() { self.expr(l); }
                self.out.text(&format!(" {op} "));
                if let Some(r) = kids.get(1) { self.expr(r); }
            }
            ParenExpr => {
                self.out.text("(");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.expr(&e);
                }
                self.out.text(")");
            }
            CallExpr => {
                let kids: Vec<_> = node.children().collect();
                if let Some(callee) = kids.iter().find(|c| is_expr_kind(c.kind())) {
                    self.expr(callee);
                }
                if let Some(args) = kids.iter().find(|c| c.kind() == ArgList) {
                    self.arg_list(args);
                }
            }
            MemberExpr => {
                let obj = node.children().find(|c| is_expr_kind(c.kind()));
                if let Some(o) = obj { self.expr(&o); }
                self.out.text(".");
                self.out.text(&member_name(node));
            }
            OptMemberExpr => {
                if let Some(o) = node.children().find(|c| is_expr_kind(c.kind())) { self.expr(&o); }
                self.out.text("?.");
                self.out.text(&member_name(node));
            }
            IndexExpr => {
                let kids: Vec<_> = node.children().filter(|c| is_expr_kind(c.kind())).collect();
                if let Some(o) = kids.first() { self.expr(o); }
                self.out.text("[");
                if let Some(i) = kids.get(1) { self.expr(i); }
                self.out.text("]");
            }
            ArrayExpr => self.comma_seq("[", "]", node),
            ObjectExpr => self.object_expr(node),
            SpreadElem => {
                self.out.text("...");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) { self.expr(&e); }
            }
            TemplateExpr => self.out.text(&node.text().to_string()), // templates re-emit verbatim (interpolation kept as-is)
            TryExpr => { self.unary_postfix(node, "?"); }
            UnwrapExpr => { self.unary_postfix(node, "!"); }
            TernaryExpr => {
                let kids: Vec<_> = node.children().filter(|c| is_expr_kind(c.kind())).collect();
                if let Some(c) = kids.first() { self.expr(c); }
                self.out.text(" ? ");
                if let Some(t) = kids.get(1) { self.expr(t); }
                self.out.text(" : ");
                if let Some(e) = kids.get(2) { self.expr(e); }
            }
            AwaitExpr => {
                self.out.text("await ");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) { self.expr(&e); }
            }
            YieldExpr => {
                self.out.text("yield");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.out.text(" ");
                    self.expr(&e);
                }
            }
            AssignExpr => {
                let kids: Vec<_> = node.children().filter(|c| is_expr_kind(c.kind())).collect();
                if let Some(t) = kids.first() { self.expr(t); }
                self.out.text(&format!(" {} ", assign_op(node)));
                if let Some(v) = kids.get(1) { self.expr(v); }
            }
            ArrowExpr => self.arrow_expr(node),
            MatchExpr => self.match_expr(node),
            RangeExpr => {
                let kids: Vec<_> = node.children().filter(|c| is_expr_kind(c.kind())).collect();
                if let Some(s) = kids.first() { self.expr(s); }
                self.out.text(range_op(node));
                if let Some(e) = kids.get(1) { self.expr(e); }
            }
            _ => self.out.text(&node.text().to_string()),
        }
    }

    fn unary_postfix(&mut self, node: &SyntaxNode, op: &str) {
        if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
            self.expr(&e);
        }
        self.out.text(op);
    }

    fn arg_list(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("(");
        let items: Vec<_> = node.children().filter(|c| is_expr_kind(c.kind()) || c.kind() == SpreadElem).collect();
        for (i, it) in items.iter().enumerate() {
            if i > 0 { self.out.text(", "); }
            self.expr(it);
        }
        self.out.text(")");
    }

    fn comma_seq(&mut self, open: &str, close: &str, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text(open);
        let items: Vec<_> = node.children().filter(|c| is_expr_kind(c.kind()) || c.kind() == SpreadElem).collect();
        for (i, it) in items.iter().enumerate() {
            if i > 0 { self.out.text(", "); }
            self.expr(it);
        }
        self.out.text(close);
    }

    fn object_expr(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("{");
        let items: Vec<_> = node.children().filter(|c| matches!(c.kind(), ObjectField | SpreadElem)).collect();
        for (i, it) in items.iter().enumerate() {
            if i > 0 { self.out.text(", "); }
            match it.kind() {
                ObjectField => {
                    self.out.text(&self.object_key(it));
                    self.out.text(": ");
                    if let Some(v) = it.children().find(|c| is_expr_kind(c.kind())) { self.expr(&v); }
                }
                SpreadElem => self.expr(it),
                _ => {}
            }
        }
        self.out.text("}");
    }
```

Add the small token helpers at file scope:

```rust
fn leading_op(node: &SyntaxNode) -> String {
    node.children_with_tokens().filter_map(|el| el.into_token())
        .find(|t| !t.kind().is_trivia())
        .map(|t| t.text().to_string()).unwrap_or_default()
}
fn binary_op(node: &SyntaxNode) -> String {
    use SyntaxKind::*;
    node.children_with_tokens().filter_map(|el| el.into_token())
        .find(|t| matches!(t.kind(),
            Plus|Minus|Star|Slash|Percent|StarStar|EqEq|BangEq|Lt|Le|Gt|Ge|AmpAmp|PipePipe|QuestionQuestion))
        .map(|t| t.text().to_string()).unwrap_or_default()
}
fn assign_op(node: &SyntaxNode) -> String {
    use SyntaxKind::*;
    node.children_with_tokens().filter_map(|el| el.into_token())
        .find(|t| matches!(t.kind(), Eq|PlusEq|MinusEq|StarEq|SlashEq))
        .map(|t| t.text().to_string()).unwrap_or_else(|| "=".into())
}
fn range_op(node: &SyntaxNode) -> &'static str {
    if node.children_with_tokens().filter_map(|el| el.into_token()).any(|t| t.kind() == SyntaxKind::DotDotEq) {
        "..="
    } else {
        ".."
    }
}
fn member_name(node: &SyntaxNode) -> String {
    // last IDENT token is the property name
    node.children_with_tokens().filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::Ident).last()
        .map(|t| t.text().to_string()).unwrap_or_default()
}
```

> `literal_text`, `object_key`, `arrow_expr`, `match_expr`, `type_ann` are defined in later tasks; for Task 1 add minimal versions: `fn literal_text(&self, n) -> String { n.text().to_string() }`, `fn object_key(&self, n) -> String { /* first ident/str token */ }` (proper escaping in Task 5), and stub `arrow_expr`/`match_expr` to verbatim until Tasks 2/3. To keep Task 1 compiling, add those stubs now and refine later.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format 2>&1 | tail -20`
Expected: `formats_expressions` + prior PASS.

```bash
git add src/syntax/format/mod.rs
git commit -m "feat(format): complete expression formatting"
```

---

## Task 2: Complete statement formatting (if/else, while, for, assignment, break/continue, import/export, enum)

**Files:**
- Modify: `src/syntax/format/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn formats_statements() {
        assert_eq!(fmt("if x{return 1}else{return 2}\n"),
            "if x {\n  return 1\n} else {\n  return 2\n}\n");
        assert_eq!(fmt("while x{ x=0 }\n"), "while x {\n  x = 0\n}\n");
        assert_eq!(fmt("for(i in 1..6){print(i)}\n"),
            "for (i in 1..6) {\n  print(i)\n}\n");
        assert_eq!(fmt("x=5\n"), "x = 5\n");
        assert_eq!(fmt("break\n"), "break\n");
        assert_eq!(fmt(r#"import * as t from "std/task""#), "import * as t from \"std/task\"\n");
        assert_eq!(fmt("enum E{A,B=2}\n"), "enum E {\n  A,\n  B = 2,\n}\n");
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::format::tests::formats_statements 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Fill out `stmt`**

Add arms to `Printer::stmt` (keep `ExprStmt`/`LetStmt`/`ReturnStmt`/`Block`/`FnDecl`/`ClassDecl` from 4a). The assignment case: `ExprStmt` whose child is an `AssignExpr` is already handled by `expr`. Add:

```rust
            IfStmt => {
                self.out.text("if ");
                let parts: Vec<_> = node.children().collect();
                if let Some(cond) = parts.iter().find(|c| is_expr_kind(c.kind())) {
                    self.expr(cond);
                }
                self.out.text(" ");
                let blocks: Vec<_> = parts.iter().filter(|c| c.kind() == Block).collect();
                if let Some(then) = blocks.first() { self.block(then); }
                // else (Block) or else-if (IfStmt)
                if let Some(elif) = parts.iter().find(|c| c.kind() == IfStmt) {
                    // back up the trailing newline from the then-block, emit ` else `
                    self.out.append_to_prev_line(" else");
                    self.out.text(" ");
                    self.stmt(elif);
                } else if let Some(els) = blocks.get(1) {
                    self.out.append_to_prev_line(" else {");
                    // re-emit the else block body without its own opening brace:
                    self.block_body_only(els);
                }
            }
            WhileStmt => {
                self.out.text("while ");
                if let Some(cond) = node.children().find(|c| is_expr_kind(c.kind())) { self.expr(&cond); }
                self.out.text(" ");
                if let Some(b) = node.children().find(|c| c.kind() == Block) { self.block(&b); }
            }
            ForStmt => self.for_stmt(node),
            BreakStmt => { self.out.text("break"); self.out.newline(); }
            ContinueStmt => { self.out.text("continue"); self.out.newline(); }
            EnumDecl => self.enum_decl(node),
            ImportStmt => { self.out.text(&normalize_import(node)); self.out.newline(); }
            ExportStmt => {
                self.out.text("export ");
                if let Some(inner) = node.children().next() { self.stmt(&inner); }
            }
```

> The `else { ... }` handling is fiddly with the line-oriented builder. Simpler, robust alternative used here: format the then-block, then for an `else` block emit ` else ` + a fresh `block`. Replace the `IfStmt` else handling above with the simpler form below if `append_to_prev_line` gymnastics prove brittle:
>
> ```rust
>             IfStmt => {
>                 self.out.text("if ");
>                 let parts: Vec<_> = node.children().collect();
>                 if let Some(cond) = parts.iter().find(|c| is_expr_kind(c.kind())) { self.expr(cond); }
>                 self.out.text(" ");
>                 let blocks: Vec<_> = parts.iter().filter(|c| c.kind() == Block).collect();
>                 if let Some(then) = blocks.first() { self.block_inline(then); } // no trailing newline
>                 if let Some(elif) = parts.iter().find(|c| c.kind() == IfStmt) {
>                     self.out.text(" else ");
>                     self.stmt(elif);
>                 } else if let Some(els) = blocks.get(1) {
>                     self.out.text(" else ");
>                     self.block(els);
>                 } else {
>                     self.out.newline();
>                 }
>             }
> ```
>
> where `block_inline` is `block` without the final `newline()`. Prefer this form; add `block_inline` (a copy of `block` whose last `self.out.text("}")` is **not** followed by `newline()`), and have `block` call `block_inline` then `newline()`.

Add the statement helpers:

```rust
    fn for_stmt(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("for ");
        // optional `await`
        if node.children_with_tokens().filter_map(|el| el.into_token()).any(|t| t.kind() == AwaitKw) {
            self.out.text("await ");
        }
        self.out.text("(");
        if let Some(var) = first_ident_text(node) { self.out.text(&var); }
        // `in` or `of`
        let kw = node.children_with_tokens().filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), InKw | OfKw)).map(|t| t.text().to_string()).unwrap_or_else(|| "of".into());
        self.out.text(&format!(" {kw} "));
        // iterable / range expression
        if let Some(it) = node.children().find(|c| is_expr_kind(c.kind())) { self.expr(&it); }
        self.out.text(") ");
        if let Some(b) = node.children().find(|c| c.kind() == Block) { self.block(&b); }
    }

    fn enum_decl(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("enum ");
        if let Some(name) = first_ident_text(node) { self.out.text(&name); }
        self.out.text(" {");
        self.out.newline();
        self.out.indent();
        for v in node.children().filter(|c| c.kind() == EnumVariant) {
            self.emit_leading(&v);
            if let Some(name) = first_ident_text(&v) { self.out.text(&name); }
            if let Some(val) = v.children().find(|c| is_expr_kind(c.kind())) {
                self.out.text(" = ");
                self.expr(&val);
            }
            self.out.text(",");
            self.out.newline();
            self.emit_trailing(&v);
        }
        self.out.dedent();
        self.out.text("}");
        self.out.newline();
    }
```

```rust
/// Canonical `import` line: `import { a, b } from "src"` or `import * as n from "src"`.
fn normalize_import(node: &SyntaxNode) -> String {
    use SyntaxKind::*;
    let src = node.children_with_tokens().filter_map(|el| el.into_token())
        .find(|t| t.kind() == Str).map(|t| t.text().to_string()).unwrap_or_default();
    if let Some(list) = node.children().find(|c| c.kind() == ImportList) {
        let names: Vec<String> = list.children_with_tokens().filter_map(|el| el.into_token())
            .filter(|t| t.kind() == Ident).map(|t| t.text().to_string()).collect();
        format!("import {{ {} }} from {}", names.join(", "), src)
    } else {
        let alias = node.children_with_tokens().filter_map(|el| el.into_token())
            .filter(|t| t.kind() == Ident).last().map(|t| t.text().to_string()).unwrap_or_default();
        format!("import * as {alias} from {src}")
    }
}
```

And refactor `block` per the note (add `block_inline`):

```rust
    fn block(&mut self, node: &SyntaxNode) {
        self.block_inline(node);
        self.out.newline();
    }
    fn block_inline(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("{");
        self.out.newline();
        self.out.indent();
        let stmts: Vec<_> = node.children().filter(|c| c.kind() != Error).collect();
        for (i, s) in stmts.iter().enumerate() {
            if i > 0 {
                let blank = self.comments.leading.get(&s.text_range())
                    .and_then(|l| l.first()).map(|c| c.blank_before).unwrap_or(false);
                if blank { self.out.blank(); }
            }
            self.emit_leading(s);
            self.stmt(s);
            self.emit_trailing(s);
        }
        self.out.dedent();
        self.out.text("}");
    }
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/syntax/format/mod.rs
git commit -m "feat(format): complete statement formatting (if/while/for/enum/import/export)"
```

---

## Task 3: Functions, arrows, and match

**Files:**
- Modify: `src/syntax/format/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn formats_functions_arrows_match() {
        assert_eq!(fmt("async fn f(){return 1}\n"), "async fn f() {\n  return 1\n}\n");
        assert_eq!(fmt("fn* g(){yield 1}\n"), "fn* g() {\n  yield 1\n}\n");
        assert_eq!(fmt("fn add(a:number,b:number):number{return a+b}\n"),
            "fn add(a: number, b: number): number {\n  return a + b\n}\n");
        assert_eq!(fmt("fn v(first,...rest){return rest}\n"),
            "fn v(first, ...rest) {\n  return rest\n}\n");
        assert_eq!(fmt("let f=(x)=>x+1\n"), "let f = (x) => x + 1\n");
        assert_eq!(fmt("let f=async (x)=>x\n"), "let f = async (x) => x\n");
        assert_eq!(fmt(r#"let r=match n{0=>"z",_=>"o"}"#),
            "let r = match n {\n  0 => \"z\",\n  _ => \"o\",\n}\n");
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::format::tests::formats_functions_arrows_match 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Full `fn_decl`/`params`/`arrow_expr`/`match_expr`**

Replace the 4a `fn_decl`/`params` and the `arrow_expr`/`match_expr` stubs:

```rust
    fn fn_decl(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        let toks: Vec<_> = node.children_with_tokens().filter_map(|el| el.into_token()).collect();
        if toks.iter().any(|t| t.kind() == AsyncKw) { self.out.text("async "); }
        self.out.text("fn");
        if toks.iter().any(|t| t.kind() == Star) { self.out.text("*"); }
        self.out.text(" ");
        if let Some(name) = first_ident_text(node) { self.out.text(&name); }
        self.params(node);
        if let Some(rt) = node.children().find(|c| c.kind() == RetType) {
            self.out.text(": ");
            if let Some(ty) = rt.children().find(|c| is_type_kind(c.kind())) { self.type_ann(&ty); }
        }
        self.out.text(" ");
        if let Some(body) = node.children().find(|c| c.kind() == Block) { self.block(&body); }
    }

    fn params(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("(");
        if let Some(list) = node.children().find(|c| c.kind() == ParamList) {
            let params: Vec<_> = list.children().filter(|c| c.kind() == Param).collect();
            for (i, p) in params.iter().enumerate() {
                if i > 0 { self.out.text(", "); }
                if p.children_with_tokens().filter_map(|el| el.into_token()).any(|t| t.kind() == DotDotDot) {
                    self.out.text("...");
                }
                if let Some(name) = first_ident_text(p) { self.out.text(&name); }
                if let Some(ty) = p.children().find(|c| is_type_kind(c.kind())) {
                    self.out.text(": ");
                    self.type_ann(&ty);
                }
            }
        }
        self.out.text(")");
    }

    fn arrow_expr(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        if node.children_with_tokens().filter_map(|el| el.into_token()).any(|t| t.kind() == AsyncKw) {
            self.out.text("async ");
        }
        self.params(node);
        self.out.text(" => ");
        if let Some(body) = node.children().find(|c| c.kind() == Block) {
            self.block_inline(&body);
        } else if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
            self.expr(&e);
        }
    }

    fn match_expr(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("match ");
        if let Some(subj) = node.children().find(|c| is_expr_kind(c.kind())) { self.expr(&subj); }
        self.out.text(" {");
        self.out.newline();
        self.out.indent();
        for arm in node.children().filter(|c| c.kind() == MatchArm) {
            self.emit_leading(&arm);
            self.match_arm(&arm);
            self.emit_trailing(&arm);
        }
        self.out.dedent();
        self.out.text("}");
        // match is an expression; the enclosing stmt emits the trailing newline.
    }

    fn match_arm(&mut self, arm: &SyntaxNode) {
        use SyntaxKind::*;
        // patterns separated by ` | `
        let pats: Vec<_> = arm.children().filter(|c| is_pattern_kind(c.kind())).collect();
        for (i, p) in pats.iter().enumerate() {
            if i > 0 { self.out.text(" | "); }
            self.out.text(&p.text().to_string()); // patterns re-emit canonically as source text (4b: compact)
        }
        if let Some(g) = arm.children().find(|c| c.kind() == MatchGuard) {
            self.out.text(" if ");
            if let Some(e) = g.children().find(|c| is_expr_kind(c.kind())) { self.expr(&e); }
        }
        self.out.text(" => ");
        // arm body is the last expr child
        if let Some(body) = arm.children().filter(|c| is_expr_kind(c.kind())).last() {
            self.expr(&body);
        }
        self.out.text(",");
        self.out.newline();
    }
```

Add the pattern-kind helper used by `match_arm` (free function at file scope):

```rust
fn is_pattern_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        WildcardPat | IdentPat | LiteralPat | RangePat | ArrayPat | ObjectPat | OrPat
    )
}
```

> Patterns re-emit via their source text (already compact); if the corpus shows non-canonical pattern spacing, add a `pattern()` printer mirroring `expr`. The idempotence gate (Task 6) catches instability.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/syntax/format/mod.rs
git commit -m "feat(format): functions, arrows, match"
```

---

## Task 4: Full class (extends, fields with `name?: T` → `name: T?`, defaults, methods)

**Files:**
- Modify: `src/syntax/format/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn formats_full_class() {
        // `name?: T` normalizes to `name: T?`; fields before methods; extends.
        let src = "class Dog extends Animal{ fn greet(){return 1} nickname?:string id:number=0 }\n";
        let out = fmt(src);
        assert!(out.contains("class Dog extends Animal {"), "{out}");
        assert!(out.contains("nickname: string?"), "name?: T -> name: T?:\n{out}");
        assert!(out.contains("id: number = 0"), "{out}");
        let id = out.find("id:").unwrap();
        let greet = out.find("fn greet").unwrap();
        assert!(id < greet, "fields before methods:\n{out}");
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::format::tests::formats_full_class 2>&1 | tail -15`
Expected: FAIL — 4a class omitted `extends`, type normalization, defaults.

- [ ] **Step 3: Complete `class_decl`/`member`**

Replace 4a's `class_decl` header to emit `extends`, and `member`'s `FieldDecl`/`MethodDecl` for full coverage:

```rust
    fn class_decl(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("class ");
        let idents: Vec<String> = node.children_with_tokens().filter_map(|el| el.into_token())
            .filter(|t| t.kind() == Ident).map(|t| t.text().to_string()).collect();
        // first ident = class name; if `extends` present, the second ident is the superclass.
        if let Some(name) = idents.first() { self.out.text(name); }
        let has_extends = node.children_with_tokens().filter_map(|el| el.into_token())
            .any(|t| t.kind() == Ident && t.text() == "extends");
        if has_extends {
            // the superclass is the ident AFTER `extends`
            if let Some(sup) = idents.iter().nth(1) {
                self.out.text(" extends ");
                self.out.text(sup);
            }
        }
        self.out.text(" {");
        self.out.newline();
        self.out.indent();

        let members: Vec<_> = node.children().filter(|c| matches!(c.kind(), FieldDecl | MethodDecl)).collect();
        let ordered: Vec<&SyntaxNode> = members.iter().filter(|m| m.kind() == FieldDecl)
            .chain(members.iter().filter(|m| m.kind() == MethodDecl)).collect();
        for (i, m) in ordered.iter().enumerate() {
            if i > 0 {
                let blank = self.comments.leading.get(&m.text_range())
                    .and_then(|l| l.first()).map(|c| c.blank_before).unwrap_or(false);
                if blank { self.out.blank(); }
            }
            self.emit_leading(m);
            self.member(m);
            self.emit_trailing(m);
        }
        self.out.dedent();
        self.out.text("}");
        self.out.newline();
    }

    fn member(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        match node.kind() {
            FieldDecl => {
                if let Some(name) = first_ident_text(node) { self.out.text(&name); }
                self.out.text(": ");
                // `name?: T` and `name: T?` BOTH normalize to `name: T?`. If the
                // field has the `?` marker token, append `?` to the printed type.
                let has_marker = node.children_with_tokens().filter_map(|el| el.into_token())
                    .any(|t| t.kind() == Question);
                if let Some(ty) = node.children().find(|c| is_type_kind(c.kind())) {
                    self.type_ann(&ty);
                    if has_marker && ty.kind() != OptionalType {
                        self.out.text("?");
                    }
                }
                if let Some(def) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.out.text(" = ");
                    self.expr(&def);
                }
                self.out.newline();
            }
            MethodDecl => {
                let toks: Vec<_> = node.children_with_tokens().filter_map(|el| el.into_token()).collect();
                if toks.iter().any(|t| t.kind() == AsyncKw) { self.out.text("async "); }
                self.out.text("fn");
                if toks.iter().any(|t| t.kind() == Star) { self.out.text("*"); }
                self.out.text(" ");
                if let Some(name) = first_ident_text(node) { self.out.text(&name); }
                self.params(node);
                if let Some(rt) = node.children().find(|c| c.kind() == RetType) {
                    self.out.text(": ");
                    if let Some(ty) = rt.children().find(|c| is_type_kind(c.kind())) { self.type_ann(&ty); }
                }
                self.out.text(" ");
                if let Some(body) = node.children().find(|c| c.kind() == Block) { self.block(&body); }
            }
            _ => { self.out.text(&node.text().to_string()); self.out.newline(); }
        }
    }
```

> `first_ident_text` returns the field/method name (the first IDENT), correct for both. The `name?: T` → `name: T?` normalization: emit `name: <type>` then append `?` when the `?`-marker token was present and the type isn't already optional — matching the spec's canonical form.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/syntax/format/mod.rs
git commit -m "feat(format): full class (extends, fields, name?:T normalization, methods)"
```

---

## Task 5: Type pretty-printing + string/key quote-escape normalization

**Files:**
- Modify: `src/syntax/format/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn formats_types_and_keys() {
        assert_eq!(fmt("let x: array< number > = []\n"), "let x: array<number> = []\n");
        assert_eq!(fmt("let x: map<string,number> = m\n"), "let x: map<string, number> = m\n");
        assert_eq!(fmt("let x: number|string = 1\n"), "let x: number | string = 1\n");
        assert_eq!(fmt("let x: number ? = nil\n"), "let x: number? = nil\n");
        // non-identifier object keys are quoted; identifier-like keys are bare.
        assert_eq!(fmt(r#"{ "a-b": 1, c: 2 }"#.to_string().as_str()), "{\"a-b\": 1, c: 2}\n");
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::format::tests::formats_types_and_keys 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Add `type_ann`, `is_type_kind`, `literal_text`, `object_key`**

Add the type printer + helpers. Replace the Task 1 stub `literal_text`/`object_key`:

```rust
    fn type_ann(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        match node.kind() {
            NamedType => self.out.text(&node.text().to_string().trim().to_string()),
            GenericType => {
                // name<T, T, ...>
                if let Some(name) = first_ident_text(node) { self.out.text(&name); }
                self.out.text("<");
                if let Some(args) = node.children().find(|c| c.kind() == TypeArgs) {
                    let ts: Vec<_> = args.children().filter(|c| is_type_kind(c.kind())).collect();
                    for (i, t) in ts.iter().enumerate() {
                        if i > 0 { self.out.text(", "); }
                        self.type_ann(t);
                    }
                }
                self.out.text(">");
            }
            OptionalType => {
                if let Some(inner) = node.children().find(|c| is_type_kind(c.kind())) { self.type_ann(&inner); }
                self.out.text("?");
            }
            UnionType => {
                let ts: Vec<_> = node.children().filter(|c| is_type_kind(c.kind())).collect();
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 { self.out.text(" | "); }
                    self.type_ann(t);
                }
            }
            TupleType => {
                self.out.text("[");
                let ts: Vec<_> = node.children().filter(|c| is_type_kind(c.kind())).collect();
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 { self.out.text(", "); }
                    self.type_ann(t);
                }
                self.out.text("]");
            }
            _ => self.out.text(&node.text().to_string().trim().to_string()),
        }
    }

    /// Literal text: numbers/bools/nil verbatim; strings re-quoted canonically.
    fn literal_text(&self, node: &SyntaxNode) -> String {
        // A string literal token starts with a quote; re-emit via canonical quoting.
        let raw = node.text().to_string();
        let t = raw.trim();
        if t.starts_with('"') || t.starts_with('\'') {
            requote(t)
        } else {
            t.to_string()
        }
    }

    /// Object key: bare if identifier-like, else a canonically-quoted string.
    fn object_key(&self, node: &SyntaxNode) -> String {
        use SyntaxKind::*;
        // key token is the first ident or string in the ObjectField
        let key_tok = node.children_with_tokens().filter_map(|el| el.into_token())
            .find(|t| matches!(t.kind(), Ident | Str));
        match key_tok {
            Some(t) if t.kind() == Ident => t.text().to_string(),
            Some(t) => {
                // a string key: keep bare if identifier-like, else quote.
                let inner = unquote(t.text());
                if crate::token::is_ident_like(&inner) {
                    inner
                } else {
                    requote(t.text())
                }
            }
            None => String::new(),
        }
    }
```

Add the free helpers (canonical double-quote with escaping, mirroring the legacy `fmt.rs` `escape quotes/backslashes` rule):

```rust
fn is_type_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(kind, NamedType | GenericType | OptionalType | UnionType | TupleType)
}

/// Strip the surrounding quotes from a string literal's raw text.
fn unquote(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2 && (s.starts_with('"') || s.starts_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Canonical double-quoted string: escape backslashes and double quotes.
fn requote(raw: &str) -> String {
    let inner = unquote(raw);
    let mut out = String::from("\"");
    for ch in inner.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}
```

> This mirrors the legacy `fmt.rs` quote/escape rule (commit `eb140d3`: "escape quotes/backslashes in non-identifier object keys") and reuses `token::is_ident_like` (the shared helper from `refactor: share is_ident_like`). String *literals* normalize to double-quoted; template strings are re-emitted verbatim (interpolation preserved) per Task 1.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/syntax/format/mod.rs
git commit -m "feat(format): type pretty-printing + string/key quote-escape normalization"
```

---

## Task 6: Corpus gates — comment-preservation + idempotence

**Files:**
- Create: `tests/cst_format.rs`

- [ ] **Step 1: The acceptance-gate tests**

Create `tests/cst_format.rs`:

```rust
//! Formatter acceptance gates over the whole example corpus:
//! (1) every source comment survives formatting (no data loss — the original bug);
//! (2) formatting is idempotent (`fmt(fmt(x)) == fmt(x)`).

use std::fs;
use std::path::{Path, PathBuf};

fn corpus() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { walk(&p, out); }
            else if p.extension().and_then(|x| x.to_str()) == Some("as") { out.push(p); }
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
    let mut v: Vec<String> = ascript::syntax::lex(src).into_iter()
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
        if before.is_empty() { continue; }
        let formatted = ascript::syntax::format_tree(&src);
        let after = comments_of(&formatted);
        if before != after {
            failures.push(format!("{}: lost/changed comments\n  before={before:?}\n  after ={after:?}",
                path.display()));
        }
    }
    assert!(failures.is_empty(), "comment preservation failures:\n{}", failures.join("\n\n"));
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
    assert!(failures.is_empty(), "idempotence failures:\n{}", failures.join("\n"));
}
```

- [ ] **Step 2: Run the gates (iterate until green)**

Run: `cargo test --test cst_format 2>&1 | tail -30`
Expected: eventually PASS. Failures name the file + the lost comment or non-idempotent case. Common fixes: a node kind still hitting the verbatim fallback (add its rule), pattern/template spacing instability (add a printer), or a comment attached to a node the printer doesn't emit (ensure every attachable node's `emit_leading`/`emit_trailing` are called). Iterate rule-by-rule until both gates pass — this is where full per-node coverage is forced.

- [ ] **Step 3: Commit**

```bash
git add tests/cst_format.rs
git commit -m "test(format): corpus comment-preservation + idempotence gates"
```

---

## Task 7: Wire `ascript fmt` to the new formatter

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Inspect the current Fmt command**

Run: `grep -n "Command::Fmt\|format_source\|fn fmt\|Fmt {" src/main.rs`
Expected: shows the `Command::Fmt { files }` arm calling `ascript::fmt::format_source`.

- [ ] **Step 2: Add a CLI test fixture (round-trip with a comment)**

Create a tiny fixture test in `tests/cst_format.rs`:

```rust
#[test]
fn cli_formatter_preserves_comments_end_to_end() {
    // The new formatter, invoked as the library entry the CLI uses, keeps comments.
    let src = "let x = 1 // keep me\n";
    assert_eq!(ascript::syntax::format_tree(src), "let x = 1 // keep me\n");
}
```

- [ ] **Step 3: Switch the `Fmt` command to the new formatter**

In `src/main.rs`, in the `Command::Fmt { files }` arm, replace the call to `ascript::fmt::format_source(&src)` with the new formatter. The new formatter does not return a `Result` for valid input, but parse errors should still block formatting (don't format broken files). Use the parse + error check:

```rust
        Command::Fmt { files } => {
            let mut had_error = false;
            for file in &files {
                match std::fs::read_to_string(file) {
                    Ok(src) => {
                        let parse = ascript::syntax::parser::parse(&src);
                        if !parse.errors.is_empty() {
                            eprintln!("{}: parse error; not formatting", file);
                            had_error = true;
                            continue;
                        }
                        let formatted = ascript::syntax::format_tree(&src);
                        if let Err(e) = std::fs::write(file, formatted) {
                            eprintln!("{}: {}", file, e);
                            had_error = true;
                        } else {
                            println!("formatted {}", file);
                        }
                    }
                    Err(e) => {
                        eprintln!("{}: {}", file, e);
                        had_error = true;
                    }
                }
            }
            if had_error { ExitCode::from(1) } else { ExitCode::SUCCESS }
        }
```

> Adjust to match the existing `Fmt` arm's exact structure (it may already loop over files / read source). The key change is `format_source` → `syntax::format_tree`, plus refusing to format files with parse errors (so a broken file isn't clobbered). The legacy `src/fmt.rs` stays compiled but unused until the migration's final merge deletes it.

- [ ] **Step 4: Verify end-to-end on a real file**

Run: `cargo build --release 2>&1 | tail -5`
Then create a scratch file and format it:
Run: `printf 'let x = 1 // hi\n/* block */\nlet y = 2\n' > /tmp/fmt_check.as && ./target/release/ascript fmt /tmp/fmt_check.as && cat /tmp/fmt_check.as`
Expected output (comments preserved — the original bug fixed):
```
let x = 1 // hi
/* block */
let y = 2
```

- [ ] **Step 5: Full suite + clippy both configs**

Run: `cargo test 2>&1 | tail -15`
Expected: green (including `cst_format` gates and the CLI fixture).
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs tests/cst_format.rs
git commit -m "feat(fmt): wire ascript fmt to the comment-preserving CST formatter"
```

---

## Done criteria for Plan 4b (and the comment-preserving formatter)

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs.
- [ ] Every expression, statement, declaration, and type formats canonically (no verbatim-fallback paths remain for valid constructs).
- [ ] `name?: T` → `name: T?` normalization; string/key quote-escape normalization (reusing `is_ident_like`); fields-before-methods reordering — all hold.
- [ ] **Acceptance gate:** every source comment survives formatting over the whole corpus, and formatting is idempotent over the whole corpus.
- [ ] `ascript fmt` uses the new formatter and **preserves comments end-to-end** (the original bug is fixed); broken files are not clobbered.
- [ ] The interpreter still runs on the legacy front-end (unchanged); `src/fmt.rs` is dead but present until the final migration merge.

**Front-end complete.** With Plans 1–4b the new pipeline lexes losslessly, parses the whole grammar into a typed CST, resolves names/slots/upvalues, and **formats while preserving comments** — the original ask, shipped. Remaining for the broader effort: the **bytecode VM + GC** vertical-slice plans (written just-in-time, now that the typed AST + resolver APIs are concrete) and the **checker** (Spec 3 + plans).
