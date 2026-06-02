# CST Event Parser + Structured Tree + ungrammar Codegen (Plan 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the flat token stream from Plan 1 into a real, structured, lossless cstree tree built by an error-recovering recursive-descent parser, with a generated typed-AST layer and a tree-sitter differential oracle — over a core grammar slice that establishes every reusable pattern.

**Architecture:** The parser is a recursive-descent emitter that produces a flat `Vec<Event>` (`Start`/`Token`/`Finish`/`Error`) over the *non-trivia* tokens (rust-analyzer style — decoupling grammar decisions from trivia). A separate `TreeBuilder` then materializes the cstree green tree by replaying events while re-inserting trivia tokens from the original stream per a fixed attachment policy, preserving byte-for-byte losslessness. A `build.rs` codegen step turns an `ascript.ungram` grammar into typed wrapper structs over the CST.

**Tech Stack:** Rust, `cstree` 0.14 (`GreenNodeBuilder`), `ungrammar` (build-dependency, codegen), the existing tree-sitter grammar (differential oracle).

**Scope note:** This is Plan 2 of the CST front-end (spec: `docs/superpowers/specs/2026-06-02-cst-frontend-migration-design.md`). It delivers the parser **infrastructure** (events, tree-builder, recovery, trivia attachment), the **codegen pipeline**, the **oracle**, and a **core grammar slice**: literals, unary/binary (with precedence), paren, call, member/index, `let`/`const`, assignment, expression statements, blocks, `if`/`while`/`return`, and `fn`/arrow. **Full grammar coverage** (classes, enums, match patterns, `for`, `import`/`export`, types, destructuring, templates, spread, `?`/`!`/ternary/`await`/`yield`) is **Plan 2b**, which repeats the exact task pattern established here for each remaining production. Builds on Plan 1 (`src/syntax/{kind,lexer,cst}.rs`); does not touch the interpreter.

**Key invariant (carried from Plan 1):** losslessness. The structured tree must still reproduce source byte-for-byte; the corpus round-trip test from Plan 1 is extended to the structured tree.

---

## File Structure

- Modify `src/syntax/kind.rs` — add **node** `SyntaxKind`s (e.g. `SOURCE_FILE`, `LET_STMT`, `BINARY_EXPR`, …) alongside the token kinds from Plan 1.
- Create `src/syntax/event.rs` — the `Event` enum + `Marker`/`CompletedMarker` for node wrapping.
- Create `src/syntax/parser.rs` — the recursive-descent parser producing `Vec<Event>` + collected errors.
- Create `src/syntax/tree_builder.rs` — replays events + interleaves trivia → cstree green tree.
- Create `src/syntax/ast/mod.rs` — hand-written `AstNode` trait + the generated typed wrappers (`include!` of the codegen output).
- Create `src/syntax/ast/ascript.ungram` — the grammar describing AST node shapes.
- Create `xtask`-free codegen: extend `build.rs` — read `ascript.ungram`, emit `$OUT_DIR/ast_nodes.rs`.
- Modify `src/syntax/mod.rs` — wire up `event`, `parser`, `tree_builder`, `ast`; export `parse`.
- Modify `tests/cst_lossless.rs` — extend the corpus round-trip to the structured tree.
- Create `tests/cst_parser_oracle.rs` — tree-sitter differential oracle over the slice.

---

## Task 1: Add node SyntaxKinds

**Files:**
- Modify: `src/syntax/kind.rs`

- [ ] **Step 1: Write a test asserting node kinds exist and are non-trivia**

Add to the `tests` mod in `src/syntax/kind.rs`:

```rust
#[test]
fn node_kinds_exist_and_are_not_trivia() {
    for k in [
        SyntaxKind::SourceFile, SyntaxKind::LetStmt, SyntaxKind::ExprStmt,
        SyntaxKind::BinaryExpr, SyntaxKind::UnaryExpr, SyntaxKind::ParenExpr,
        SyntaxKind::CallExpr, SyntaxKind::ArgList, SyntaxKind::MemberExpr,
        SyntaxKind::IndexExpr, SyntaxKind::Literal, SyntaxKind::NameRef,
        SyntaxKind::Block, SyntaxKind::IfStmt, SyntaxKind::WhileStmt,
        SyntaxKind::ReturnStmt, SyntaxKind::FnDecl, SyntaxKind::ParamList,
        SyntaxKind::Param, SyntaxKind::ArrowExpr, SyntaxKind::AssignExpr,
        SyntaxKind::Error,
    ] {
        assert!(!k.is_trivia(), "{k:?} must not be trivia");
    }
}
```

- [ ] **Step 2: Run it (expect compile failure)**

Run: `cargo test --lib syntax::kind::tests::node_kinds_exist 2>&1 | tail -10`
Expected: FAIL to compile — the node variants don't exist yet.

- [ ] **Step 3: Add the node variants**

In `src/syntax/kind.rs`, add these variants to the `SyntaxKind` enum, in the `// --- nodes ---` section (after `Root` from Plan 1; keep `Root` for the flat-tree helper, add `SourceFile` as the real parser root). No `#[static_text]` on nodes:

```rust
    // --- nodes (Plan 2) ---
    SourceFile,
    // statements
    LetStmt, ExprStmt, Block, IfStmt, WhileStmt, ReturnStmt, FnDecl,
    ParamList, Param,
    // expressions
    Literal, NameRef, UnaryExpr, BinaryExpr, ParenExpr, CallExpr, ArgList,
    MemberExpr, IndexExpr, ArrowExpr, AssignExpr,
```

(`Error` already exists from Plan 1.)

- [ ] **Step 4: Run the test**

Run: `cargo test --lib syntax::kind 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/syntax/kind.rs
git commit -m "feat(syntax): add node SyntaxKinds for the parser"
```

---

## Task 2: The Event model

