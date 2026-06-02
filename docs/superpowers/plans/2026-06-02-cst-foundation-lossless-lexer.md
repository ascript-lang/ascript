# CST Foundation — Lossless Lexer & cstree Scaffolding (Plan 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the lossless front-end foundation — a `cstree`-backed concrete syntax tree and a trivia-emitting lexer that reproduces source byte-for-byte — without touching the parser, interpreter, or formatter.

**Architecture:** A new `src/syntax/` module introduces a flat `SyntaxKind` enum (all token kinds + trivia + a `ROOT` node), a lexer that emits *every* lexeme including comments/whitespace/newlines as text-carrying tokens, and a builder that assembles those tokens into a `cstree` green tree. A byte-for-byte round-trip test over the whole `examples/` corpus proves nothing is dropped. The existing front-end (`lexer.rs`/`token.rs`/`parser.rs`/`ast.rs`) is untouched and continues to run the binary — the new code coexists.

**Tech Stack:** Rust, `cstree` 0.14 (green/red trees + string interning + `#[derive(Syntax)]`), existing `ariadne`/`AsError` error types.

**Scope note:** This is Plan 1 of the CST migration (spec: `docs/superpowers/specs/2026-06-02-cst-frontend-migration-design.md`). It delivers Step A's *lexer + lossless* portion only. Follow-on plans: (2) event parser + ungrammar codegen, (3) CST-native interp vertical slice + 5% perf benchmark, (4) coverage growth to full parity, (5) comment-preserving formatter, (6) name resolver + binding cache.

**Key invariant established here:** *losslessness* — concatenating every token's text in lex order equals the source exactly. Every later layer depends on this property; it is the single most important thing this plan proves.

---

## File Structure

- Create `src/syntax/mod.rs` — module root; re-exports `SyntaxKind`, `lex`, `build_flat_tree`, `AscriptLang`/`SyntaxNode` alias.
- Create `src/syntax/kind.rs` — the `SyntaxKind` enum (`#[derive(cstree::Syntax)]`) + helpers (`is_trivia`).
- Create `src/syntax/lexer.rs` — the trivia-emitting lexer: `LexToken`, `lex(&str) -> Vec<LexToken>`.
- Create `src/syntax/cst.rs` — `cstree` glue: the `SyntaxNode`/`SyntaxToken` type aliases and `build_flat_tree`.
- Create `src/syntax/tests.rs` — round-trip + corpus tests (or `#[cfg(test)] mod tests` inline; this plan uses inline `#[cfg(test)]` blocks per file, matching repo convention).
- Modify `Cargo.toml` — add `cstree` dependency.
- Modify `src/lib.rs:1-17` — add `pub mod syntax;`.

**Convention reuse:** spans in the existing lexer are *char* offsets (`src/span.rs`). `cstree` is text/byte based, so the new lexer carries **owned token text** and lets `cstree` handle offsets/interning. We do not produce `Span`s in this plan. The new lexer is self-contained — it does *not* reuse the legacy scanners, because losslessness needs only exact text slices (boundary scanning), not decoded escape values (those arrive in a later plan when the parser builds literal values).

---

## Task 1: Add the cstree dependency

**Files:**
- Modify: `Cargo.toml:14-15` (the `[dependencies]` block start)

- [ ] **Step 1: Add the dependency**

Add this line to the `[dependencies]` section of `Cargo.toml` (right after the `indexmap` line at `Cargo.toml:18`):

```toml
cstree = { version = "0.14", features = ["derive"] }
```

- [ ] **Step 2: Verify it resolves and builds**

Run: `cargo build 2>&1 | tail -5`
Expected: builds successfully (downloads `cstree` + `cstree_derive`), no errors.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add cstree 0.14 (derive) dependency"
```

---

## Task 2: Spike — SyntaxKind + cstree glue that builds and round-trips one token

This task locks the exact `cstree` 0.14 derive/builder API by compiling the smallest possible tree. If any derive attribute or signature differs from what's written here, this is where it surfaces (compile error), before any real lexer code depends on it.

**Files:**
- Create: `src/syntax/kind.rs`
- Create: `src/syntax/cst.rs`
- Create: `src/syntax/mod.rs`
- Modify: `src/lib.rs:1`

- [ ] **Step 1: Create a minimal `SyntaxKind` enum**

Create `src/syntax/kind.rs`:

```rust
//! The flat set of syntax kinds: every token kind, every trivia kind, and the
//! node kinds. This single enum is the contract between the lexer, the tree
//! builder, cstree, and (later) the generated typed-AST layer.

/// `cstree`'s derive requires a fieldless `#[repr(u32)]` enum. Variants with a
/// fixed spelling get `#[static_text("…")]` so cstree can intern them once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
#[derive(cstree::Syntax)]
pub enum SyntaxKind {
    // --- nodes ---
    /// The whole-document root node.
    Root,

    // --- trivia (text varies → no static_text) ---
    Whitespace,
    Newline,
    LineComment,
    BlockComment,

    // --- a single real token, just for the spike ---
    /// Numeric literal (text varies).
    Number,
}

