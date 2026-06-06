# CST Comment-Preserving Formatter — Machinery (Plan 4a)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the hard, novel core of the comment-preserving formatter — a comment **attachment/classification** pass (leading vs trailing via newline analysis, with blank-line counting) and a CST pretty-printer that emits attached comments, **carrying them through node reordering** (the fields-before-methods case) — proven on focused fixtures. This is the mechanism that fixes the original bug (the formatter silently dropping comments).

**Architecture:** Two pieces. (1) `comments.rs`: a pre-pass over the CST that classifies each comment token as *trailing* the preceding statement/member or *leading* the following one (decided by whether a `Newline` separates the comment from prior content), records the blank-line-before count, and builds a `CommentMap` keyed by the **attachable** node's `TextRange`. Because comments attach to *nodes*, reordering a node carries its comments. (2) `format/mod.rs`: a CST-walking pretty-printer with an indentation-aware output builder that, around each attachable node, emits its leading comments (with the blank-line rule) and trailing comment. This plan implements the machinery + a representative slice (incl. the class fields-before-methods reorder) to prove it; **Plan 4b** completes per-node canonical rules for the whole grammar and adds the full-corpus gate + `ascript fmt` cutover.

**Tech Stack:** Rust, the Plan 1/2/2b CST + typed AST, `cstree` red-tree API (`parent`, `first_token`, `descendants_with_tokens`, `text_range`).

**Scope note:** Plan 4a of the CST front-end (spec: `docs/superpowers/specs/2026-06-02-cst-frontend-migration-design.md`). Delivers the comment machinery + blank-line rule + reorder-carrying, gated on focused fixtures (not the full corpus). The legacy `src/fmt.rs` and `ascript fmt` are **untouched** here — the new formatter lives alongside under `src/syntax/format/` until Plan 4b wires the CLI to it. Depends on Plans 2 / 2b.

---

## File Structure

- Create `src/syntax/format/mod.rs` — `format(root) -> String`, the output builder, and the pretty-printer (representative node rules for this plan).
- Create `src/syntax/format/comments.rs` — the `CommentMap` + attachment pass.
- Modify `src/syntax/mod.rs` — `pub mod format;` + a `format_tree(src) -> String` convenience.

**Attachment granularity:** comments attach to the nearest *attachable* node — a statement (child of `SourceFile`/`Block`), a class/enum member (`FieldDecl`/`MethodDecl`/`EnumVariant`), or a top-level declaration. This is the granularity at which the formatter reorders, so node-attachment makes comments move with their owner.

---

## Task 1: Output builder + printer skeleton (no comments yet)

**Files:**
- Create: `src/syntax/format/mod.rs`
- Modify: `src/syntax/mod.rs`

- [ ] **Step 1: Scaffold + a canonical-layout test for a tiny program**

Create `src/syntax/format/mod.rs`:

```rust
//! CST-walking pretty-printer. Imposes canonical layout while re-emitting
//! comments (see comments.rs). This plan (4a) covers the machinery + a
//! representative node slice; Plan 4b completes per-node coverage.

pub mod comments;

use crate::syntax::cst::SyntaxNode;
use crate::syntax::kind::SyntaxKind;

/// Indentation-aware output builder.
struct Out {
    buf: String,
    indent: usize,
    /// True at the start of a line (so we know to emit indentation).
    at_line_start: bool,
}

impl Out {
    fn new() -> Self {
        Out { buf: String::new(), indent: 0, at_line_start: true }
    }
    /// Emit raw text on the current line (writing pending indentation first).
    fn text(&mut self, s: &str) {
        if self.at_line_start && !s.is_empty() {
            for _ in 0..self.indent {
                self.buf.push_str("  "); // 2-space indent (match legacy fmt)
            }
            self.at_line_start = false;
        }
        self.buf.push_str(s);
    }
    /// End the current line.
    fn newline(&mut self) {
        // Trim trailing spaces before the newline.
        while self.buf.ends_with(' ') {
            self.buf.pop();
        }
        self.buf.push('\n');
        self.at_line_start = true;
    }
    /// Emit ONE blank line. Precondition: the buffer already ends with a newline
    /// (every statement/comment emitter ends with `newline()`), so a single extra
    /// '\n' yields exactly one blank line. Used by the blank-line rule.
    fn blank(&mut self) {
        debug_assert!(self.buf.ends_with('\n'));
        self.buf.push('\n');
        self.at_line_start = true;
    }
    fn indent(&mut self) { self.indent += 1; }
    fn dedent(&mut self) { self.indent = self.indent.saturating_sub(1); }
}

/// Format a parsed source tree into canonical text.
pub fn format(root: &SyntaxNode) -> String {
    let comments = comments::attach(root);
    let mut out = Out::new();
    let mut p = Printer { out: &mut out, comments: &comments };
    p.source_file(root);
    // Ensure exactly one trailing newline.
    let mut s = out.buf;
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
    s
}

struct Printer<'a> {
    out: &'a mut Out,
    comments: &'a comments::CommentMap,
}

impl Printer<'_> {
    fn source_file(&mut self, node: &SyntaxNode) {
        let stmts: Vec<_> = node.children().collect();
        for (i, stmt) in stmts.iter().enumerate() {
            self.stmt(stmt);
            if i + 1 < stmts.len() {
                self.out.newline();
            }
        }
    }

    /// Format a statement. This plan handles ExprStmt + a few forms; Plan 4b
    /// completes the match.
    fn stmt(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        match node.kind() {
            ExprStmt => {
                // its single child is the expression
                if let Some(e) = node.children().next() {
                    self.expr(&e);
                }
                self.out.newline();
            }
            _ => {
                // Fallback for this plan: emit the node's source text verbatim
                // (canonical rules for these kinds arrive in Task 4 / Plan 4b).
                self.out.text(&node.text().to_string());
                self.out.newline();
            }
        }
    }

    /// Format an expression (representative subset for 4a).
    fn expr(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        match node.kind() {
            Literal | NameRef => self.out.text(&node.text().to_string()),
            BinaryExpr => {
                let kids: Vec<_> = node.children().collect();
                // children: lhs, rhs; the operator is a token between them.
                let op = node
                    .children_with_tokens()
                    .filter_map(|el| el.into_token())
                    .find(|t| !t.kind().is_trivia())
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();
                if let Some(l) = kids.first() {
                    self.expr(l);
                }
                self.out.text(&format!(" {op} "));
                if let Some(r) = kids.get(1) {
                    self.expr(r);
                }
            }
            _ => self.out.text(&node.text().to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_to_tree;

    fn fmt(src: &str) -> String {
        format(&parse_to_tree(src))
    }

    #[test]
    fn canonicalizes_binary_spacing() {
        assert_eq!(fmt("1+2"), "1 + 2\n");
        assert_eq!(fmt("1   +    2"), "1 + 2\n");
    }
}
```

- [ ] **Step 2: Wire the module**

In `src/syntax/mod.rs` add:

```rust
pub mod format;

/// Parse + format in one step (convenience for tests/tools).
pub fn format_tree(src: &str) -> String {
    format::format(&parse_to_tree(src))
}
```

> `comments::attach` doesn't exist yet (Task 2). For Task 1, temporarily stub it: add `pub mod comments;` with a minimal `CommentMap`/`attach` (Task 2 fills it). To keep Task 1 runnable, create `comments.rs` now with the empty stub in Step 3.

- [ ] **Step 3: Minimal `comments.rs` stub so Task 1 compiles**

Create `src/syntax/format/comments.rs`:

```rust
//! Comment attachment pass — full implementation in Task 2.

use crate::syntax::cst::SyntaxNode;
use cstree::text::TextRange;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct CommentMap {
    /// attachable-node range → leading comments (in order).
    pub leading: HashMap<TextRange, Vec<Leading>>,
    /// attachable-node range → trailing same-line comment text.
    pub trailing: HashMap<TextRange, String>,
}

#[derive(Debug, Clone)]
pub struct Leading {
    pub text: String,
    /// True if a blank line should precede this comment (blank-line rule).
    pub blank_before: bool,
}

/// Build the comment map for `root`. (Task 1 stub: returns empty.)
pub fn attach(_root: &SyntaxNode) -> CommentMap {
    CommentMap::default()
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format::tests::canonicalizes_binary_spacing 2>&1 | tail -15`
Expected: PASS. (If `node.text()` requires a resolver, the tree is resolver-backed per Plans 1/2 — use the same text accessor.)