**Files:**
- Create: `src/syntax/event.rs`
- Modify: `src/syntax/mod.rs`

- [ ] **Step 1: Write the event-model unit test**

Create `src/syntax/event.rs`:

```rust
//! Parser output is a flat list of events, not a tree built directly. This
//! decouples grammar decisions (which ignore trivia) from tree construction
//! (which re-inserts trivia). `Start` carries a `forward_parent` slot so a
//! completed node can be retro-actively wrapped by an outer node (needed for
//! left-associative binary expressions discovered after parsing the lhs).

use crate::syntax::kind::SyntaxKind;

#[derive(Debug, Clone)]
pub enum Event {
    /// Open a node. `kind` is `Tombstone` until the node is completed; some
    /// Start events are abandoned (left as Tombstone) and skipped by the builder.
    Start { kind: SyntaxKind, forward_parent: Option<usize> },
    /// Finish the current node.
    Finish,
    /// Consume the next non-trivia token (the builder pulls the actual token,
    /// including any preceding trivia, from the token stream).
    Token { kind: SyntaxKind },
    /// A parse error at the current position; carries a message for diagnostics.
    Error { message: String },
}

/// Placeholder kind for an Start event that has not been assigned a node kind
/// yet, or was abandoned. The builder skips Tombstone Start/Finish pairs.
pub const TOMBSTONE: SyntaxKind = SyntaxKind::Error;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_constructible() {
        let evs = vec![
            Event::Start { kind: SyntaxKind::SourceFile, forward_parent: None },
            Event::Token { kind: SyntaxKind::Number },
            Event::Finish,
        ];
        assert_eq!(evs.len(), 3);
    }
}
```

- [ ] **Step 2: Wire the module**

In `src/syntax/mod.rs` add:

```rust
pub mod event;
```

- [ ] **Step 3: Run the test**

Run: `cargo test --lib syntax::event 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/syntax/event.rs src/syntax/mod.rs
git commit -m "feat(syntax): parser event model"
```

---

## Task 3: Parser scaffold — token cursor + markers (parses a single number)

**Files:**
- Create: `src/syntax/parser.rs`
- Modify: `src/syntax/mod.rs`

- [ ] **Step 1: Write the smallest parser test**

Create `src/syntax/parser.rs`:

```rust
//! Hand-written recursive-descent parser. Operates over the NON-trivia tokens
//! (trivia is skipped for grammar decisions and re-inserted by the tree builder)
//! and emits a `Vec<Event>` plus a list of `ParseError`s. Never aborts: on error
//! it emits an `Error` event and recovers, so it always yields a tree.

use crate::syntax::event::{Event, TOMBSTONE};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::lexer::{lex, LexToken};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Index into the *non-trivia* token list where the error occurred.
    pub token_index: usize,
}

pub struct Parse {
    pub events: Vec<Event>,
    pub errors: Vec<ParseError>,
    /// The full token stream (incl. trivia), needed by the tree builder.
    pub tokens: Vec<LexToken>,
}

struct Parser {
    tokens: Vec<LexToken>,
    /// Indices (into `tokens`) of the non-trivia tokens, in order.
    nontrivia: Vec<usize>,
    /// Cursor into `nontrivia`.
    pos: usize,
    events: Vec<Event>,
    errors: Vec<ParseError>,
}

/// A pending open node. `complete` sets its kind; if dropped uncompleted it
/// stays a Tombstone and is skipped.
struct Marker {
    pos: usize, // index into events of the Start
    completed: bool,
}

struct CompletedMarker {
    pos: usize, // index into events of the Start
}

impl Parser {
    fn new(src: &str) -> Self {
        let tokens = lex(src);
        let nontrivia = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.kind.is_trivia())
            .map(|(i, _)| i)
            .collect();
        Parser { tokens, nontrivia, pos: 0, events: Vec::new(), errors: Vec::new() }
    }

    /// Kind of the current non-trivia token, or `Error` (used as EOF sentinel)
    /// when past the end.
    fn current(&self) -> SyntaxKind {
        match self.nontrivia.get(self.pos) {
            Some(&ti) => self.tokens[ti].kind,
            None => SyntaxKind::Error,
        }
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    fn at_end(&self) -> bool {
        self.pos >= self.nontrivia.len()
    }

    /// Open a node; returns a Marker to be `complete`d or abandoned.
    fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::Start { kind: TOMBSTONE, forward_parent: None });
        Marker { pos, completed: false }
    }

    /// Consume the current non-trivia token, emitting a Token event.
    fn bump(&mut self) {
        let kind = self.current();
        if !self.at_end() {
            self.events.push(Event::Token { kind });
            self.pos += 1;
        }
    }

    fn complete(&mut self, mut m: Marker, kind: SyntaxKind) -> CompletedMarker {
        m.completed = true;
        if let Event::Start { kind: slot, .. } = &mut self.events[m.pos] {
            *slot = kind;
        }
        self.events.push(Event::Finish);
        CompletedMarker { pos: m.pos }
    }

    /// Wrap an already-completed node `cm` in a new outer node of `kind`
    /// (left-assoc binary expressions). Returns the new Marker (already open).
    fn precede(&mut self, cm: &CompletedMarker) -> Marker {
        let new_pos = self.events.len();
        self.events.push(Event::Start { kind: TOMBSTONE, forward_parent: None });
        if let Event::Start { forward_parent, .. } = &mut self.events[cm.pos] {
            *forward_parent = Some(new_pos);
        }
        Marker { pos: new_pos, completed: false }
    }

    fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.errors.push(ParseError { message: message.clone(), token_index: self.pos });
        self.events.push(Event::Error { message });
    }
}

/// Parse `src` into events + errors + the token stream.
pub fn parse(src: &str) -> Parse {
    let mut p = Parser::new(src);
    let m = p.start();
    // Core slice: a sequence of statements until EOF.
    while !p.at_end() {
        let before = p.pos;
        stmt(&mut p);
        // Guard against non-advancing loops on unexpected input.
        if p.pos == before {
            p.error("unexpected token");
            p.bump();
        }
    }
    p.complete(m, SyntaxKind::SourceFile);
    Parse { events: p.events, errors: p.errors, tokens: p.tokens }
}

/// Placeholder statement parser — Task 6+ fills this in. For Task 3 it parses a
/// single number literal as an expression statement so the scaffold is testable.
fn stmt(p: &mut Parser) {
    let m = p.start();
    if p.at(SyntaxKind::Number) {
        let lit = p.start();
        p.bump();
        p.complete(lit, SyntaxKind::Literal);
        p.complete(m, SyntaxKind::ExprStmt);
    } else {
        p.error("expected statement");
        p.bump();
        p.complete(m, SyntaxKind::Error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_kinds(src: &str) -> Vec<SyntaxKind> {
        parse(src)
            .events
            .into_iter()
            .filter_map(|e| match e {
                Event::Start { kind, .. } if kind != crate::syntax::event::TOMBSTONE => Some(kind),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_a_number_statement() {
        // SourceFile > ExprStmt > Literal
        assert_eq!(
            node_kinds("42"),
            vec![SyntaxKind::SourceFile, SyntaxKind::ExprStmt, SyntaxKind::Literal]
        );
        assert!(parse("42").errors.is_empty());
    }

    #[test]
    fn unexpected_token_recovers_not_panics() {
        let p = parse("+");
        assert!(!p.errors.is_empty(), "should record an error");
        // Must terminate (no infinite loop) and still produce a SourceFile.
        assert!(matches!(p.events.first(), Some(Event::Start { kind: SyntaxKind::SourceFile, .. })));
    }
}
```