impl SyntaxKind {
    /// Trivia = tokens that carry no semantic meaning (whitespace + comments).
    /// The parser will attach these to nodes rather than treating them as
    /// structural tokens.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::Whitespace
                | SyntaxKind::Newline
                | SyntaxKind::LineComment
                | SyntaxKind::BlockComment
        )
    }
}
```

- [ ] **Step 2: Create the cstree glue + a spike test**

Create `src/syntax/cst.rs`:

```rust
//! cstree integration: type aliases for our concrete syntax tree and helpers to
//! build it. `SyntaxKind` is used directly as cstree's syntax type (no separate
//! Language marker is needed in cstree 0.14).

use crate::syntax::kind::SyntaxKind;

/// Red-tree node handle, parameterized by our syntax kind. `()` is the custom
/// per-node data slot (unused for now; reserved for the future resolution cache).
pub type SyntaxNode = cstree::syntax::SyntaxNode<SyntaxKind>;
pub type SyntaxToken = cstree::syntax::SyntaxToken<SyntaxKind>;

#[cfg(test)]
mod tests {
    use super::*;
    use cstree::build::GreenNodeBuilder;

    #[test]
    fn builds_and_reads_back_one_token() {
        let mut builder: GreenNodeBuilder<SyntaxKind> = GreenNodeBuilder::new();
        builder.start_node(SyntaxKind::Root);
        builder.token(SyntaxKind::Number, "42");
        builder.finish_node();
        let (green, cache) = builder.finish();

        // cstree interns token text; to read text back we need the interner the
        // builder produced. `new_root_with_resolver` attaches it for traversal.
        let resolver = cache.unwrap().into_interner().unwrap();
        let root = SyntaxNode::new_root_with_resolver(green, resolver);

        // The root's text must reproduce exactly what we fed in.
        assert_eq!(root.text().to_string(), "42");
    }
}
```

Create `src/syntax/mod.rs`:

```rust
//! Lossless concrete-syntax-tree front-end (cstree-backed).
//!
//! This module is being built in parallel with the legacy `lexer`/`token`/
//! `parser`/`ast` front-end and does not yet drive the binary. See
//! `docs/superpowers/specs/2026-06-02-cst-frontend-migration-design.md`.

pub mod cst;
pub mod kind;

pub use kind::SyntaxKind;
```

- [ ] **Step 3: Register the module**

In `src/lib.rs`, add after line 1 (`pub mod ast;`):

```rust
pub mod syntax;
```

- [ ] **Step 4: Run the spike test**

Run: `cargo test --lib syntax::cst::tests::builds_and_reads_back_one_token -- --nocapture 2>&1 | tail -20`
Expected: PASS. If it fails to **compile**, the cstree 0.14 API differs from the spike — consult `https://docs.rs/cstree/0.14.0/`. The two most likely deltas and their fixes:
- `into_interner()`/`into_resolver()` naming: try `cache.unwrap().into_interner()` vs `.into_resolver()`; the test asserts you can recover a resolver to read text.
- `SyntaxNode::new_root_with_resolver` may instead be `new_root` if the resolver is embedded. Adjust the two lines in the test accordingly.
Do not proceed until this test passes — it is the contract every later task assumes.

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy --all-targets 2>&1 | tail -5`
Expected: no warnings in `src/syntax/`.

```bash
git add src/syntax/ src/lib.rs
git commit -m "feat(syntax): cstree glue + SyntaxKind spike (builds + round-trips one token)"
```

---

## Task 3: Define the complete SyntaxKind enum

Replace the spike enum with the full token set, mapped 1:1 from the existing `Tok` enum (`src/token.rs:6-77`) plus trivia. Node kinds beyond `Root` are added in the parser plan, not here.

**Files:**
- Modify: `src/syntax/kind.rs`

- [ ] **Step 1: Write a test that asserts trivia classification**

Add to a `#[cfg(test)] mod tests` block at the bottom of `src/syntax/kind.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivia_classification() {
        assert!(SyntaxKind::Whitespace.is_trivia());
        assert!(SyntaxKind::LineComment.is_trivia());
        assert!(SyntaxKind::BlockComment.is_trivia());
        assert!(SyntaxKind::Newline.is_trivia());
        assert!(!SyntaxKind::Number.is_trivia());
        assert!(!SyntaxKind::Plus.is_trivia());
        assert!(!SyntaxKind::LetKw.is_trivia());
    }
}
```

- [ ] **Step 2: Run it (expect compile failure)**

Run: `cargo test --lib syntax::kind 2>&1 | tail -10`
Expected: FAIL to compile — `SyntaxKind::Plus` / `LetKw` do not exist yet.

- [ ] **Step 3: Expand the enum to the full token set**

Replace the enum body in `src/syntax/kind.rs` with the complete set. Use `#[static_text(...)]` for every fixed-spelling token; omit it for variable-text tokens (literals, identifiers, templates, trivia):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
#[derive(cstree::Syntax)]
pub enum SyntaxKind {
    // --- nodes (only Root for now; parser plan adds the rest) ---
    Root,

    // --- trivia ---
    Whitespace,
    Newline,
    LineComment,
    BlockComment,

    // --- literals / identifiers (variable text) ---
    Number,
    Str,
    Ident,
    TemplateStr,
    TemplateStart,
    TemplateMiddle,
    TemplateEnd,

