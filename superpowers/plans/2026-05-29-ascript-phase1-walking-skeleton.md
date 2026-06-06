# AScript Phase 1 · Milestone 1 — Walking Skeleton Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a minimal end-to-end AScript interpreter so that `ascript run file.as` evaluates arithmetic expressions, string literals, and calls the built-in `print` — exercising every architectural layer (lexer → parser → async tree-walker → CLI).

**Architecture:** A single Rust crate `ascript` (library + binary) whose modules mirror the future crate boundaries from the spec (§12.2). Source flows `lex → parse → eval`. The evaluator's core methods are `async fn` running on a Tokio runtime, establishing the async-eval seam (spec §7) even though the skeleton never suspends yet. A unified `AsError { message, span }` carries errors; rich `ariadne` diagnostics are deferred to a later milestone.

**Tech Stack:** Rust (edition 2021), `tokio` (async runtime), `async-recursion` (recursive `async fn`), `cargo test`.

**Scope note (deviation from spec §12.2):** The spec describes a multi-crate workspace. This skeleton uses one crate with internal modules (`span`, `token`, `lexer`, `ast`, `parser`, `value`, `error`, `interp`) named after the future crates. Splitting into a workspace is deferred until compile-time or boundary pressure justifies it. Spans use **char offsets** in the skeleton; the spec's byte-offset precision arrives with the `ariadne` diagnostics milestone.

**Prerequisite:** A working Rust toolchain (`rustc`/`cargo` ≥ 1.75 for stable `async fn` ergonomics). Verify with `cargo --version`.

---

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | crate metadata + dependencies |
| `src/lib.rs` | module declarations + the `run_source` pipeline entry point |
| `src/main.rs` | `ascript` binary: arg parsing, file read, run, exit code |
| `src/span.rs` | `Span { start, end }` source-location type |
| `src/error.rs` | `AsError` unified error with optional span + `Display` |
| `src/value.rs` | `Value` runtime value enum + `Display` |
| `src/token.rs` | `Tok` token kinds + `Token { tok, span }` |
| `src/lexer.rs` | `lex(&str) -> Result<Vec<Token>, AsError>` |
| `src/ast.rs` | `Expr`, `Stmt`, `BinOp`, `UnOp` + `Display` (s-expr, for tests) |
| `src/parser.rs` | `parse(&[Token]) -> Result<Vec<Stmt>, AsError>` (precedence climbing) |
| `src/interp.rs` | `Interp` async tree-walking evaluator + builtin `print` |
| `tests/cli.rs` | end-to-end test invoking the compiled binary |
| `examples/hello.as` | sample program for manual runs |

---

## Task 1: Project scaffold + foundational types

Sets up the crate, dependencies, and the three leaf types (`Span`, `AsError`, `Value`) that everything else depends on. TDD target: `Value`'s `Display` formatting (numbers print without a trailing `.0`, matching JS/Lua).

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/span.rs`
- Create: `src/error.rs`
- Create: `src/value.rs`

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "ascript"
version = "0.1.0"
edition = "2021"

[lib]
name = "ascript"
path = "src/lib.rs"

[[bin]]
name = "ascript"
path = "src/main.rs"

[dependencies]
tokio = { version = "1", features = ["rt", "rt-multi-thread", "macros"] }
async-recursion = "1"
```

- [ ] **Step 2: Create `src/span.rs`**

```rust
//! Source location: a half-open `[start, end)` range of char offsets.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }
}
```

- [ ] **Step 3: Create `src/error.rs`**

```rust
//! Unified error type for every stage (lex, parse, eval).

use crate::span::Span;
use std::fmt;

#[derive(Debug)]
pub struct AsError {
    pub message: String,
    pub span: Option<Span>,
}

impl AsError {
    pub fn new(message: impl Into<String>) -> Self {
        AsError { message: message.into(), span: None }
    }

    pub fn at(message: impl Into<String>, span: Span) -> Self {
        AsError { message: message.into(), span: Some(span) }
    }
}

impl fmt::Display for AsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(s) => write!(f, "{} (at {}..{})", self.message, s.start, s.end),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for AsError {}
```

- [ ] **Step 4: Create `src/value.rs` with the failing test**

```rust
//! Runtime values. The skeleton supports four of the eight value kinds
//! from spec §4; the rest arrive in later milestones.

use std::fmt;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    Str(Rc<str>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{}", b),
            // Rust's f64 Display already prints 7.0 as "7" and 2.5 as "2.5".
            Value::Number(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{}", s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_values_like_a_script_language() {
        assert_eq!(Value::Number(7.0).to_string(), "7");
        assert_eq!(Value::Number(2.5).to_string(), "2.5");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Nil.to_string(), "nil");
        assert_eq!(Value::Str("hi".into()).to_string(), "hi");
    }
}
```