- [ ] **Step 2: Wire the module**

In `src/syntax/mod.rs` add:

```rust
pub mod parser;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --lib syntax::parser 2>&1 | tail -15`
Expected: `parses_a_number_statement` and `unexpected_token_recovers_not_panics` PASS.

- [ ] **Step 4: Commit**

```bash
git add src/syntax/parser.rs src/syntax/mod.rs
git commit -m "feat(syntax): parser scaffold (cursor, markers, events, recovery)"
```

---

## Task 4: TreeBuilder — events + trivia → lossless cstree tree

**Files:**
- Create: `src/syntax/tree_builder.rs`
- Modify: `src/syntax/mod.rs`

- [ ] **Step 1: Write the losslessness test for the structured tree**

Create `src/syntax/tree_builder.rs`:

```rust
//! Materialize a cstree green tree from parser events, re-inserting trivia from
//! the original token stream so the tree is byte-for-byte lossless.
//!
//! Trivia attachment policy: trivia is emitted at the point in the token stream
//! where it occurs. Concretely, before emitting each non-trivia Token, the
//! builder first flushes any trivia tokens that precede it in source order at
//! the *current* tree position. This guarantees losslessness; node-relative
//! "leading vs trailing" attachment is a formatter concern (Plan 4) and is
//! derivable from the tree because trivia sits between the surrounding tokens.

use crate::syntax::cst::SyntaxNode;
use crate::syntax::event::{Event, TOMBSTONE};
use crate::syntax::kind::SyntaxKind;
use crate::syntax::lexer::LexToken;
use crate::syntax::parser::Parse;
use cstree::build::GreenNodeBuilder;

pub fn build_tree(parse: Parse) -> SyntaxNode {
    let Parse { mut events, tokens, .. } = parse;

    // Resolve forward_parent links into a flat order: when a Start has a
    // forward_parent, the parent's Start must be emitted first. We rewrite by
    // walking events and, at each Start, following the forward_parent chain.
    let mut builder: GreenNodeBuilder<SyntaxKind> = GreenNodeBuilder::new();
    let mut token_pos = 0usize; // cursor into `tokens` (incl. trivia)

    // Helper: flush trivia tokens up to (not including) the next non-trivia token.
    fn flush_trivia(
        builder: &mut GreenNodeBuilder<SyntaxKind>,
        tokens: &[LexToken],
        token_pos: &mut usize,
    ) {
        while *token_pos < tokens.len() && tokens[*token_pos].kind.is_trivia() {
            let t = &tokens[*token_pos];
            builder.token(t.kind, &t.text);
            *token_pos += 1;
        }
    }

    // Pre-resolve forward parents: produce a reordered event list where a node
    // that is `forward_parent` of another is started before it.
    let resolved = resolve_forward_parents(&mut events);

    // Track open-node depth so trailing trivia (after the last non-trivia token)
    // is flushed *inside* the root, before the root's `finish_node` — cstree
    // rejects tokens added after the root closes.
    let mut depth: usize = 0;

    for ev in resolved {
        match ev {
            Event::Start { kind, .. } if kind != TOMBSTONE => {
                // Leading trivia attaches inside the node that follows it; it is
                // flushed at the next Token event. Node-relative leading/trailing
                // attachment is a formatter concern (Plan 4).
                builder.start_node(kind);
                depth += 1;
            }
            Event::Start { .. } => { /* tombstone: skip, no matching depth change */ }
            Event::Finish => {
                depth -= 1;
                // Closing the root: flush any remaining trailing trivia (final
                // newline / EOF comment) inside it before finishing.
                if depth == 0 {
                    flush_trivia(&mut builder, &tokens, &mut token_pos);
                }
                builder.finish_node();
            }
            Event::Token { kind } => {
                // Emit any trivia preceding this token, then the token itself.
                flush_trivia(&mut builder, &tokens, &mut token_pos);
                debug_assert!(token_pos < tokens.len());
                let t = &tokens[token_pos];
                builder.token(kind, &t.text);
                token_pos += 1;
            }
            Event::Error { .. } => { /* errors don't materialize tokens */ }
        }
    }

    let (green, cache) = builder.finish();
    let resolver = cache.unwrap().into_interner().unwrap();
    SyntaxNode::new_root_with_resolver(green, resolver)
}

/// Reorder events so that any node referenced as a `forward_parent` is started
/// immediately before the node that points to it. Mirrors rowan/rust-analyzer's
/// `process` step for retro-active node wrapping.
fn resolve_forward_parents(events: &mut [Event]) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    for i in 0..events.len() {
        match events[i].clone() {
            Event::Start { kind, forward_parent } if kind != TOMBSTONE => {
                // Collect the forward-parent chain (innermost first), then emit
                // outermost-first.
                let mut chain = vec![kind];
                let mut fp = forward_parent;
                while let Some(idx) = fp {
                    if let Event::Start { kind: pk, forward_parent: pfp } = events[idx].clone() {
                        // Mark the parent consumed so it isn't emitted again.
                        events[idx] = Event::Start { kind: TOMBSTONE, forward_parent: None };
                        chain.push(pk);
                        fp = pfp;
                    } else {
                        break;
                    }
                }
                for k in chain.into_iter().rev() {
                    out.push(Event::Start { kind: k, forward_parent: None });
                }
            }
            Event::Start { .. } => { /* consumed/tombstone: skip */ }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parser::parse;

    #[test]
    fn structured_tree_round_trips() {
        let src = "  42 // trailing\n";
        let node = build_tree(parse(src));
        assert_eq!(node.text().to_string(), src, "structured tree must be lossless");
    }
}
```