    // --- operators & punctuation (fixed text) ---
    #[static_text("+")] Plus,
    #[static_text("-")] Minus,
    #[static_text("*")] Star,
    #[static_text("/")] Slash,
    #[static_text("%")] Percent,
    #[static_text("**")] StarStar,
    #[static_text("(")] LParen,
    #[static_text(")")] RParen,
    #[static_text("{")] LBrace,
    #[static_text("}")] RBrace,
    #[static_text("[")] LBracket,
    #[static_text("]")] RBracket,
    #[static_text(",")] Comma,
    #[static_text(".")] Dot,
    #[static_text(":")] Colon,
    #[static_text(";")] Semicolon,
    #[static_text("!")] Bang,
    #[static_text("!=")] BangEq,
    #[static_text("==")] EqEq,
    #[static_text("=")] Eq,
    #[static_text("<")] Lt,
    #[static_text("<=")] Le,
    #[static_text(">")] Gt,
    #[static_text(">=")] Ge,
    #[static_text("&&")] AmpAmp,
    #[static_text("||")] PipePipe,
    #[static_text("??")] QuestionQuestion,
    #[static_text("?")] Question,
    #[static_text("?.")] QuestionDot,
    #[static_text("|")] Pipe,
    #[static_text("+=")] PlusEq,
    #[static_text("-=")] MinusEq,
    #[static_text("*=")] StarEq,
    #[static_text("/=")] SlashEq,
    #[static_text("..")] DotDot,
    #[static_text("..=")] DotDotEq,
    #[static_text("...")] DotDotDot,
    #[static_text("=>")] FatArrow,

    // --- keywords (fixed text) ---
    #[static_text("true")] TrueKw,
    #[static_text("false")] FalseKw,
    #[static_text("nil")] NilKw,
    #[static_text("let")] LetKw,
    #[static_text("const")] ConstKw,
    #[static_text("if")] IfKw,
    #[static_text("else")] ElseKw,
    #[static_text("while")] WhileKw,
    #[static_text("for")] ForKw,
    #[static_text("in")] InKw,
    #[static_text("of")] OfKw,
    #[static_text("return")] ReturnKw,
    #[static_text("break")] BreakKw,
    #[static_text("continue")] ContinueKw,
    #[static_text("fn")] FnKw,
    #[static_text("enum")] EnumKw,
    #[static_text("match")] MatchKw,
    #[static_text("class")] ClassKw,
    #[static_text("import")] ImportKw,
    #[static_text("export")] ExportKw,
    #[static_text("async")] AsyncKw,
    #[static_text("await")] AwaitKw,
    #[static_text("yield")] YieldKw,

    // --- sentinel for unrecognized input (parser plan uses Error nodes too) ---
    Error,
}
```

Keep the existing `impl SyntaxKind { pub fn is_trivia(self) -> bool { … } }` from Task 2 above the tests.

> Note: the soft keyword `as` (object-destructuring rename) is lexed as `Ident` and recognized contextually by the parser (matching today's behavior); it is intentionally **not** a `SyntaxKind` keyword variant.

- [ ] **Step 4: Run the test**

Run: `cargo test --lib syntax::kind 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy --all-targets 2>&1 | tail -5`
Expected: clean.

```bash
git add src/syntax/kind.rs
git commit -m "feat(syntax): complete SyntaxKind enum mapped from Tok + trivia"
```

---

## Task 4: Trivia-emitting lexer — whitespace & comments first (the losslessness core)

**Files:**
- Create: `src/syntax/lexer.rs`
- Modify: `src/syntax/mod.rs`

- [ ] **Step 1: Write the losslessness test for trivia**

Create `src/syntax/lexer.rs` with the type, a stub `lex`, and the test:

```rust
//! Trivia-emitting lexer for the lossless CST front-end. Unlike the legacy
//! lexer (which discards whitespace and comments), this one emits EVERY lexeme
//! as a text-carrying token. Concatenating all token texts reproduces the
//! source exactly — the losslessness invariant.

use crate::syntax::kind::SyntaxKind;

/// One lexeme: its kind plus the exact source text it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexToken {
    pub kind: SyntaxKind,
    pub text: String,
}

/// Reconstruct source from a token stream — used by the losslessness invariant.
pub fn render(tokens: &[LexToken]) -> String {
    tokens.iter().map(|t| t.text.as_str()).collect()
}