```bash
git add src/syntax/format/ src/syntax/mod.rs
git commit -m "feat(format): output builder + printer skeleton (binary spacing)"
```

---

## Task 2: Comment attachment pass (leading/trailing classification + blank-line counting)

**Files:**
- Modify: `src/syntax/format/comments.rs`

- [ ] **Step 1: Write classification tests**

Add to a `tests` mod at the bottom of `src/syntax/format/comments.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_to_tree;
    use crate::syntax::kind::SyntaxKind;

    fn first_stmt(root: &SyntaxNode) -> SyntaxNode {
        root.children().next().unwrap()
    }

    #[test]
    fn standalone_comment_is_leading() {
        let root = parse_to_tree("// hello\nx\n");
        let map = attach(&root);
        let s = first_stmt(&root); // the ExprStmt for `x`
        let lead = map.leading.get(&s.text_range()).expect("leading on first stmt");
        assert_eq!(lead.len(), 1);
        assert_eq!(lead[0].text, "// hello");
    }

    #[test]
    fn same_line_comment_is_trailing() {
        let root = parse_to_tree("x // tail\ny\n");
        let map = attach(&root);
        let s = first_stmt(&root); // ExprStmt for `x`
        assert_eq!(map.trailing.get(&s.text_range()).map(|t| t.as_str()), Some("// tail"));
    }

    #[test]
    fn blank_line_before_comment_is_recorded() {
        let root = parse_to_tree("a\n\n// grouped\nb\n");
        let map = attach(&root);
        // second statement (`b`) carries the leading comment with blank_before.
        let b = root.children().nth(1).unwrap();
        let lead = map.leading.get(&b.text_range()).expect("leading on b");
        assert!(lead[0].blank_before, "2+ newlines before comment → blank line preserved");
    }
}
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::format::comments 2>&1 | tail -15`
Expected: FAIL — `attach` is a stub.

- [ ] **Step 3: Implement `attach`**

Replace the stub `attach` in `src/syntax/format/comments.rs` with the classification pass. Walk the tree's tokens in source order; classify each comment by newline context; attach to the nearest *attachable* node.

```rust
use crate::syntax::kind::SyntaxKind;

/// Attachable node kinds — the granularity comments attach to.
fn is_attachable(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        LetStmt | ExprStmt | Block | IfStmt | WhileStmt | ReturnStmt | FnDecl
            | ForStmt | BreakStmt | ContinueStmt | EnumDecl | ClassDecl
            | ImportStmt | ExportStmt | FieldDecl | MethodDecl | EnumVariant
    )
}

/// Nearest attachable ancestor of a node (including itself).
fn attachable_of(node: &SyntaxNode) -> Option<SyntaxNode> {
    let mut cur = Some(node.clone());
    while let Some(n) = cur {
        if is_attachable(n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

pub fn attach(root: &SyntaxNode) -> CommentMap {
    use SyntaxKind::*;
    let mut map = CommentMap::default();

    // Pending leading comments + the blank-line state seen before them.
    let mut pending: Vec<Leading> = Vec::new();
    // Newlines seen since the last non-trivia token (for trailing vs leading)
    // and since the last comment (for blank-line-before).
    let mut newlines_since_content = 0usize;
    // The attachable node owning the most recent non-trivia token (trailing target).
    let mut last_content_node: Option<SyntaxNode> = None;

    for tok in root.descendants_with_tokens().filter_map(|el| el.into_token()) {
        match tok.kind() {
            Newline => {
                newlines_since_content += 1;
            }
            Whitespace => { /* ignore */ }
            LineComment | BlockComment => {
                let text = tok.text().to_string();
                if newlines_since_content == 0 && last_content_node.is_some() {
                    // Same line as preceding content → trailing comment.
                    if let Some(n) = &last_content_node {
                        if let Some(a) = attachable_of(n) {
                            map.trailing.insert(a.text_range(), text);
                        }
                    }
                } else {
                    // Own line → leading comment of the next content node.
                    pending.push(Leading { text, blank_before: newlines_since_content >= 2 });
                }
                // After a comment, reset the newline counter so a blank line
                // BETWEEN two stacked leading comments is measured fresh.
                newlines_since_content = 0;
            }
            _ => {
                // A non-trivia token: flush pending leading comments onto the
                // attachable node this token belongs to.
                if !pending.is_empty() {
                    if let Some(a) = tok.parent().and_then(|p| attachable_of(&p)) {
                        map.leading.entry(a.text_range()).or_default().append(&mut pending);
                    } else {
                        pending.clear();
                    }
                }
                last_content_node = tok.parent();
                newlines_since_content = 0;
            }
        }
    }

    map
}
```