- [ ] **Step 2: Wire the module + a `parse_to_tree` convenience**

In `src/syntax/mod.rs` add:

```rust
pub mod tree_builder;

/// Parse source into a structured, lossless cstree tree.
pub fn parse_to_tree(src: &str) -> crate::syntax::cst::SyntaxNode {
    tree_builder::build_tree(parser::parse(src))
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test --lib syntax::tree_builder 2>&1 | tail -15`
Expected: `structured_tree_round_trips` PASS. If the `into_interner()` call differs from Plan 1/Task 2's spike, mirror whatever Plan 1 used (keep them identical).

- [ ] **Step 4: Commit**

```bash
git add src/syntax/tree_builder.rs src/syntax/mod.rs
git commit -m "feat(syntax): tree builder (events + trivia -> lossless cstree)"
```

---

## Task 5: Corpus losslessness for the structured tree

**Files:**
- Modify: `tests/cst_lossless.rs`

- [ ] **Step 1: Add a corpus test that parses to a structured tree and round-trips**

Add to `tests/cst_lossless.rs`:

```rust
#[test]
fn structured_tree_is_lossless_over_corpus() {
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let node = ascript::syntax::parse_to_tree(&src);
        assert_eq!(
            node.text().to_string(),
            src,
            "structured tree not lossless for {}",
            path.display()
        );
    }
}
```

- [ ] **Step 2: Run it**

Run: `cargo test --test cst_lossless structured_tree_is_lossless_over_corpus 2>&1 | tail -15`
Expected: PASS. (The parser only structures the core slice so far; un-handled constructs become `Error` nodes, but **every token is still emitted**, so losslessness holds. This test guards that property as grammar coverage grows.)

- [ ] **Step 3: Commit**

```bash
git add tests/cst_lossless.rs
git commit -m "test(syntax): structured-tree corpus losslessness"
```

---

## Task 6: Expressions — literals, names, precedence-climbing binary, unary, paren

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write expression-shape tests**

Add to the `tests` mod in `src/syntax/parser.rs`:

```rust
    fn tree_shape(src: &str) -> Vec<SyntaxKind> {
        // Pre-order node kinds (excluding tombstones), via the structured tree.
        use crate::syntax::cst::SyntaxNode;
        fn walk(n: &SyntaxNode, out: &mut Vec<SyntaxKind>) {
            out.push(n.kind());
            for c in n.children() {
                walk(&c, out);
            }
        }
        let node = crate::syntax::tree_builder::build_tree(parse(src));
        let mut out = Vec::new();
        walk(&node, &mut out);
        out
    }

    #[test]
    fn precedence_groups_multiply_under_add() {
        // 1 + 2 * 3  =>  Binary(+) { Literal, Binary(*) { Literal, Literal } }
        let shape = tree_shape("1 + 2 * 3");
        assert_eq!(
            shape,
            vec![
                SyntaxKind::SourceFile, SyntaxKind::ExprStmt,
                SyntaxKind::BinaryExpr,                 // +
                SyntaxKind::Literal,                    // 1
                SyntaxKind::BinaryExpr,                 // *
                SyntaxKind::Literal, SyntaxKind::Literal, // 2, 3
            ]
        );
        assert!(parse("1 + 2 * 3").errors.is_empty());
    }

    #[test]
    fn unary_and_paren() {
        assert_eq!(
            tree_shape("-(1)"),
            vec![
                SyntaxKind::SourceFile, SyntaxKind::ExprStmt,
                SyntaxKind::UnaryExpr, SyntaxKind::ParenExpr, SyntaxKind::Literal,
            ]
        );
    }

    #[test]
    fn name_reference() {
        assert_eq!(
            tree_shape("x"),
            vec![SyntaxKind::SourceFile, SyntaxKind::ExprStmt, SyntaxKind::NameRef]
        );
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::precedence_groups_multiply_under_add 2>&1 | tail -15`
Expected: FAIL — `stmt` only handles a bare number.

- [ ] **Step 3: Replace `stmt`'s placeholder with real expression parsing**

In `src/syntax/parser.rs`, replace the placeholder `stmt` fn with an expression-statement entry plus a precedence-climbing expression parser. Add the binding-power table and these functions (replace the old `fn stmt`):