pub fn lex(_src: &str) -> Vec<LexToken> {
    todo!("implemented incrementally across Tasks 4-7")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<SyntaxKind> {
        lex(src).into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lossless_trivia_only() {
        let src = "  \n\t// a line comment\n/* block\n comment */  \n";
        assert_eq!(render(&lex(src)), src, "lexer must be lossless");
    }

    #[test]
    fn classifies_trivia_kinds() {
        use SyntaxKind::*;
        // "  " ws, "\n" newline, "// c" line comment, "\n" newline
        assert_eq!(kinds("  \n// c\n"), vec![Whitespace, Newline, LineComment, Newline]);
    }

    #[test]
    fn unterminated_block_comment_is_lossless() {
        // Recovery: an unterminated block comment is still emitted in full so
        // the stream stays lossless; the parser plan reports the error.
        let src = "/* never closed";
        assert_eq!(render(&lex(src)), src);
    }
}
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::lexer 2>&1 | tail -15`
Expected: FAIL — `lex` panics with `todo!`.

- [ ] **Step 3: Implement the lexer skeleton + trivia scanning**

Replace the `lex` stub in `src/syntax/lexer.rs` with:

```rust
pub fn lex(src: &str) -> Vec<LexToken> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut out: Vec<LexToken> = Vec::new();

    // Helper: push a token covering chars[start..end].
    macro_rules! push {
        ($kind:expr, $start:expr, $end:expr) => {{
            let text: String = chars[$start..$end].iter().collect();
            out.push(LexToken { kind: $kind, text });
        }};
    }

    while i < chars.len() {
        let c = chars[i];
        let start = i;

        // --- newline (its own kind so the formatter can count blank lines) ---
        if c == '\n' {
            i += 1;
            push!(SyntaxKind::Newline, start, i);
            continue;
        }
        // --- other whitespace (runs collapsed into one token, excluding \n) ---
        if c.is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() && chars[i] != '\n' {
                i += 1;
            }
            push!(SyntaxKind::Whitespace, start, i);
            continue;
        }
        // --- comments ---
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            push!(SyntaxKind::LineComment, start, i);
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            // Consume the closing */ if present; if not, we still emit what we
            // scanned (losslessness) and leave error reporting to the parser.
            if i + 1 < chars.len() {
                i += 2;
            } else {
                i = chars.len();
            }
            push!(SyntaxKind::BlockComment, start, i);
            continue;
        }

        // --- non-trivia: implemented in Tasks 6-8. For now, emit one Error
        //     char so the stream stays lossless and tests can grow. ---
        i += 1;
        push!(SyntaxKind::Error, start, i);
    }

    out
}
```

- [ ] **Step 4: Run the trivia tests**

Run: `cargo test --lib syntax::lexer 2>&1 | tail -15`
Expected: `lossless_trivia_only`, `classifies_trivia_kinds`, `unterminated_block_comment_is_lossless` all PASS.

- [ ] **Step 5: Export `lex` + `render` from the module**

In `src/syntax/mod.rs` add:

```rust
pub mod lexer;

pub use lexer::{lex, render, LexToken};
```

- [ ] **Step 6: Clippy + commit**

Run: `cargo clippy --all-targets 2>&1 | tail -5`
Expected: clean (the trailing `Error` arm is intentional and temporary).

```bash
git add src/syntax/lexer.rs src/syntax/mod.rs
git commit -m "feat(syntax): trivia-emitting lexer (whitespace/comments) + losslessness tests"
```

---

## Task 5: Lex operators, punctuation, numbers

**Files:**
- Modify: `src/syntax/lexer.rs`

- [ ] **Step 1: Add tests for operators + numbers**

Add to the `tests` mod in `src/syntax/lexer.rs`:

```rust
    #[test]
    fn operators_and_numbers() {
        use SyntaxKind::*;
        assert_eq!(kinds("1 + 2"), vec![Number, Whitespace, Plus, Whitespace, Number]);
        assert_eq!(kinds("a**=b"), vec![Ident, StarStar, Eq, Ident]); // **= is ** then = (matches legacy: StarStar wins, then Eq)
        assert_eq!(kinds("x ?? y ?. z"), vec![Ident, Whitespace, QuestionQuestion, Whitespace, Ident, Whitespace, QuestionDot, Whitespace, Ident]);
        assert_eq!(kinds("0..=10"), vec![Number, DotDotEq, Number]);
        assert_eq!(kinds("a...b"), vec![Ident, DotDotDot, Ident]);
        assert_eq!(render(&lex("3.14 + 0xFF")), "3.14 + 0xFF");
    }
```

> The `a**=b` expectation matches the legacy lexer (`src/lexer.rs:132-142`): `**` is recognized before `*=`, so `**=` lexes as `StarStar` then `Eq`. Preserve this exactly.

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::lexer::tests::operators_and_numbers 2>&1 | tail -15`
Expected: FAIL — these currently lex as `Error` chars.

- [ ] **Step 3: Implement operator/punctuation/number scanning**

In `src/syntax/lexer.rs`, replace the temporary `Error`-char arm (the `// --- non-trivia` block) with operator, number, and a fallthrough. Insert this **before** the final `Error` fallback:

```rust
        // --- numbers: reuse the legacy scan shape (decimal/float/hex/bin/sci) ---
        if c.is_ascii_digit() {
            i = scan_number(&chars, i);
            push!(SyntaxKind::Number, start, i);
            continue;
        }

        // --- multi-char operators first (longest match), then single-char ---
        if let Some((kind, len)) = match_operator(&chars, i) {
            i += len;
            push!(kind, start, i);
            continue;
        }
```

Then add these two free functions at the bottom of the file (outside `lex`):