> Classification rule: a comment with **no `Newline` before it since the last real token** is *trailing* that token's statement; otherwise it's *leading* the next statement, and `blank_before` is set when 2+ newlines preceded it (the blank-line rule's "1 significant blank line"). Stacked leading comments each get their own `blank_before` from the newlines between them.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format::comments 2>&1 | tail -15`
Expected: PASS.

```bash
git add src/syntax/format/comments.rs
git commit -m "feat(format): comment attachment pass (leading/trailing + blank-line)"
```

---

## Task 3: Emit attached comments + the blank-line rule

**Files:**
- Modify: `src/syntax/format/mod.rs`

- [ ] **Step 1: Comment-preservation + blank-line tests**

Add to the `tests` mod in `src/syntax/format/mod.rs`:

```rust
    #[test]
    fn preserves_leading_comment() {
        assert_eq!(fmt("// hi\nx\n"), "// hi\nx\n");
    }

    #[test]
    fn preserves_trailing_comment() {
        assert_eq!(fmt("x // tail\n"), "x // tail\n");
    }

    #[test]
    fn blank_line_rule() {
        // 2+ blank lines collapse to 1; the grouping blank line is preserved.
        assert_eq!(fmt("a\n\n\n\nb\n"), "a\n\nb\n");
        // a single blank line between items is preserved.
        assert_eq!(fmt("a\n\nb\n"), "a\n\nb\n");
        // no blank line stays no blank line.
        assert_eq!(fmt("a\nb\n"), "a\nb\n");
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::format::tests::preserves_leading_comment 2>&1 | tail -15`
Expected: FAIL — the printer ignores comments and the blank-line state.

- [ ] **Step 3: Emit comments around attachable nodes + apply the blank-line rule**

Add comment-emitting helpers to `Printer` and call them from `source_file`/`stmt`. Replace `source_file` and add helpers:

```rust
    fn source_file(&mut self, node: &SyntaxNode) {
        let stmts: Vec<_> = node.children().collect();
        for (i, stmt) in stmts.iter().enumerate() {
            // Each statement/comment emitter ends with a newline, so items are
            // already separated by one '\n'. Between items we add ONE extra blank
            // line iff the blank-line rule wants it (≥1 source blank line, driven
            // by the first leading comment's `blank_before` or, for bare items,
            // the source gap).
            if i > 0 {
                let lead = self.comments.leading.get(&stmt.text_range());
                let want_blank = lead
                    .and_then(|l| l.first())
                    .map(|c| c.blank_before)
                    .unwrap_or_else(|| self.blank_between_bare(&stmts[i - 1], stmt));
                if want_blank {
                    self.out.blank();
                }
            }
            self.emit_leading(stmt);
            self.stmt(stmt);
            self.emit_trailing(stmt);
        }
    }

    fn emit_leading(&mut self, node: &SyntaxNode) {
        if let Some(comments) = self.comments.leading.get(&node.text_range()).cloned() {
            for (i, c) in comments.iter().enumerate() {
                if i > 0 && c.blank_before {
                    self.out.blank(); // blank line between stacked leading comments
                }
                self.out.text(&c.text);
                self.out.newline();
            }
        }
    }

    fn emit_trailing(&mut self, node: &SyntaxNode) {
        if let Some(c) = self.comments.trailing.get(&node.text_range()).cloned() {
            // Append on the same line as the statement just emitted. `stmt` ended
            // with a newline; back up to attach the trailing comment.
            self.append_trailing(&c);
        }
    }
```

The trailing comment needs to land on the *same* line as the statement. Since `stmt` ends with `newline()`, add a method on `Out` to append before the last newline:

```rust
impl Out {
    /// Append ` <comment>` at the end of the last non-empty line (before its
    /// trailing newline). Used for same-line trailing comments.
    fn append_to_prev_line(&mut self, comment: &str) {
        // Drop the trailing '\n' (and any blank), append " comment", re-add '\n'.
        while self.buf.ends_with('\n') {
            self.buf.pop();
        }
        self.buf.push(' ');
        self.buf.push_str(comment);
        self.buf.push('\n');
        self.at_line_start = true;
    }
}
```

and in `Printer`:

```rust
    fn append_trailing(&mut self, comment: &str) {
        self.out.append_to_prev_line(comment);
    }

    /// Heuristic blank-line preservation between two bare statements (no leading
    /// comment driving it): preserved by the comment pass for commented items;
    /// for bare items 4a keeps it simple — preserve exactly one blank when the
    /// source had ≥1 blank line between them. Implemented via a source-gap check.
    fn blank_between_bare(&self, prev: &SyntaxNode, next: &SyntaxNode) -> bool {
        // Count Newline trivia tokens strictly between prev's end and next's start.
        let gap_start = prev.text_range().end();
        let gap_end = next.text_range().start();
        let mut newlines = 0usize;
        let root = next.ancestors().last().unwrap_or_else(|| next.clone());
        for t in root.descendants_with_tokens().filter_map(|el| el.into_token()) {
            let r = t.text_range();
            if r.start() >= gap_start && r.end() <= gap_end
                && t.kind() == SyntaxKind::Newline
            {
                newlines += 1;
            }
        }
        newlines >= 2
    }
```

> The blank-line rule: between two items, ≥2 source newlines (i.e. ≥1 blank line) → exactly one blank line in output; otherwise none. For commented items the first leading comment's `blank_before` drives it; for bare items `blank_between_bare` counts the gap. Stacked leading comments preserve a single blank between them via `blank_before`.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format 2>&1 | tail -20`
Expected: comment-preservation + blank-line tests PASS.

```bash
git add src/syntax/format/mod.rs
git commit -m "feat(format): emit leading/trailing comments + blank-line rule"
```

---

## Task 4: A few real statement rules (so comments attach to structured nodes)

This task adds canonical formatting for `LetStmt` and `Block`/`FnDecl` so the reorder test in Task 5 has real structured output (the Task 1 fallback emits verbatim text, which wouldn't reflow). Full coverage is Plan 4b.

**Files:**
- Modify: `src/syntax/format/mod.rs`

- [ ] **Step 1: Tests**

Add to the `tests` mod:

```rust
    #[test]
    fn formats_let_and_fn() {
        assert_eq!(fmt("let   x=1"), "let x = 1\n");
        assert_eq!(
            fmt("fn f(a,b){return a+b}"),
            "fn f(a, b) {\n  return a + b\n}\n"
        );
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::format::tests::formats_let_and_fn 2>&1 | tail -15`
Expected: FAIL — `LetStmt`/`FnDecl` hit the verbatim fallback.

- [ ] **Step 3: Add `LetStmt`, `Block`, `FnDecl`, `ReturnStmt` rules**

In `Printer::stmt`, replace the catch-all with real arms for these kinds (keep the verbatim fallback for the rest, to be filled in Plan 4b):

```rust
    fn stmt(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        match node.kind() {
            ExprStmt => {
                if let Some(e) = node.children().next() {
                    self.expr(&e);
                }
                self.out.newline();
            }
            LetStmt => {
                // `let`/`const` keyword + name + optional `= init`
                let kw = first_kw_text(node);
                self.out.text(&kw);
                self.out.text(" ");
                if let Some(name) = first_ident_text(node) {
                    self.out.text(&name);
                }
                if let Some(init) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.out.text(" = ");
                    self.expr(&init);
                }
                self.out.newline();
            }
            ReturnStmt => {
                self.out.text("return");
                if let Some(e) = node.children().find(|c| is_expr_kind(c.kind())) {
                    self.out.text(" ");
                    self.expr(&e);
                }
                self.out.newline();
            }
            Block => self.block(node),
            FnDecl => self.fn_decl(node),
            _ => {
                self.out.text(&node.text().to_string());
                self.out.newline();
            }
        }
    }

    fn block(&mut self, node: &SyntaxNode) {
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
        self.out.newline();
    }

    fn fn_decl(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        // `[async] fn[*] name(params) [: ret]` then the body block.
        self.out.text("fn ");
        if let Some(name) = first_ident_text(node) {
            self.out.text(&name);
        }
        self.params(node);
        self.out.text(" ");
        if let Some(body) = node.children().find(|c| c.kind() == Block) {
            self.block(&body);
        }
    }

    fn params(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("(");
        if let Some(list) = node.children().find(|c| c.kind() == ParamList) {
            let params: Vec<_> = list.children().filter(|c| c.kind() == Param).collect();
            for (i, p) in params.iter().enumerate() {
                if i > 0 { self.out.text(", "); }
                if let Some(name) = first_ident_text(p) {
                    self.out.text(&name);
                }
            }
        }
        self.out.text(")");
    }
```

Add the small helpers:

```rust
fn first_kw_text(node: &SyntaxNode) -> String {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| matches!(t.kind(), SyntaxKind::LetKw | SyntaxKind::ConstKw))
        .map(|t| t.text().to_string())
        .unwrap_or_else(|| "let".to_string())
}

fn first_ident_text(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::Ident)
        .map(|t| t.text().to_string())
}

fn is_expr_kind(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        Literal | NameRef | UnaryExpr | BinaryExpr | ParenExpr | CallExpr | MemberExpr
            | IndexExpr | ArrowExpr | AssignExpr | ArrayExpr | ObjectExpr | TemplateExpr
            | OptMemberExpr | TryExpr | UnwrapExpr | TernaryExpr | AwaitExpr | YieldExpr
            | MatchExpr | RangeExpr
    )
}
```

> Note: `async`/`fn*`/return-type/param-types are simplified here (4a). Plan 4b adds them; the `formats_let_and_fn` test uses a plain `fn`.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format 2>&1 | tail -20`
Expected: PASS.

```bash
git add src/syntax/format/mod.rs
git commit -m "feat(format): let/return/block/fn canonical rules (4a slice)"
```

---

## Task 5: The key proof — comments carry through fields-before-methods reordering

**Files:**
- Modify: `src/syntax/format/mod.rs`

- [ ] **Step 1: The reorder-carries-comments test**

Add to the `tests` mod:

```rust
    #[test]
    fn class_reorders_fields_before_methods_carrying_comments() {
        // Source: a method appears BEFORE a field, each with its own comment.
        // Canonical layout puts fields first — and each comment must travel with
        // its owner.
        let src = "class C {\n  // the greet method\n  fn greet() { return 1 }\n  // the name field\n  name: string\n}\n";
        let out = fmt(src);
        // Field (and its comment) must come before the method (and its comment).
        let name_pos = out.find("name: string").expect("field present");
        let greet_pos = out.find("fn greet").expect("method present");
        assert!(name_pos < greet_pos, "fields must be reordered before methods:\n{out}");
        let name_c = out.find("// the name field").expect("field comment present");
        let greet_c = out.find("// the greet method").expect("method comment present");
        assert!(name_c < name_pos && name_pos < greet_c,
            "each comment must travel with its member:\n{out}");
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::format::tests::class_reorders 2>&1 | tail -20`
Expected: FAIL — `ClassDecl` hits the verbatim fallback (no reordering, comments not re-attached structurally).

- [ ] **Step 3: Add `ClassDecl` with fields-before-methods reordering**

In `Printer::stmt`, add a `ClassDecl => self.class_decl(node),` arm and implement it. The crux: collect members, **partition fields before methods (stable within each group)**, and for each member emit its attached comments via `emit_leading`/`emit_trailing` — which works *because comments are keyed by the member node's range, not its source position*.

```rust
    fn class_decl(&mut self, node: &SyntaxNode) {
        use SyntaxKind::*;
        self.out.text("class ");
        if let Some(name) = first_ident_text(node) {
            self.out.text(&name);
        }
        // (extends clause omitted in 4a; added in Plan 4b)
        self.out.text(" {");
        self.out.newline();
        self.out.indent();

        let members: Vec<_> = node
            .children()
            .filter(|c| matches!(c.kind(), FieldDecl | MethodDecl))
            .collect();
        let fields = members.iter().filter(|m| m.kind() == FieldDecl);
        let methods = members.iter().filter(|m| m.kind() == MethodDecl);
        let ordered: Vec<&SyntaxNode> = fields.chain(methods).collect();

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
                // `name: Type [= default]` — simplified for 4a (verbatim type/default).
                if let Some(name) = first_ident_text(node) {
                    self.out.text(&name);
                }
                self.out.text(": ");
                // emit the type/default portion verbatim for 4a (Plan 4b normalizes
                // `name?: T` → `name: T?` and pretty-prints the type).
                let ty = node
                    .children()
                    .find(|c| matches!(c.kind(), NamedType | GenericType | OptionalType | UnionType | TupleType))
                    .map(|t| t.text().to_string())
                    .unwrap_or_default();
                self.out.text(&ty);
                self.out.newline();
            }
            MethodDecl => {
                self.out.text("fn ");
                if let Some(name) = first_ident_text(node) {
                    self.out.text(&name);
                }
                self.params(node);
                self.out.text(" ");
                if let Some(body) = node.children().find(|c| c.kind() == Block) {
                    self.block(&body);
                }
            }
            _ => {
                self.out.text(&node.text().to_string());
                self.out.newline();
            }
        }
    }
```

> This is the plan's central proof: because `emit_leading`/`emit_trailing` look comments up by the **member node's `TextRange`**, partitioning the members into fields-then-methods automatically moves each comment with its member. A position-keyed comment store (the approach the spec rejected) would mis-place them here.

- [ ] **Step 4: Run + commit**

Run: `cargo test --lib syntax::format 2>&1 | tail -20`
Expected: the reorder-carries-comments test + all prior PASS.

```bash
git add src/syntax/format/mod.rs
git commit -m "feat(format): class fields-before-methods reorder carries attached comments"
```

---

## Task 6: Idempotence on the 4a slice

**Files:**
- Modify: `src/syntax/format/mod.rs`

- [ ] **Step 1: Idempotence test**

Add to the `tests` mod:

```rust
    #[test]
    fn idempotent_on_slice() {
        for src in [
            "1+2\n",
            "// hi\nx\n",
            "x // tail\n",
            "a\n\n\nb\n",
            "let x=1\n",
            "fn f(a,b){return a+b}\n",
            "class C {\n  // m\n  fn greet() { return 1 }\n  // f\n  name: string\n}\n",
        ] {
            let once = fmt(src);
            let twice = fmt(&once);
            assert_eq!(once, twice, "fmt not idempotent for {src:?}:\n{once}\n---\n{twice}");
        }
    }
```

- [ ] **Step 2: Run**

Run: `cargo test --lib syntax::format::tests::idempotent_on_slice 2>&1 | tail -20`
Expected: PASS. If a case is not idempotent, the second pass reveals an instability (most likely blank-line handling or trailing-comment spacing) — fix the offending rule until `fmt(fmt(x)) == fmt(x)`.

- [ ] **Step 3: Full suite + clippy both configs + commit**

Run: `cargo test 2>&1 | tail -15`
Expected: green (legacy `fmt` untouched; new formatter additive).
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

```bash
git add src/syntax/format/mod.rs
git commit -m "test(format): idempotence on the 4a slice"
```

---

## Done criteria for Plan 4a

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs.
- [ ] Comments are **classified** correctly (leading vs trailing via newline analysis) and **preserved** in output.
- [ ] The **blank-line rule** holds (≥1 source blank line → exactly one; else none; 2+ collapse).
- [ ] **The central proof:** class members reorder fields-before-methods and **each comment travels with its member** (node-attachment, not position).
- [ ] Idempotence holds on the 4a slice.
- [ ] The legacy `src/fmt.rs` and `ascript fmt` are **unchanged** (new formatter is additive, not yet wired to the CLI).

**Next plan:** `cst-formatter-coverage.md` (Plan 4b) — complete canonical rules for **every** node kind (full expressions incl. arrays/objects/templates/ternary/match/optional-chaining/unwrap; all statements; `async`/`fn*`/rest/param+return types; `enum`; full `class` incl. `extends`, typed/optional/defaulted fields with **`name?: T` → `name: T?`** normalization; `import`/`export`; type pretty-printing; **quote/escape normalization** for string and object keys), then the acceptance gates: **comment-preservation + idempotence over the entire `examples/**/*.as` corpus**, and finally **wire `ascript fmt` to the new formatter** (replacing the comment-dropping legacy path) with a differential check that non-commented files format identically to the legacy formatter.