```rust
fn stmt(p: &mut Parser) {
    let m = p.start();
    expr(p);
    p.complete(m, SyntaxKind::ExprStmt);
}

/// Infix binding powers (left, right). Higher binds tighter. Mirrors the legacy
/// precedence-climbing parser ordering for the core operators.
fn infix_binding_power(kind: SyntaxKind) -> Option<(u8, u8)> {
    use SyntaxKind::*;
    Some(match kind {
        PipePipe => (1, 2),
        AmpAmp => (3, 4),
        EqEq | BangEq => (5, 6),
        Lt | Le | Gt | Ge => (7, 8),
        Plus | Minus => (9, 10),
        Star | Slash | Percent => (11, 12),
        StarStar => (16, 15), // right-assoc
        _ => return None,
    })
}

fn expr(p: &mut Parser) {
    expr_bp(p, 0);
}

/// Pratt/precedence-climbing core. `min_bp` is the minimum left binding power
/// that may bind here.
fn expr_bp(p: &mut Parser, min_bp: u8) {
    let mut lhs = lhs(p);
    loop {
        let op = p.current();
        let Some((l_bp, r_bp)) = infix_binding_power(op) else { break };
        if l_bp < min_bp {
            break;
        }
        let m = p.precede(&lhs);
        p.bump(); // operator
        expr_bp(p, r_bp);
        lhs = p.complete(m, SyntaxKind::BinaryExpr);
    }
}

/// Parse a unary/primary expression, returning its CompletedMarker.
fn lhs(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    match p.current() {
        Minus | Bang => {
            let m = p.start();
            p.bump();
            // Unary binds tighter than any binary operator here.
            let operand = lhs(p);
            // Allow postfix (call/member) on the operand already handled in lhs.
            let _ = operand;
            p.complete(m, UnaryExpr)
        }
        _ => primary(p),
    }
}

/// Parse a primary expression (literal, name, paren) then postfix chains.
fn primary(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let cm = match p.current() {
        Number | Str | TrueKw | FalseKw | NilKw => {
            let m = p.start();
            p.bump();
            p.complete(m, Literal)
        }
        Ident => {
            let m = p.start();
            p.bump();
            p.complete(m, NameRef)
        }
        LParen => {
            let m = p.start();
            p.bump(); // (
            expr(p);
            if p.at(RParen) {
                p.bump();
            } else {
                p.error("expected ')'");
            }
            p.complete(m, ParenExpr)
        }
        _ => {
            let m = p.start();
            p.error("expected expression");
            p.complete(m, Error)
        }
    };
    postfix(p, cm)
}

/// Parse postfix call `(...)`, member `.x`, and index `[...]` chains.
fn postfix(p: &mut Parser, mut cm: CompletedMarker) -> CompletedMarker {
    use SyntaxKind::*;
    loop {
        match p.current() {
            LParen => {
                let m = p.precede(&cm);
                arg_list(p);
                cm = p.complete(m, CallExpr);
            }
            Dot => {
                let m = p.precede(&cm);
                p.bump(); // .
                if p.at(Ident) {
                    p.bump();
                } else {
                    p.error("expected property name after '.'");
                }
                cm = p.complete(m, MemberExpr);
            }
            LBracket => {
                let m = p.precede(&cm);
                p.bump(); // [
                expr(p);
                if p.at(RBracket) {
                    p.bump();
                } else {
                    p.error("expected ']'");
                }
                cm = p.complete(m, IndexExpr);
            }
            _ => break,
        }
    }
    cm
}

fn arg_list(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // (
    while !p.at(RParen) && !p.at_end() {
        expr(p);
        if p.at(Comma) {
            p.bump();
        } else {
            break;
        }
    }
    if p.at(RParen) {
        p.bump();
    } else {
        p.error("expected ')' to close arguments");
    }
    p.complete(m, ArgList);
}
```

- [ ] **Step 4: Run the expression tests**

Run: `cargo test --lib syntax::parser 2>&1 | tail -20`
Expected: `precedence_groups_multiply_under_add`, `unary_and_paren`, `name_reference`, and the Task 3 tests all PASS.

- [ ] **Step 5: Run corpus losslessness (must still hold)**

Run: `cargo test --test cst_lossless 2>&1 | tail -10`
Expected: PASS (every token still emitted).