```rust
/// Advance past a numeric literal starting at `i` (which points at a digit).
/// Mirrors the legacy lexer (`src/lexer.rs:323-395`): hex/bin prefixes, decimal
/// with `_` separators, a fraction only when `.` is followed by a digit (so
/// `0..5` and `a.0` are NOT consumed as floats), and an optional exponent.
fn scan_number(chars: &[char], mut i: usize) -> usize {
    let n = chars.len();
    // 0x.. / 0b.. prefixes
    if chars[i] == '0' && i + 1 < n && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
        i += 2;
        while i < n && (chars[i].is_ascii_hexdigit() || chars[i] == '_') { i += 1; }
        return i;
    }
    if chars[i] == '0' && i + 1 < n && (chars[i + 1] == 'b' || chars[i + 1] == 'B') {
        i += 2;
        while i < n && (chars[i] == '0' || chars[i] == '1' || chars[i] == '_') { i += 1; }
        return i;
    }
    // integer part
    while i < n && (chars[i].is_ascii_digit() || chars[i] == '_') { i += 1; }
    // fraction: only if `.` is followed by a digit
    if i + 1 < n && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
        i += 1;
        while i < n && (chars[i].is_ascii_digit() || chars[i] == '_') { i += 1; }
    }
    // exponent: e/E [+/-] digits
    if i < n && (chars[i] == 'e' || chars[i] == 'E') {
        let mut j = i + 1;
        if j < n && (chars[j] == '+' || chars[j] == '-') { j += 1; }
        if j < n && chars[j].is_ascii_digit() {
            j += 1;
            while j < n && chars[j].is_ascii_digit() { j += 1; }
            i = j;
        }
    }
    i
}

/// Longest-match operator/punctuation table. Returns (kind, char-length).
/// Order matters: 3-char before 2-char before 1-char. `**` before `*=` to match
/// the legacy lexer exactly.
fn match_operator(chars: &[char], i: usize) -> Option<(SyntaxKind, usize)> {
    use SyntaxKind::*;
    let n = chars.len();
    let c0 = chars[i];
    let c1 = if i + 1 < n { Some(chars[i + 1]) } else { None };
    let c2 = if i + 2 < n { Some(chars[i + 2]) } else { None };

    // 3-char
    match (c0, c1, c2) {
        ('.', Some('.'), Some('=')) => return Some((DotDotEq, 3)),
        ('.', Some('.'), Some('.')) => return Some((DotDotDot, 3)),
        _ => {}
    }
    // 2-char
    if let Some(c1) = c1 {
        let two = match (c0, c1) {
            ('*', '*') => Some(StarStar),
            ('=', '=') => Some(EqEq),
            ('!', '=') => Some(BangEq),
            ('<', '=') => Some(Le),
            ('>', '=') => Some(Ge),
            ('&', '&') => Some(AmpAmp),
            ('|', '|') => Some(PipePipe),
            ('?', '?') => Some(QuestionQuestion),
            ('?', '.') => Some(QuestionDot),
            ('+', '=') => Some(PlusEq),
            ('-', '=') => Some(MinusEq),
            ('*', '=') => Some(StarEq),
            ('/', '=') => Some(SlashEq),
            ('.', '.') => Some(DotDot),
            ('=', '>') => Some(FatArrow),
            _ => None,
        };
        if let Some(k) = two {
            return Some((k, 2));
        }
    }
    // 1-char
    let one = match c0 {
        '+' => Plus, '-' => Minus, '*' => Star, '/' => Slash, '%' => Percent,
        '(' => LParen, ')' => RParen, '{' => LBrace, '}' => RBrace,
        '[' => LBracket, ']' => RBracket, ',' => Comma, '.' => Dot,
        ':' => Colon, ';' => Semicolon, '!' => Bang, '=' => Eq,
        '<' => Lt, '>' => Gt, '|' => Pipe, '?' => Question,
        _ => return None,
    };
    Some((one, 1))
}
```

> Note: `match_operator` does not know about `{`/`}` inside templates — templates are handled in Task 8 *before* this point in the dispatch, so a `{` reaching here is a real brace.

- [ ] **Step 4: Run the test**

Run: `cargo test --lib syntax::lexer::tests::operators_and_numbers 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy --all-targets 2>&1 | tail -5`
Expected: clean.

```bash
git add src/syntax/lexer.rs
git commit -m "feat(syntax): lex operators, punctuation, and numbers (legacy-faithful)"
```

---

## Task 6: Lex identifiers & keywords

**Files:**
- Modify: `src/syntax/lexer.rs`

- [ ] **Step 1: Add tests**

Add to the `tests` mod:

```rust
    #[test]
    fn identifiers_and_keywords() {
        use SyntaxKind::*;
        assert_eq!(kinds("let x"), vec![LetKw, Whitespace, Ident]);
        assert_eq!(kinds("return"), vec![ReturnKw]);
        assert_eq!(kinds("await x"), vec![AwaitKw, Whitespace, Ident]);
        assert_eq!(kinds("as"), vec![Ident]); // soft keyword stays Ident
        assert_eq!(kinds("trueish"), vec![Ident]); // not the `true` keyword
        assert_eq!(kinds("_foo123"), vec![Ident]);
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::lexer::tests::identifiers_and_keywords 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Implement identifier/keyword scanning**

In `lex`, insert this arm **before** the `match_operator` block (identifiers must be tried before single-char operators, though they don't overlap, keeping it before numbers is fine too — place it right after the number arm):

```rust
        // --- identifiers & keywords ---
        if c.is_alphabetic() || c == '_' {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let text: String = chars[i..j].iter().collect();
            let kind = keyword_kind(&text).unwrap_or(SyntaxKind::Ident);
            out.push(LexToken { kind, text });
            i = j;
            continue;
        }