- [ ] **Step 5: Create `src/lib.rs`**

```rust
pub mod error;
pub mod span;
pub mod value;
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test --lib value`
Expected: `test value::tests::displays_values_like_a_script_language ... ok`

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml src/lib.rs src/span.rs src/error.rs src/value.rs
git commit -m "feat: scaffold ascript crate with span, error, and value types"
```

---

## Task 2: Lexer

Turns source text into a `Vec<Token>`. TDD target: tokenizing `1 + 2 * 3`, a string literal, and the `true`/`false`/`nil` keywords, ending in `Eof`.

**Files:**
- Create: `src/token.rs`
- Create: `src/lexer.rs`
- Modify: `src/lib.rs` (add `pub mod token; pub mod lexer;`)

- [ ] **Step 1: Create `src/token.rs`**

```rust
//! Token kinds and tokens (a kind plus its source span).

use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Number(f64),
    Str(String),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
    Comma,
    True,
    False,
    Nil,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}
```

- [ ] **Step 2: Create `src/lexer.rs` with the failing test**

```rust
//! Hand-written lexer. Produces tokens with char-offset spans.

use crate::error::AsError;
use crate::span::Span;
use crate::token::{Tok, Token};

pub fn lex(src: &str) -> Result<Vec<Token>, AsError> {
    let chars: Vec<char> = src.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        let start = i;

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        match c {
            '+' => push(&mut tokens, Tok::Plus, start, &mut i),
            '-' => push(&mut tokens, Tok::Minus, start, &mut i),
            '*' => push(&mut tokens, Tok::Star, start, &mut i),
            '/' => push(&mut tokens, Tok::Slash, start, &mut i),
            '%' => push(&mut tokens, Tok::Percent, start, &mut i),
            '(' => push(&mut tokens, Tok::LParen, start, &mut i),
            ')' => push(&mut tokens, Tok::RParen, start, &mut i),
            ',' => push(&mut tokens, Tok::Comma, start, &mut i),
            '"' => {
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != '"' {
                    s.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(AsError::at("unterminated string", Span::new(start, i)));
                }
                i += 1; // consume closing quote
                tokens.push(Token { tok: Tok::Str(s), span: Span::new(start, i) });
            }
            c if c.is_ascii_digit() => {
                let mut j = i;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == '.' {
                    j += 1;
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                let text: String = chars[i..j].iter().collect();
                let n: f64 = text
                    .parse()
                    .map_err(|_| AsError::at("invalid number", Span::new(i, j)))?;
                tokens.push(Token { tok: Tok::Number(n), span: Span::new(i, j) });
                i = j;
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut j = i;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let text: String = chars[i..j].iter().collect();
                let tok = match text.as_str() {
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "nil" => Tok::Nil,
                    _ => Tok::Ident(text),
                };
                tokens.push(Token { tok, span: Span::new(i, j) });
                i = j;
            }
            other => {
                return Err(AsError::at(
                    format!("unexpected character '{}'", other),
                    Span::new(start, start + 1),
                ));
            }
        }
    }

    tokens.push(Token { tok: Tok::Eof, span: Span::new(chars.len(), chars.len()) });
    Ok(tokens)
}