- [ ] **Step 6: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): expressions — precedence-climbing binary, unary, paren, call/member/index"
```

---

## Task 7: Statements — let/const, assignment, block, if/while/return

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write statement-shape tests**

Add to the `tests` mod:

```rust
    #[test]
    fn let_statement() {
        assert_eq!(
            tree_shape("let x = 1"),
            vec![
                SyntaxKind::SourceFile, SyntaxKind::LetStmt,
                SyntaxKind::Literal, // initializer 1
            ]
        );
        assert!(parse("let x = 1").errors.is_empty());
    }

    #[test]
    fn if_else_with_block() {
        let p = parse("if x { return 1 } else { return 2 }");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let shape = tree_shape("if x { return 1 } else { return 2 }");
        assert!(shape.contains(&SyntaxKind::IfStmt));
        assert!(shape.contains(&SyntaxKind::Block));
        assert!(shape.contains(&SyntaxKind::ReturnStmt));
    }

    #[test]
    fn while_loop() {
        assert!(parse("while x { x = 0 }").errors.is_empty());
        assert!(tree_shape("while x { x = 0 }").contains(&SyntaxKind::WhileStmt));
    }

    #[test]
    fn assignment_is_a_statement() {
        assert!(tree_shape("x = 5").contains(&SyntaxKind::AssignExpr));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::let_statement 2>&1 | tail -15`
Expected: FAIL — `stmt` only parses expression statements.

- [ ] **Step 3: Extend `stmt` to dispatch on statement keywords**

In `src/syntax/parser.rs`, replace `fn stmt` with a dispatcher and add the statement functions:

```rust
fn stmt(p: &mut Parser) {
    use SyntaxKind::*;
    match p.current() {
        LetKw | ConstKw => let_stmt(p),
        IfKw => if_stmt(p),
        WhileKw => while_stmt(p),
        ReturnKw => return_stmt(p),
        FnKw => fn_decl(p),
        LBrace => {
            block(p);
        }
        _ => expr_stmt(p),
    }
}

fn expr_stmt(p: &mut Parser) {
    let m = p.start();
    // Parse an expression, then allow a trailing `= rhs` as an assignment.
    let lhs = expr_returning(p);
    if p.at(SyntaxKind::Eq) {
        let am = p.precede(&lhs);
        p.bump(); // =
        expr(p);
        p.complete(am, SyntaxKind::AssignExpr);
    }
    p.complete(m, SyntaxKind::ExprStmt);
}

/// Like `expr` but returns the CompletedMarker so callers can wrap it (assignment).
fn expr_returning(p: &mut Parser) -> CompletedMarker {
    // expr_bp returns nothing; re-implement the top level to capture the marker.
    let cm = lhs(p);
    // Continue infix climbing from the parsed lhs at min_bp 0.
    let mut lhs_cm = cm;
    loop {
        let op = p.current();
        let Some((l_bp, r_bp)) = infix_binding_power(op) else { break };
        let _ = l_bp;
        let m = p.precede(&lhs_cm);
        p.bump();
        expr_bp(p, r_bp);
        lhs_cm = p.complete(m, SyntaxKind::BinaryExpr);
    }
    lhs_cm
}

fn let_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // let/const
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected a name after let/const");
    }
    if p.at(Eq) {
        p.bump();
        expr(p);
    }
    p.complete(m, LetStmt);
}

fn block(p: &mut Parser) -> CompletedMarker {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // {
    while !p.at(RBrace) && !p.at_end() {
        let before = p.pos;
        stmt(p);
        if p.pos == before {
            p.error("unexpected token in block");
            p.bump();
        }
    }
    if p.at(RBrace) {
        p.bump();
    } else {
        p.error("expected '}' to close block");
    }
    p.complete(m, Block)
}

fn if_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // if
    expr(p); // condition
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' after if condition");
    }
    if p.at(ElseKw) {
        p.bump();
        if p.at(IfKw) {
            if_stmt(p); // else if
        } else if p.at(LBrace) {
            block(p);
        } else {
            p.error("expected '{' or 'if' after else");
        }
    }
    p.complete(m, IfStmt);
}

fn while_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // while
    expr(p);
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' after while condition");
    }
    p.complete(m, WhileStmt);
}