```

Add this free function at the bottom of the file:

```rust
/// Map a reserved word to its keyword kind. Mirrors the legacy keyword set
/// (`src/token.rs`); `as`/`of` handling matches today (`of` is a keyword,
/// `as` is a soft keyword recognized by the parser, so it stays `Ident`).
fn keyword_kind(s: &str) -> Option<SyntaxKind> {
    use SyntaxKind::*;
    Some(match s {
        "true" => TrueKw, "false" => FalseKw, "nil" => NilKw,
        "let" => LetKw, "const" => ConstKw, "if" => IfKw, "else" => ElseKw,
        "while" => WhileKw, "for" => ForKw, "in" => InKw, "of" => OfKw,
        "return" => ReturnKw, "break" => BreakKw, "continue" => ContinueKw,
        "fn" => FnKw, "enum" => EnumKw, "match" => MatchKw, "class" => ClassKw,
        "import" => ImportKw, "export" => ExportKw, "async" => AsyncKw,
        "await" => AwaitKw, "yield" => YieldKw,
        _ => return None,
    })
}
```

> Confirm against `src/token.rs:42-76` that this keyword set matches the legacy `Tok` keywords exactly. If the legacy lexer treats `of` differently, match whatever `src/lexer.rs` does — the differential corpus test in Task 8 will catch any divergence.

- [ ] **Step 4: Run the test**

Run: `cargo test --lib syntax::lexer::tests::identifiers_and_keywords 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: Clippy + commit**

```bash
git add src/syntax/lexer.rs
git commit -m "feat(syntax): lex identifiers and keywords"
```

---

## Task 7: Lex strings & templates

**Files:**
- Modify: `src/syntax/lexer.rs`

- [ ] **Step 1: Add tests**

Add to the `tests` mod:

```rust
    #[test]
    fn strings_and_templates_are_lossless() {
        for src in [
            r#""hello\nworld""#,
            r#"'single \'quoted\''"#,
            "`plain template`",
            "`a${x}b`",
            "`outer ${ `inner ${y}` } end`", // nested template
            r#""has } and { and ${ literally""#,
        ] {
            assert_eq!(render(&lex(src)), src, "not lossless: {src}");
        }
    }

    #[test]
    fn string_kinds() {
        use SyntaxKind::*;
        assert_eq!(kinds(r#""hi""#), vec![Str]);
        assert_eq!(kinds("`plain`"), vec![TemplateStr]);
        // `a${x}b` => TemplateStart "a${", Ident x, TemplateEnd "}b"
        assert_eq!(kinds("`a${x}b`"), vec![TemplateStart, Ident, TemplateEnd]);
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::lexer::tests::strings_and_templates_are_lossless syntax::lexer::tests::string_kinds 2>&1 | tail -20`
Expected: FAIL — `"`/`'`/`` ` `` currently fall through to `Error`.

- [ ] **Step 3: Implement string + template lexing**

This is the most intricate part because templates interleave text and interpolated expressions, and interpolations can contain nested templates and braces. Reuse the legacy approach (`src/lexer.rs` template handling) by tracking a brace/template stack *within this lexer*. Add, in `lex`, **before** the `match_operator` block:

```rust
        // --- plain strings: "..." and '...' ---
        if c == '"' || c == '\'' {
            let j = scan_string_end(&chars, i, c);
            push!(SyntaxKind::Str, start, j);
            i = j;
            continue;
        }
        // --- templates: `...` with ${ } interpolations ---
        if c == '`' {
            // Emit TemplateStr (no interp) or TemplateStart (up to first `${`).
            let (kind, j) = scan_template_chunk(&chars, i, /*from_backtick=*/ true);
            push!(kind, start, j);
            i = j;
            if kind == SyntaxKind::TemplateStart {
                template_stack.push(brace_depth);
            }
            continue;
        }
        // --- `}` that closes a template interpolation resumes template text ---
        if c == '}' && template_stack.last() == Some(&brace_depth) {
            let (kind, j) = scan_template_chunk(&chars, i, /*from_backtick=*/ false);
            push!(kind, start, j);
            i = j;
            if kind == SyntaxKind::TemplateEnd {
                template_stack.pop();
            }
            continue;
        }
```

Add `brace_depth`/`template_stack` locals at the top of `lex` (next to `out`):

```rust
    let mut brace_depth = 0usize;
    let mut template_stack: Vec<usize> = Vec::new();