fn push(tokens: &mut Vec<Token>, tok: Tok, start: usize, i: &mut usize) {
    tokens.push(Token { tok, span: Span::new(start, start + 1) });
    *i += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        lex(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn lexes_arithmetic() {
        assert_eq!(
            kinds("1 + 2 * 3"),
            vec![
                Tok::Number(1.0),
                Tok::Plus,
                Tok::Number(2.0),
                Tok::Star,
                Tok::Number(3.0),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_strings_and_keywords() {
        assert_eq!(
            kinds("\"hi\" true nil"),
            vec![Tok::Str("hi".into()), Tok::True, Tok::Nil, Tok::Eof]
        );
    }

    #[test]
    fn rejects_unterminated_string() {
        let err = lex("\"oops").unwrap_err();
        assert!(err.message.contains("unterminated"));
    }
}
```

- [ ] **Step 3: Update `src/lib.rs`**

```rust
pub mod error;
pub mod lexer;
pub mod span;
pub mod token;
pub mod value;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib lexer`
Expected: 3 tests pass (`lexes_arithmetic`, `lexes_strings_and_keywords`, `rejects_unterminated_string`).

- [ ] **Step 5: Commit**

```bash
git add src/token.rs src/lexer.rs src/lib.rs
git commit -m "feat: add lexer for numbers, strings, keywords, and operators"
```

---

## Task 3: AST + Parser

Defines expression/statement nodes and a precedence-climbing parser. TDD target: `1 + 2 * 3` parses to `(+ 1 (* 2 3))` (multiplication binds tighter), and `print("hi")` parses to a call. The `Display` impls render an s-expression so tests read cleanly.

**Files:**
- Create: `src/ast.rs`
- Create: `src/parser.rs`
- Modify: `src/lib.rs` (add `pub mod ast; pub mod parser;`)

- [ ] **Step 1: Create `src/ast.rs`**

```rust
//! Abstract syntax tree for the skeleton subset.

use std::fmt;

#[derive(Clone, Debug)]
pub enum Expr {
    Number(f64),
    Str(String),
    Bool(bool),
    Nil,
    Ident(String),
    Unary { op: UnOp, expr: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Call { callee: Box<Expr>, args: Vec<Expr> },
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
}

#[derive(Clone, Copy, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Clone, Copy, Debug)]
pub enum UnOp {
    Neg,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
        };
        write!(f, "{}", s)
    }
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnOp::Neg => write!(f, "-"),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Number(n) => write!(f, "{}", n),
            Expr::Str(s) => write!(f, "{:?}", s),
            Expr::Bool(b) => write!(f, "{}", b),
            Expr::Nil => write!(f, "nil"),
            Expr::Ident(name) => write!(f, "{}", name),
            Expr::Unary { op, expr } => write!(f, "({} {})", op, expr),
            Expr::Binary { op, lhs, rhs } => write!(f, "({} {} {})", op, lhs, rhs),
            Expr::Call { callee, args } => {
                write!(f, "(call {}", callee)?;
                for a in args {
                    write!(f, " {}", a)?;
                }
                write!(f, ")")
            }
        }
    }
}
```

- [ ] **Step 2: Create `src/parser.rs` with the failing test**

```rust
//! Recursive-descent / precedence-climbing parser for the skeleton subset.

use crate::ast::{BinOp, Expr, Stmt, UnOp};
use crate::error::AsError;
use crate::span::Span;
use crate::token::{Tok, Token};

pub fn parse(tokens: &[Token]) -> Result<Vec<Stmt>, AsError> {
    let mut parser = Parser { tokens, pos: 0 };
    parser.program()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn advance(&mut self) -> Tok {
        let t = self.tokens[self.pos].tok.clone();
        self.pos += 1;
        t
    }

    fn eat(&mut self, expected: &Tok) -> Result<(), AsError> {
        if self.peek() == expected {
            self.pos += 1;
            Ok(())
        } else {
            Err(AsError::at(
                format!("expected {:?}, found {:?}", expected, self.peek()),
                self.span(),
            ))
        }
    }

    fn program(&mut self) -> Result<Vec<Stmt>, AsError> {
        let mut stmts = Vec::new();
        while *self.peek() != Tok::Eof {
            stmts.push(Stmt::Expr(self.expr()?));
        }
        Ok(stmts)
    }

    fn expr(&mut self) -> Result<Expr, AsError> {
        self.additive()
    }

    fn additive(&mut self) -> Result<Expr, AsError> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.multiplicative()?;
            left = Expr::Binary { op, lhs: Box::new(left), rhs: Box::new(right) };
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, AsError> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.unary()?;
            left = Expr::Binary { op, lhs: Box::new(left), rhs: Box::new(right) };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, AsError> {
        if *self.peek() == Tok::Minus {
            self.advance();
            let expr = self.unary()?;
            return Ok(Expr::Unary { op: UnOp::Neg, expr: Box::new(expr) });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, AsError> {
        let mut expr = self.primary()?;
        while *self.peek() == Tok::LParen {
            self.advance();
            let mut args = Vec::new();
            if *self.peek() != Tok::RParen {
                loop {
                    args.push(self.expr()?);
                    if *self.peek() == Tok::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
            self.eat(&Tok::RParen)?;
            expr = Expr::Call { callee: Box::new(expr), args };
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, AsError> {
        let span = self.span();
        match self.advance() {
            Tok::Number(n) => Ok(Expr::Number(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            Tok::Nil => Ok(Expr::Nil),
            Tok::Ident(name) => Ok(Expr::Ident(name)),
            Tok::LParen => {
                let inner = self.expr()?;
                self.eat(&Tok::RParen)?;
                Ok(inner)
            }
            other => Err(AsError::at(format!("unexpected token {:?}", other), span)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn sexpr(src: &str) -> String {
        let tokens = lex(src).unwrap();
        let stmts = parse(&tokens).unwrap();
        match &stmts[0] {
            Stmt::Expr(e) => e.to_string(),
        }
    }

    #[test]
    fn multiplication_binds_tighter_than_addition() {
        assert_eq!(sexpr("1 + 2 * 3"), "(+ 1 (* 2 3))");
    }

    #[test]
    fn parentheses_override_precedence() {
        assert_eq!(sexpr("(1 + 2) * 3"), "(* (+ 1 2) 3)");
    }

    #[test]
    fn parses_a_call() {
        assert_eq!(sexpr("print(\"hi\")"), "(call print \"hi\")");
    }
}
```

- [ ] **Step 3: Update `src/lib.rs`**

```rust
pub mod ast;
pub mod error;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;
pub mod value;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib parser`
Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/ast.rs src/parser.rs src/lib.rs
git commit -m "feat: add AST and precedence-climbing parser"
```

---

## Task 4: Async tree-walking interpreter

Evaluates the AST. The core methods are `async fn` (the §7 seam); recursion uses `#[async_recursion]`. Output goes into an in-memory buffer so it is testable. The only callable in the skeleton is the builtin `print`.

**Files:**
- Create: `src/interp.rs`
- Modify: `src/lib.rs` (add `pub mod interp;`)

- [ ] **Step 1: Create `src/interp.rs` with the failing test**

```rust
//! Async tree-walking evaluator. `eval_expr`/`exec` are async to establish
//! the event-loop seam from spec §7, even though the skeleton never suspends.

use crate::ast::{BinOp, Expr, Stmt, UnOp};
use crate::error::AsError;
use crate::value::Value;
use async_recursion::async_recursion;

pub struct Interp {
    /// Captured program output (what `print` writes). Exposed for testing and
    /// flushed to stdout by the CLI.
    pub output: String,
}

impl Interp {
    pub fn new() -> Self {
        Interp { output: String::new() }
    }

    pub async fn exec(&mut self, program: &[Stmt]) -> Result<(), AsError> {
        for stmt in program {
            match stmt {
                Stmt::Expr(e) => {
                    self.eval_expr(e).await?;
                }
            }
        }
        Ok(())
    }

    #[async_recursion(?Send)]
    pub async fn eval_expr(&mut self, expr: &Expr) -> Result<Value, AsError> {
        match expr {
            Expr::Number(n) => Ok(Value::Number(*n)),
            Expr::Str(s) => Ok(Value::Str(s.as_str().into())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Nil => Ok(Value::Nil),
            Expr::Ident(name) => {
                Err(AsError::new(format!("undefined variable '{}'", name)))
            }
            Expr::Unary { op, expr } => {
                let v = self.eval_expr(expr).await?;
                match (op, v) {
                    (UnOp::Neg, Value::Number(n)) => Ok(Value::Number(-n)),
                    (UnOp::Neg, _) => Err(AsError::new("cannot negate a non-number")),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let l = self.eval_expr(lhs).await?;
                let r = self.eval_expr(rhs).await?;
                let (a, b) = match (l, r) {
                    (Value::Number(a), Value::Number(b)) => (a, b),
                    _ => return Err(AsError::new("arithmetic requires two numbers")),
                };
                let result = match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => a / b,
                    BinOp::Mod => a % b,
                };
                Ok(Value::Number(result))
            }
            Expr::Call { callee, args } => {
                let name = match callee.as_ref() {
                    Expr::Ident(n) => n.clone(),
                    _ => return Err(AsError::new("only named builtins are callable in the skeleton")),
                };
                let mut values = Vec::new();
                for a in args {
                    values.push(self.eval_expr(a).await?);
                }
                self.call_builtin(&name, &values)
            }
        }
    }

    fn call_builtin(&mut self, name: &str, args: &[Value]) -> Result<Value, AsError> {
        match name {
            "print" => {
                let parts: Vec<String> = args.iter().map(|v| v.to_string()).collect();
                self.output.push_str(&parts.join(" "));
                self.output.push('\n');
                Ok(Value::Nil)
            }
            other => Err(AsError::new(format!("'{}' is not a function", other))),
        }
    }
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;

    async fn eval_to_value(src: &str) -> Value {
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let Stmt::Expr(e) = &stmts[0];
        interp.eval_expr(e).await.unwrap()
    }

    #[tokio::test]
    async fn evaluates_arithmetic_with_precedence() {
        match eval_to_value("1 + 2 * 3").await {
            Value::Number(n) => assert_eq!(n, 7.0),
            other => panic!("expected number, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn print_writes_to_the_output_buffer() {
        let stmts = parse(&lex("print(1 + 2 * 3)").unwrap()).unwrap();
        let mut interp = Interp::new();
        interp.exec(&stmts).await.unwrap();
        assert_eq!(interp.output, "7\n");
    }

    #[tokio::test]
    async fn calling_a_non_builtin_is_an_error() {
        let stmts = parse(&lex("nope(1)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let err = interp.exec(&stmts).await.unwrap_err();
        assert!(err.message.contains("is not a function"));
    }
}
```

- [ ] **Step 2: Update `src/lib.rs`**

```rust
pub mod ast;
pub mod error;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;
pub mod value;
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib interp`
Expected: 3 tests pass (`evaluates_arithmetic_with_precedence`, `print_writes_to_the_output_buffer`, `calling_a_non_builtin_is_an_error`).

- [ ] **Step 4: Commit**

```bash
git add src/interp.rs src/lib.rs
git commit -m "feat: add async tree-walking interpreter with builtin print"
```

---

## Task 5: Pipeline + CLI + end-to-end test

Wires the stages into `run_source`, adds the `ascript run <file>` binary, and proves the whole thing works by invoking the compiled binary on a real file.

**Files:**
- Modify: `src/lib.rs` (add the `run_source` pipeline function)
- Create: `src/main.rs`
- Create: `examples/hello.as`
- Create: `tests/cli.rs`

- [ ] **Step 1: Add `run_source` to `src/lib.rs`**

Append below the module declarations:

```rust
use crate::error::AsError;
use crate::interp::Interp;

/// Lex → parse → evaluate. Returns the program's captured output.
pub async fn run_source(src: &str) -> Result<String, AsError> {
    let tokens = lexer::lex(src)?;
    let program = parser::parse(&tokens)?;
    let mut interp = Interp::new();
    interp.exec(&program).await?;
    Ok(interp.output)
}
```

- [ ] **Step 2: Create `examples/hello.as`**

```
print(1 + 2 * 3)
```

- [ ] **Step 3: Create `src/main.rs`**

```rust
use std::process::ExitCode;

// Single-threaded runtime matches spec §7's single-threaded event loop and the
// interpreter's `?Send` (Rc-friendly) futures.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.len() != 3 || args[1] != "run" {
        eprintln!("usage: ascript run <file.as>");
        return ExitCode::from(2);
    }

    let path = &args[2];
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", path, e);
            return ExitCode::from(1);
        }
    };

    match ascript::run_source(&src).await {
        Ok(output) => {
            print!("{}", output);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}
```

- [ ] **Step 4: Create `tests/cli.rs` (the failing end-to-end test)**

```rust
//! End-to-end test: build the binary and run a real `.as` file.

use std::process::Command;

#[test]
fn runs_a_program_file_and_prints_result() {
    let file = std::env::temp_dir().join("ascript_skeleton_hello.as");
    std::fs::write(&file, "print(1 + 2 * 3)\n").unwrap();

    // Cargo sets CARGO_BIN_EXE_<name> for integration tests.
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg(&file).output().unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "7\n");
}

#[test]
fn reports_usage_without_args() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage"));
}
```

- [ ] **Step 5: Run the full test suite to verify everything passes**

Run: `cargo test`
Expected: all unit tests (value, lexer, parser, interp) and both `tests/cli.rs` tests pass.

- [ ] **Step 6: Manually verify the binary on the example**

Run: `cargo run --quiet -- run examples/hello.as`
Expected output: `7`

- [ ] **Step 7: Commit**

```bash
git add src/lib.rs src/main.rs examples/hello.as tests/cli.rs
git commit -m "feat: wire run_source pipeline and ascript CLI with end-to-end test"
```

---

## Definition of Done

- `cargo test` passes (unit + integration).
- `cargo run -- run examples/hello.as` prints `7`.
- Every architectural layer exists and connects: lexer → parser → async evaluator → CLI.
- The async-eval seam (`async fn eval_expr` on a Tokio runtime) is in place for later milestones to build on.

## Hand-off to Milestone 2

Milestone 2 ("Variables & control flow") extends this skeleton: add `let`/`const` with an `Environment` (a scope chain the `Interp` carries), comparison/logical operators, `if`/`while`/`for`, and block statements. The `Stmt` enum, `Expr` enum, and `eval_expr` match arms are the designed extension points — new variants slot in without touching the lexer→parser→eval→CLI seam established here.