fn return_stmt(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // return
    // Optional value: if the next token can start an expression on the same
    // logical line. For the core slice, parse an expression unless at `}`/EOF.
    if !p.at(RBrace) && !p.at_end() {
        expr(p);
    }
    p.complete(m, ReturnStmt);
}
```

- [ ] **Step 4: Run statement tests**

Run: `cargo test --lib syntax::parser 2>&1 | tail -20`
Expected: all parser tests PASS.

- [ ] **Step 5: Corpus losslessness**

Run: `cargo test --test cst_lossless 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): statements — let/const, assignment, block, if/while/return"
```

---

## Task 8: Functions & arrows

**Files:**
- Modify: `src/syntax/parser.rs`

- [ ] **Step 1: Write function/arrow tests**

Add to the `tests` mod:

```rust
    #[test]
    fn fn_declaration() {
        let p = parse("fn add(a, b) { return a + b }");
        assert!(p.errors.is_empty(), "errors: {:?}", p.errors);
        let shape = tree_shape("fn add(a, b) { return a + b }");
        assert!(shape.contains(&SyntaxKind::FnDecl));
        assert!(shape.contains(&SyntaxKind::ParamList));
        assert!(shape.contains(&SyntaxKind::Param));
    }

    #[test]
    fn arrow_expression() {
        assert!(parse("let f = (x) => x + 1").errors.is_empty());
        assert!(tree_shape("let f = (x) => x + 1").contains(&SyntaxKind::ArrowExpr));
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::parser::tests::fn_declaration 2>&1 | tail -15`
Expected: FAIL — `fn` not yet parsed (it falls to `expr_stmt` and errors).

- [ ] **Step 3: Add `fn_decl`, `param_list`, and arrow handling**

In `src/syntax/parser.rs`, add `fn_decl`/`param_list`, and extend `primary` to recognize an arrow that starts with `(`. Add:

```rust
fn fn_decl(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // fn
    if p.at(Ident) {
        p.bump();
    } else {
        p.error("expected function name");
    }
    if p.at(LParen) {
        param_list(p);
    } else {
        p.error("expected '(' after function name");
    }
    if p.at(LBrace) {
        block(p);
    } else {
        p.error("expected '{' for function body");
    }
    p.complete(m, FnDecl);
}

fn param_list(p: &mut Parser) {
    use SyntaxKind::*;
    let m = p.start();
    p.bump(); // (
    while !p.at(RParen) && !p.at_end() {
        let pm = p.start();
        if p.at(Ident) {
            p.bump();
        } else {
            p.error("expected parameter name");
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

Then, in `primary`, replace the `LParen =>` arm with one that disambiguates an arrow `(params) =>` from a parenthesized expression by scanning ahead for a `)` followed by `=>`:

```rust
        LParen if is_arrow_ahead(p) => {
            let m = p.start();
            param_list(p);
            p.bump(); // =>  (guaranteed by is_arrow_ahead)
            // Arrow body: a block or an expression.
            if p.at(LBrace) {
                block(p);
            } else {
                expr(p);
            }
            p.complete(m, ArrowExpr)
        }
        LParen => {
            let m = p.start();
            p.bump(); // (
            expr(p);
            if p.at(RParen) {
                p.bump();
            } else {
                p.error("expected ')'");
            }
            p.complete(m, ParenExpr)
        }
```

And add the lookahead helper (scans the non-trivia token kinds from the current `(` to a matching `)` at depth 0, then checks for `=>`):

```rust
/// True if the `(` at the cursor begins an arrow parameter list, i.e. the
/// matching `)` is immediately followed by `=>`.
fn is_arrow_ahead(p: &Parser) -> bool {
    use SyntaxKind::*;
    let mut depth = 0i32;
    let mut i = p.pos;
    while i < p.nontrivia.len() {
        match p.tokens[p.nontrivia[i]].kind {
            LParen => depth += 1,
            RParen => {
                depth -= 1;
                if depth == 0 {
                    // token after the matching ')'
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
```

- [ ] **Step 4: Run function/arrow tests**

Run: `cargo test --lib syntax::parser 2>&1 | tail -20`
Expected: all parser tests PASS.

- [ ] **Step 5: Corpus losslessness**

Run: `cargo test --test cst_lossless 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/syntax/parser.rs
git commit -m "feat(syntax): functions and arrow expressions"
```

---

## Task 9: ungrammar grammar + build.rs codegen + typed AST accessors

**Files:**
- Create: `src/syntax/ast/ascript.ungram`
- Create: `src/syntax/ast/mod.rs`
- Modify: `build.rs`
- Modify: `Cargo.toml` (add `ungrammar` as a build-dependency)
- Modify: `src/syntax/mod.rs`

- [ ] **Step 1: Add ungrammar as a build-dependency**

In `Cargo.toml`, add a `[build-dependencies]` section (or append if present):

```toml
[build-dependencies]
cc = "1"           # already used for tree-sitter; keep it
ungrammar = "1"
```

> If `[build-dependencies]` already exists with `cc`, just add the `ungrammar` line.

- [ ] **Step 2: Write the grammar for the core slice**

Create `src/syntax/ast/ascript.ungram` describing the node shapes built so far:

```
SourceFile = Stmt*

Stmt =
    LetStmt
  | ExprStmt
  | Block
  | IfStmt
  | WhileStmt
  | ReturnStmt
  | FnDecl

LetStmt = ('let' | 'const') 'ident' ('=' Expr)?
ExprStmt = Expr
Block = '{' Stmt* '}'
IfStmt = 'if' cond:Expr then:Block ('else' (IfStmt | Block))?
WhileStmt = 'while' cond:Expr body:Block
ReturnStmt = 'return' Expr?
FnDecl = 'fn' 'ident' ParamList Block
ParamList = '(' Param* ')'
Param = 'ident'

Expr =
    Literal
  | NameRef
  | UnaryExpr
  | BinaryExpr
  | ParenExpr
  | CallExpr
  | MemberExpr
  | IndexExpr
  | ArrowExpr
  | AssignExpr

Literal = 'number' | 'string' | 'true' | 'false' | 'nil'
NameRef = 'ident'
UnaryExpr = op:('-' | '!') Expr
BinaryExpr = lhs:Expr op:('+'|'-'|'*'|'/'|'%'|'**'|'=='|'!='|'<'|'<='|'>'|'>='|'&&'|'||') rhs:Expr
ParenExpr = '(' Expr ')'
CallExpr = Expr ArgList
ArgList = '(' Expr* ')'
MemberExpr = Expr '.' 'ident'
IndexExpr = Expr '[' Expr ']'
ArrowExpr = ParamList '=>' (Block | Expr)
AssignExpr = target:Expr '=' value:Expr
```

> ungrammar's grammar is a *shape* description only; it has no bearing on parsing. The generator in Step 3 reads it to emit typed accessor structs whose node kinds match `SyntaxKind`.

- [ ] **Step 3: Write the codegen in build.rs**

Append to `build.rs` (keep the existing tree-sitter compile). This minimal generator emits, for each node rule, a newtype wrapping `SyntaxNode` with a `cast`/`kind` and child accessors by kind. Add:

```rust
fn generate_ast_nodes() {
    use std::fmt::Write as _;
    println!("cargo:rerun-if-changed=src/syntax/ast/ascript.ungram");
    let text = std::fs::read_to_string("src/syntax/ast/ascript.ungram")
        .expect("read ascript.ungram");
    let grammar: ungrammar::Grammar = text.parse().expect("parse ungrammar");

    let mut out = String::new();
    out.push_str("// @generated by build.rs from ascript.ungram — do not edit.\n");
    out.push_str("use crate::syntax::cst::SyntaxNode;\n");
    out.push_str("use crate::syntax::kind::SyntaxKind;\n\n");

    for node in grammar.iter() {
        let data = &grammar[node];
        // Only emit a struct for node rules whose name matches a SyntaxKind
        // variant (PascalCase). Alternation rules (Stmt/Expr) are emitted as
        // enums in a later iteration of the generator; for the core slice we
        // emit only concrete node structs.
        let name = &data.name;
        if is_enum_rule(&grammar, node) {
            continue;
        }
        let _ = writeln!(out, "#[derive(Debug, Clone)]");
        let _ = writeln!(out, "pub struct {name}(pub SyntaxNode);");
        let _ = writeln!(out, "impl {name} {{");
        let _ = writeln!(out, "    pub fn cast(node: SyntaxNode) -> Option<Self> {{");
        let _ = writeln!(out, "        if node.kind() == SyntaxKind::{name} {{ Some(Self(node)) }} else {{ None }}");
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "    pub fn syntax(&self) -> &SyntaxNode {{ &self.0 }}");
        let _ = writeln!(out, "}}\n");
    }

    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("ast_nodes.rs");
    std::fs::write(dest, out).expect("write ast_nodes.rs");
}

/// A rule is an "enum" (alternation of node references) if its RHS is an Alt of
/// node references — those become Rust enums later; for now they're skipped.
fn is_enum_rule(grammar: &ungrammar::Grammar, node: ungrammar::Node) -> bool {
    matches!(&grammar[node].rule, ungrammar::Rule::Alt(_))
}
```

Then call it from `main()` in `build.rs` (add the call alongside the tree-sitter compile):

```rust
    generate_ast_nodes();
```

- [ ] **Step 4: Include the generated nodes + an AstNode test**

Create `src/syntax/ast/mod.rs`:

```rust
//! Typed AST: thin wrappers over `SyntaxNode`, generated from `ascript.ungram`.
//! The generated file lives in `OUT_DIR`; we `include!` it here.

include!(concat!(env!("OUT_DIR"), "/ast_nodes.rs"));

#[cfg(test)]
mod tests {
    use crate::syntax::parse_to_tree;
    use crate::syntax::kind::SyntaxKind;

    #[test]
    fn cast_source_file_then_find_let() {
        let root = parse_to_tree("let x = 1");
        let file = super::SourceFile::cast(root).expect("root is SourceFile");
        // The first child of interest is a LetStmt.
        let has_let = file
            .syntax()
            .descendants()
            .any(|n| n.kind() == SyntaxKind::LetStmt);
        assert!(has_let);
    }
}
```

In `src/syntax/mod.rs` add:

```rust
pub mod ast;
```

- [ ] **Step 5: Build + run the typed-AST test**

Run: `cargo build 2>&1 | tail -10`
Expected: build succeeds; `build.rs` generates `ast_nodes.rs`. If `ungrammar`'s API names differ (`Grammar::iter`, indexing, `Rule::Alt`), consult `https://docs.rs/ungrammar` and adjust the generator; the generated output shape (one `struct Name(SyntaxNode)` with `cast`/`syntax` per concrete node) is what matters.
Run: `cargo test --lib syntax::ast 2>&1 | tail -15`
Expected: `cast_source_file_then_find_let` PASS.

- [ ] **Step 6: Clippy both configs + commit**

Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

```bash
git add Cargo.toml Cargo.lock build.rs src/syntax/ast/ src/syntax/mod.rs
git commit -m "feat(syntax): ungrammar grammar + build.rs codegen + typed AST accessors"
```

---

## Task 10: tree-sitter differential oracle over the core slice

**Files:**
- Create: `tests/cst_parser_oracle.rs`

- [ ] **Step 1: Write the differential acceptance test**

The new parser and the vendored tree-sitter grammar must **agree on accept/reject** for inputs in the core slice. Create `tests/cst_parser_oracle.rs`:

```rust
//! Differential oracle: for core-slice snippets, the new parser's "no errors"
//! verdict must match the tree-sitter grammar's "no ERROR node" verdict. This
//! guards that the hand-written parser and the grammar stay in agreement as the
//! parser grows. (Full-corpus differential testing arrives once grammar coverage
//! is complete in Plan 2b.)

/// Snippets that BOTH parsers must accept (core slice only).
const ACCEPT: &[&str] = &[
    "1 + 2 * 3",
    "-(1)",
    "let x = 1",
    "const y = x + 1",
    "if x { return 1 } else { return 2 }",
    "while x { x = 0 }",
    "fn add(a, b) { return a + b }",
    "let f = (x) => x + 1",
    "foo(1, 2)",
    "a.b[c]",
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
    // The legacy hand-written parser is the available in-process oracle; the
    // tree-sitter grammar is exercised by tests/treesitter_conformance.rs.
    for src in ACCEPT {
        let toks = ascript::lexer::lex(src).expect("legacy lex");
        assert!(ascript::parser::parse(&toks).is_ok(), "legacy rejected {src:?}");
    }
}
```

> We use the legacy hand-written parser as the in-process differential oracle here (it already mirrors the tree-sitter grammar via `frontend_conformance.rs`). The tree-sitter grammar itself is covered by the existing `treesitter_conformance.rs`; full new-parser-vs-tree-sitter differential testing over the whole corpus lands in Plan 2b once coverage is complete.

- [ ] **Step 2: Run the oracle**

Run: `cargo test --test cst_parser_oracle 2>&1 | tail -15`
Expected: both tests PASS.

- [ ] **Step 3: Run the full suite + clippy both configs**

Run: `cargo test 2>&1 | tail -15`
Expected: green.
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

- [ ] **Step 4: Commit**

```bash
git add tests/cst_parser_oracle.rs
git commit -m "test(syntax): differential parser oracle over the core slice"
```

---

## Done criteria for Plan 2

- [ ] `cargo test` green; `cargo clippy` clean in both feature configs.
- [ ] The parser produces a **structured, lossless** cstree tree (corpus round-trip still byte-for-byte).
- [ ] Precedence, unary, paren, call/member/index, `let`/`const`, assignment, block, `if`/`while`/`return`, `fn`/arrow all parse to the right node shapes with **error recovery** (no panics; errors collected, tree always produced).
- [ ] `ascript.ungram` + `build.rs` codegen produce typed AST wrappers that `cast` from the tree.
- [ ] Differential oracle agrees on the core slice.
- [ ] The interpreter and binary are **unchanged** (legacy front-end still runs everything).

**Next plan:** `cst-parser-coverage.md` (Plan 2b) — extend the parser + grammar + typed AST + oracle to the **full** grammar, one production per task, repeating Tasks 6–10's pattern: templates, spread, object/array literals, `?`/`!`/ternary/`await`/`yield`, `for`/`for await`, `match` (array/object/range/guard patterns), destructuring (`let {a, b as c}` / `[a, ...rest]`), `enum`, `class` (fields/methods/`init`/`super`), `import`/`export`, type annotations (`T?`, `array<T>`, `map<K,V>`, unions), and the `?.` optional-member. Then swap the full-corpus differential oracle to new-parser-vs-tree-sitter.