```

And maintain `brace_depth` in the operator arm: after the `match_operator` push, adjust for braces. Replace the operator arm's body with:

```rust
        if let Some((kind, len)) = match_operator(&chars, i) {
            match kind {
                SyntaxKind::LBrace => brace_depth += 1,
                SyntaxKind::RBrace => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
            i += len;
            push!(kind, start, i);
            continue;
        }
```

Add the two scanners at the bottom of the file. For losslessness we only need *boundaries*, not decoded escape values, so these scanners just find the end index (handling `\` escapes inline so an escaped quote/backtick doesn't end the literal early):

```rust
/// Find the index just past the closing quote of a "..."/'...' string starting
/// at `i` (which points at the opening quote `q`). Honors backslash escapes.
/// If the string is unterminated, returns chars.len() (lossless recovery).
fn scan_string_end(chars: &[char], i: usize, q: char) -> usize {
    let n = chars.len();
    let mut j = i + 1;
    while j < n {
        match chars[j] {
            '\\' if j + 1 < n => j += 2,
            c if c == q => return j + 1,
            _ => j += 1,
        }
    }
    n
}

/// Scan a template text chunk. `from_backtick=true` starts just after a `` ` ``
/// (or is given the backtick at `i`); `false` starts at a `}` closing an
/// interpolation. Returns (kind, end_index) where kind is one of
/// TemplateStr/TemplateStart (from backtick) or TemplateMiddle/TemplateEnd
/// (from `}`). Stops at an unescaped `${` (more interpolation) or the closing
/// `` ` ``. Lossless: the returned slice includes the opening `` ` ``/`}` and
/// the closing `` ` ``/`${`.
fn scan_template_chunk(chars: &[char], i: usize, from_backtick: bool) -> (SyntaxKind, usize) {
    use SyntaxKind::*;
    let n = chars.len();
    let mut j = i + 1; // skip opening ` or }
    while j < n {
        match chars[j] {
            '\\' if j + 1 < n => j += 2,
            '`' => {
                let kind = if from_backtick { TemplateStr } else { TemplateEnd };
                return (kind, j + 1);
            }
            '$' if j + 1 < n && chars[j + 1] == '{' => {
                let kind = if from_backtick { TemplateStart } else { TemplateMiddle };
                return (kind, j + 2); // include the ${
            }
            _ => j += 1,
        }
    }
    // Unterminated: emit what we have (lossless); parser reports the error.
    let kind = if from_backtick { TemplateStr } else { TemplateEnd };
    (kind, n)
}
```

> This mirrors the legacy template state machine (`src/lexer.rs:59-..` and the `}`-resume logic around `:270`). The differential corpus test in Task 8 validates parity against the legacy lexer on every real template in `examples/`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib syntax::lexer::tests::strings_and_templates_are_lossless syntax::lexer::tests::string_kinds 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Full lexer test run + clippy + commit**

Run: `cargo test --lib syntax::lexer 2>&1 | tail -15`
Expected: all `syntax::lexer` tests PASS.
Run: `cargo clippy --all-targets 2>&1 | tail -5`
Expected: clean.

```bash
git add src/syntax/lexer.rs
git commit -m "feat(syntax): lex strings and templates (lossless, legacy-faithful)"
```

---

## Task 8: Corpus losslessness + differential token-count guard

Prove losslessness on every real program, and guard token parity against the legacy lexer.

**Files:**
- Create: `tests/cst_lossless.rs`

- [ ] **Step 1: Write the corpus losslessness test**

Create `tests/cst_lossless.rs`:

```rust
//! Every example program must round-trip through the new lexer byte-for-byte,
//! and the new lexer's non-trivia token count must match the legacy lexer's
//! token count (a differential guard that the two agree on tokenization).

use std::fs;
use std::path::Path;

fn as_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            as_files(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("as") {
            out.push(p);
        }
    }
}

fn corpus() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    as_files(Path::new("examples"), &mut v);
    v.sort();
    assert!(!v.is_empty(), "no example .as files found");
    v
}

#[test]
fn lexer_is_lossless_over_corpus() {
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let toks = ascript::syntax::lex(&src);
        let rendered = ascript::syntax::render(&toks);
        assert_eq!(rendered, src, "lexer not lossless for {}", path.display());
    }
}

#[test]
fn no_error_tokens_over_corpus() {
    use ascript::syntax::SyntaxKind;
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        for t in ascript::syntax::lex(&src) {
            assert_ne!(
                t.kind,
                SyntaxKind::Error,
                "unexpected Error token {:?} in {}",
                t.text,
                path.display()
            );
        }
    }
}

#[test]
fn nontrivia_token_count_matches_legacy() {
    use ascript::syntax::SyntaxKind;
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        // Legacy: count tokens excluding the trailing Eof.
        let legacy = ascript::lexer::lex(&src).expect("legacy lex");
        let legacy_count = legacy
            .iter()
            .filter(|t| !matches!(t.tok, ascript::token::Tok::Eof))
            .count();
        // New: count non-trivia tokens.
        let new_count = ascript::syntax::lex(&src)
            .into_iter()
            .filter(|t| !t.kind.is_trivia())
            .count();
        assert_eq!(
            new_count, legacy_count,
            "token count mismatch for {} (new={}, legacy={})",
            path.display(), new_count, legacy_count
        );
    }
}
```

- [ ] **Step 2: Ensure required items are public**

Confirm `ascript::lexer::lex`, `ascript::token::Tok`, and `ascript::syntax::{lex, render, SyntaxKind}` are reachable. `lexer` and `token` are already `pub mod` (`src/lib.rs:8,16`); `syntax` was added in Task 2. `SyntaxKind::is_trivia` is `pub` (Task 2). No change expected.

- [ ] **Step 3: Run the corpus tests**

Run: `cargo test --test cst_lossless 2>&1 | tail -25`
Expected: all three PASS. If `nontrivia_token_count_matches_legacy` fails, the report names the file — inspect that file's tokens to find where the new lexer diverges (most likely a template or operator edge case) and fix the lexer; re-run.

- [ ] **Step 4: Run the full suite (no regressions) + clippy both configs**

Run: `cargo test 2>&1 | tail -15`
Expected: full suite green (new tests added, nothing else changed).
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean in BOTH configs (per CLAUDE.md).

- [ ] **Step 5: Commit**

```bash
git add tests/cst_lossless.rs
git commit -m "test(syntax): corpus losslessness + differential token-count guard vs legacy lexer"
```

---

## Task 9: Build a flat CST and prove tree-level round-trip

Assemble the token stream into a `cstree` green tree (all tokens under `Root`) and prove the *tree's* text reproduces the source. This exercises the cstree builder end-to-end and is the seam the parser plan will replace (it will introduce real node structure instead of a flat list).

**Files:**
- Modify: `src/syntax/cst.rs`
- Modify: `src/syntax/mod.rs`

- [ ] **Step 1: Write the tree round-trip test**

Add to `src/syntax/cst.rs` `tests` mod:

```rust
    #[test]
    fn flat_tree_round_trips_source() {
        let src = "let x = 1 // c\nfoo(`t${x}`)\n";
        let node = crate::syntax::build_flat_tree(src);
        assert_eq!(node.text().to_string(), src);
    }
```

- [ ] **Step 2: Run (expect failure)**

Run: `cargo test --lib syntax::cst::tests::flat_tree_round_trips_source 2>&1 | tail -15`
Expected: FAIL — `build_flat_tree` does not exist.

- [ ] **Step 3: Implement `build_flat_tree`**

Add to `src/syntax/cst.rs`:

```rust
use cstree::build::GreenNodeBuilder;
use crate::syntax::lexer::lex;

/// Build a flat CST: a single `Root` node containing every lexeme (including
/// trivia) as a token, in source order. Temporary scaffolding — the parser plan
/// replaces this with real node structure. Proves the cstree builder + lexer
/// produce a lossless tree.
pub fn build_flat_tree(src: &str) -> SyntaxNode {
    let mut builder: GreenNodeBuilder<SyntaxKind> = GreenNodeBuilder::new();
    builder.start_node(SyntaxKind::Root);
    for t in lex(src) {
        builder.token(t.kind, &t.text);
    }
    builder.finish_node();
    let (green, cache) = builder.finish();
    let resolver = cache.unwrap().into_interner().unwrap();
    SyntaxNode::new_root_with_resolver(green, resolver)
}
```

> If Task 2's spike required different `cache`/resolver handling, mirror that exact code here (keep the two in sync).

- [ ] **Step 4: Export it**

In `src/syntax/mod.rs` add to the re-exports:

```rust
pub use cst::{build_flat_tree, SyntaxNode, SyntaxToken};
```

- [ ] **Step 5: Run the test**

Run: `cargo test --lib syntax::cst::tests::flat_tree_round_trips_source 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 6: Add a corpus tree round-trip test**

Add to `tests/cst_lossless.rs`:

```rust
#[test]
fn flat_tree_is_lossless_over_corpus() {
    for path in corpus() {
        let src = fs::read_to_string(&path).unwrap();
        let node = ascript::syntax::build_flat_tree(&src);
        assert_eq!(node.text().to_string(), src, "tree not lossless for {}", path.display());
    }
}
```

- [ ] **Step 7: Run everything + clippy both configs**

Run: `cargo test --test cst_lossless 2>&1 | tail -15`
Expected: all PASS including `flat_tree_is_lossless_over_corpus`.
Run: `cargo clippy --all-targets 2>&1 | tail -5 && cargo clippy --no-default-features --all-targets 2>&1 | tail -5`
Expected: clean both.

- [ ] **Step 8: Commit**

```bash
git add src/syntax/cst.rs src/syntax/mod.rs tests/cst_lossless.rs
git commit -m "feat(syntax): build flat cstree CST + corpus tree-level losslessness"
```

---

## Done criteria for Plan 1

- [ ] `cargo test` fully green; `cargo clippy --all-targets` and `cargo clippy --no-default-features --all-targets` both clean.
- [ ] New lexer is byte-for-byte lossless over every `examples/**/*.as`.
- [ ] New lexer's non-trivia token count matches the legacy lexer on the whole corpus.
- [ ] A flat cstree CST round-trips the whole corpus.
- [ ] The legacy front-end and the binary are **unchanged** — nothing user-facing switched.

**Next plan:** `2026-06-??-cst-event-parser-and-codegen.md` — convert the parser to emit `start_node/token/finish_node` events with error recovery and trivia attachment, add the `ascript.ungram` grammar + `build.rs` codegen for the typed-AST layer, and add the tree-sitter differential parse oracle.
