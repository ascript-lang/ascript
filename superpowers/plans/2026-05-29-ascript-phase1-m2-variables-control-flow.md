# AScript Phase 1 · Milestone 2 — Variables & Control Flow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the walking-skeleton interpreter so AScript programs can declare variables (`let`/`const`), use the full set of value-level operators (arithmetic incl. `**`, comparison, equality, logical with short-circuit, and nil-coalescing `??`), reassign mutable variables (incl. compound assignment `+= -= *= /=`), and branch/loop with `if`/`else`, `while`, and numeric-range `for`. Enough to write real programs (factorial, primality, etc.).

**Architecture:** Builds directly on Milestone 1 (lexer → parser → async tree-walking interpreter → CLI). First we add source spans to AST expression nodes (so runtime errors point at source). Then a `Rc<RefCell<…>>`-based lexical scope chain (`Environment`) threaded through evaluation as a parameter. Operators, statements, and control flow are added incrementally, each via TDD.

**Tech Stack:** Rust (edition 2021), `tokio` (single-threaded), `async-recursion`, `cargo test`. No new crates.

**Starting state (end of Milestone 1, on `main`):** `src/{span,error,value,token,lexer,ast,parser,interp}.rs`, `src/lib.rs` (`run_source`), `src/main.rs` (`ascript run` CLI, current_thread tokio), `tests/cli.rs`. Supported language: number/string/bool/nil literals, arithmetic (`+ - * / %`), unary `-`, parenthesized expressions, calling the single builtin `print`. `Ident` currently errors (no variables yet). 12 tests pass.

**Conventions carried over:**
- Spans are CHAR offsets. `AsError::new(msg)` (no span) / `AsError::at(msg, span)`.
- Truthiness: only `nil` and `false` are falsy (everything else, incl. `0`/`""`, is truthy) — spec §4.
- Single-threaded interpreter; values and scopes use `Rc`/`RefCell`, never `Arc`/`Mutex`.
- Statements are delimited structurally; an explicit `;` is accepted as an optional separator but never required.
- IEEE-754 numeric semantics are intentional and production-correct: `1/0` → `inf`, `0/0` → `NaN` (matching JS). This is a deliberate decision, not a gap.

---

## Scope & Justified Deferrals

This milestone deliberately includes **every spec feature whose only dependencies already exist** (numbers, strings, bools, nil, variables, control flow). The following are deferred **only** because they depend on a value type or mechanism not yet built. Each names its owning milestone.

| Deferred feature (spec ref) | Why it is *necessarily* deferred | Owning milestone |
|---|---|---|
| `for (x of iterable)` (§3) | Iteration requires an iterable value type (arrays); none exists yet. The range form `for (i in a..b)` **is** included here since it needs only numbers. | **M3 — Functions & Data** |
| Arrays `[…]`, objects `{…}`, maps (§4) | New heap value kinds; large surface; not needed for variables/control flow. | **M3 — Functions & Data** |
| Member access `.`, indexing `[]`, optional chaining `?.` (§4) | All operate on objects/arrays, which don't exist yet. | **M3 — Functions & Data** |
| User-defined functions `fn`, `return`, closures (§3) | Requires a callable `Value::Function` capturing an `Environment` + a call stack; the builtin-dispatch generalization noted in the M1 review lands here. | **M3 — Functions & Data** |
| `match` expression (§3, §8.2) | Most useful over enum variants and destructuring; pattern matching is co-designed with enums. | **M5 — Classes & Enums** |
| Template strings `` `…${}` `` (§2) | Interpolation requires lexing embedded sub-expressions (a lexer mode); orthogonal to control flow. | **M3 — Functions & Data** (richer literals) |
| `?` Result-propagation operator (§6) | Only meaningful once functions return `Result` pairs and the panic/Result tiers exist. The lexer reserves bare `?` with a clear error now. | **M3 — Functions & Data** / Result tier |
| Type annotations & runtime contracts (§5) | Gradual contracts are a dedicated, self-contained layer. | **M4 — Gradual Type Contracts** |
| Classes/enums, ESM modules, async I/O, stdlib (§§8–11) | Later phases by design. | **M5+ / Phase 2+** |

Everything else from the spec that pertains to variables, expressions, and control flow is implemented in this milestone — **nothing in-theme is punted.**

---

## File Structure (after Milestone 2)

| File | Change | Responsibility |
|---|---|---|
| `src/ast.rs` | modified | `Expr { kind, span }`; `ExprKind` (+ `Assign`); `Stmt` (Expr/Let/Block/If/While/ForRange); `BinOp`/`UnOp` (expanded); `Display` |
| `src/parser.rs` | modified | statement dispatch + full operator precedence ladder; spans on every expr |
| `src/interp.rs` | modified | env-threaded evaluation; statements; operators; short-circuit logic |
| `src/value.rs` | modified | `PartialEq` derive + `is_truthy()` |
| `src/env.rs` | **new** | `Environment` lexical scope chain (define/get/assign) |
| `src/lexer.rs` | modified | new operator tokens + keywords |
| `src/token.rs` | modified | new `Tok` variants |
| `src/lib.rs` | modified | `pub mod env;`; `run_source` creates a global environment |
| `examples/factorial.as` | **new** | demo program |
| `tests/cli.rs` | modified | end-to-end test for the demo |

---

## Task 1: Add source spans to AST expression nodes

Refactor `Expr` from a bare enum into `Expr { kind: ExprKind, span: Span }`. Mechanical but touches `ast.rs`, `parser.rs`, `interp.rs`. Immediate payoff: the interpreter attaches a span to the "undefined variable" error. Do this FIRST, before the AST grows.

**Files:**
- Modify: `src/ast.rs`
- Modify: `src/parser.rs`
- Modify: `src/interp.rs`

- [ ] **Step 1: Rewrite `src/ast.rs`** to wrap expressions with spans.

```rust
//! Abstract syntax tree.

use crate::span::Span;
use std::fmt;

/// An expression node plus the source span it was parsed from.
#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
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
        write!(f, "{}", self.kind)
    }
}

impl fmt::Display for ExprKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprKind::Number(n) => write!(f, "{}", n),
            ExprKind::Str(s) => write!(f, "{:?}", s),
            ExprKind::Bool(b) => write!(f, "{}", b),
            ExprKind::Nil => write!(f, "nil"),
            ExprKind::Ident(name) => write!(f, "{}", name),
            ExprKind::Unary { op, expr } => write!(f, "({} {})", op, expr),
            ExprKind::Binary { op, lhs, rhs } => write!(f, "({} {} {})", op, lhs, rhs),
            ExprKind::Call { callee, args } => {
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

- [ ] **Step 2: Rewrite `src/parser.rs`** so every expression is built as `Expr { kind, span }`.

```rust
//! Recursive-descent / precedence-climbing parser.

use crate::ast::{BinOp, Expr, ExprKind, Stmt, UnOp};
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

    /// End offset of the most recently consumed token.
    fn prev_end(&self) -> usize {
        self.tokens[self.pos - 1].span.end
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
            let span = Span::new(left.span.start, right.span.end);
            left = Expr {
                kind: ExprKind::Binary { op, lhs: Box::new(left), rhs: Box::new(right) },
                span,
            };
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
            let span = Span::new(left.span.start, right.span.end);
            left = Expr {
                kind: ExprKind::Binary { op, lhs: Box::new(left), rhs: Box::new(right) },
                span,
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, AsError> {
        let start = self.span().start;
        if *self.peek() == Tok::Minus {
            self.advance();
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Ok(Expr {
                kind: ExprKind::Unary { op: UnOp::Neg, expr: Box::new(operand) },
                span,
            });
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
            let span = Span::new(expr.span.start, self.prev_end());
            expr = Expr {
                kind: ExprKind::Call { callee: Box::new(expr), args },
                span,
            };
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, AsError> {
        let tok_span = self.span();
        let kind = match self.advance() {
            Tok::Number(n) => ExprKind::Number(n),
            Tok::Str(s) => ExprKind::Str(s),
            Tok::True => ExprKind::Bool(true),
            Tok::False => ExprKind::Bool(false),
            Tok::Nil => ExprKind::Nil,
            Tok::Ident(name) => ExprKind::Ident(name),
            Tok::LParen => {
                let inner = self.expr()?;
                self.eat(&Tok::RParen)?;
                return Ok(inner);
            }
            other => return Err(AsError::at(format!("unexpected token {:?}", other), tok_span)),
        };
        Ok(Expr { kind, span: tok_span })
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

    #[test]
    fn binary_span_covers_both_operands() {
        let tokens = lex("1 + 2").unwrap();
        let stmts = parse(&tokens).unwrap();
        match &stmts[0] {
            Stmt::Expr(e) => assert_eq!(e.span, Span::new(0, 5)),
        }
    }
}
```

- [ ] **Step 3: Update `src/interp.rs`** so `eval_expr` matches on `&expr.kind` and the undefined-variable error carries a span. Update the `use` line to `use crate::ast::{BinOp, Expr, ExprKind, Stmt, UnOp};` and replace `eval_expr`'s body:

```rust
    #[async_recursion(?Send)]
    pub async fn eval_expr(&mut self, expr: &Expr) -> Result<Value, AsError> {
        match &expr.kind {
            ExprKind::Number(n) => Ok(Value::Number(*n)),
            ExprKind::Str(s) => Ok(Value::Str(s.as_str().into())),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Nil => Ok(Value::Nil),
            ExprKind::Ident(name) => Err(AsError::at(
                format!("undefined variable '{}'", name),
                expr.span,
            )),
            ExprKind::Unary { op, expr: operand } => {
                let v = self.eval_expr(operand).await?;
                match (op, v) {
                    (UnOp::Neg, Value::Number(n)) => Ok(Value::Number(-n)),
                    (UnOp::Neg, _) => Err(AsError::new("cannot negate a non-number")),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
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
            ExprKind::Call { callee, args } => {
                let name = match &callee.kind {
                    ExprKind::Ident(n) => n.clone(),
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
```

- [ ] **Step 4: Run** `cargo test` (expect all prior tests + `binary_span_covers_both_operands`) and `cargo clippy --all-targets` (report, don't suppress, warnings).

- [ ] **Step 5: Commit**

```bash
git add src/ast.rs src/parser.rs src/interp.rs
git commit -m "refactor: add source spans to AST expression nodes"
```

---

## Task 2: Lexical environment (scope chain)

A standalone `Environment` type with unit tests; no interpreter wiring yet.

**Files:**
- Create: `src/env.rs`
- Modify: `src/lib.rs` (add `pub mod env;`, alphabetical)

- [ ] **Step 1: Create `src/env.rs`**

```rust
//! Lexical scope chain. `Environment` is a cheap-to-clone handle to a scope;
//! child scopes link to their parent so name lookup walks outward. Single
//! threaded, so `Rc<RefCell<…>>` (never `Arc`/`Mutex`).

use crate::value::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

struct Binding {
    value: Value,
    mutable: bool,
}

struct Scope {
    vars: HashMap<String, Binding>,
    parent: Option<Environment>,
}

/// A handle to a lexical scope. Cloning shares the same underlying scope.
#[derive(Clone)]
pub struct Environment(Rc<RefCell<Scope>>);

/// Why an assignment failed.
#[derive(Debug, PartialEq)]
pub enum AssignError {
    Undefined,
    Immutable,
}

impl Environment {
    /// Create a new, empty global scope.
    pub fn global() -> Self {
        Environment(Rc::new(RefCell::new(Scope { vars: HashMap::new(), parent: None })))
    }

    /// Create a new child scope whose parent is `self`.
    pub fn child(&self) -> Self {
        Environment(Rc::new(RefCell::new(Scope {
            vars: HashMap::new(),
            parent: Some(self.clone()),
        })))
    }

    /// Define a binding in THIS scope. Errors if the name is already bound here
    /// (shadowing an outer scope is allowed; redefining in the same scope is not).
    pub fn define(&self, name: &str, value: Value, mutable: bool) -> Result<(), String> {
        let mut scope = self.0.borrow_mut();
        if scope.vars.contains_key(name) {
            return Err(format!("'{}' is already defined in this scope", name));
        }
        scope.vars.insert(name.to_string(), Binding { value, mutable });
        Ok(())
    }

    /// Look up a name, walking outward through parent scopes.
    pub fn get(&self, name: &str) -> Option<Value> {
        let scope = self.0.borrow();
        if let Some(binding) = scope.vars.get(name) {
            return Some(binding.value.clone());
        }
        match &scope.parent {
            Some(parent) => parent.get(name),
            None => None,
        }
    }

    /// Assign to an EXISTING binding, walking outward. Errors if not found
    /// (Undefined) or the binding is immutable (Immutable).
    pub fn assign(&self, name: &str, value: Value) -> Result<(), AssignError> {
        let mut scope = self.0.borrow_mut();
        if let Some(binding) = scope.vars.get_mut(name) {
            if !binding.mutable {
                return Err(AssignError::Immutable);
            }
            binding.value = value;
            return Ok(());
        }
        match &scope.parent {
            Some(parent) => parent.assign(name, value),
            None => Err(AssignError::Undefined),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defines_and_gets() {
        let env = Environment::global();
        env.define("x", Value::Number(5.0), true).unwrap();
        assert!(matches!(env.get("x"), Some(Value::Number(n)) if n == 5.0));
    }

    #[test]
    fn redefining_in_same_scope_errors() {
        let env = Environment::global();
        env.define("x", Value::Number(1.0), true).unwrap();
        assert!(env.define("x", Value::Number(2.0), true).is_err());
    }

    #[test]
    fn child_reads_parent_but_can_shadow() {
        let parent = Environment::global();
        parent.define("x", Value::Number(1.0), true).unwrap();
        let child = parent.child();
        assert!(matches!(child.get("x"), Some(Value::Number(n)) if n == 1.0));
        child.define("x", Value::Number(9.0), true).unwrap();
        assert!(matches!(child.get("x"), Some(Value::Number(n)) if n == 9.0));
        assert!(matches!(parent.get("x"), Some(Value::Number(n)) if n == 1.0));
    }

    #[test]
    fn assign_walks_outward_and_respects_mutability() {
        let parent = Environment::global();
        parent.define("m", Value::Number(1.0), true).unwrap();
        parent.define("c", Value::Number(2.0), false).unwrap();
        let child = parent.child();
        child.assign("m", Value::Number(10.0)).unwrap();
        assert!(matches!(parent.get("m"), Some(Value::Number(n)) if n == 10.0));
        assert_eq!(child.assign("c", Value::Number(3.0)), Err(AssignError::Immutable));
        assert_eq!(child.assign("nope", Value::Nil), Err(AssignError::Undefined));
    }
}
```

- [ ] **Step 2: Update `src/lib.rs`** module declarations (alphabetical):

```rust
pub mod ast;
pub mod env;
pub mod error;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;
pub mod value;
```

(Leave the `use` lines and `run_source` below unchanged for now.)

- [ ] **Step 3: Run** `cargo test --lib env` (expect 4) + `cargo clippy --all-targets`. NOTE: these tests avoid `==` on `Value` (it gains `PartialEq` in Task 3), so they compile now.

- [ ] **Step 4: Commit**

```bash
git add src/env.rs src/lib.rs
git commit -m "feat: add lexical environment (scope chain) with define/get/assign"
```

---

## Task 3: Full value-operator set

Adds value truthiness/equality, the exponent operator `**`, comparison (`< <= > >=`), equality (`== !=`), short-circuit logical (`&& ||`), nil-coalescing (`??`), and unary `!`. Precedence (loosest→tightest): `??` → `||` → `&&` → equality → comparison → additive → multiplicative → exponent → unary → postfix.

**Files:** `src/value.rs`, `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/value.rs`** — derive `PartialEq`, add `is_truthy`.

Change the `Value` derive to `#[derive(Clone, Debug, PartialEq)]`. Add below the enum (above `Display`):

```rust
impl Value {
    /// Spec §4: only `nil` and `false` are falsy. Everything else
    /// (including `0` and `""`) is truthy.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}
```

Add tests:

```rust
    #[test]
    fn truthiness_follows_spec() {
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Number(0.0).is_truthy());
        assert!(Value::Str("".into()).is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Nil.is_truthy());
    }

    #[test]
    fn equality_is_structural_and_cross_kind_is_false() {
        assert_eq!(Value::Number(1.0), Value::Number(1.0));
        assert_eq!(Value::Str("a".into()), Value::Str("a".into()));
        assert_ne!(Value::Number(1.0), Value::Str("1".into()));
        assert_ne!(Value::Bool(true), Value::Number(1.0));
    }
```

- [ ] **Step 2: `src/token.rs`** — add these variants (before `Eof`):

```rust
    StarStar,
    Bang,
    BangEq,
    EqEq,
    Lt,
    Le,
    Gt,
    Ge,
    AmpAmp,
    PipePipe,
    QuestionQuestion,
```

- [ ] **Step 3: `src/lexer.rs`** — handle `**`, `!`/`!=`, `==` (bare `=` still errors until Task 4), `<`/`<=`, `>`/`>=`, `&&`, `||`, `??`. Replace the simple `'*'` push arm and add the new arms (place among the operator arms):

```rust
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    tokens.push(Token { tok: Tok::StarStar, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Star, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::BangEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Bang, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::EqEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    return Err(AsError::at(
                        "unexpected character '=' (plain assignment arrives with let/const)",
                        Span::new(start, start + 1),
                    ));
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::Le, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Lt, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::Ge, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Gt, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '&' => {
                if i + 1 < chars.len() && chars[i + 1] == '&' {
                    tokens.push(Token { tok: Tok::AmpAmp, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    return Err(AsError::at("unexpected character '&'", Span::new(start, start + 1)));
                }
            }
            '|' => {
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    tokens.push(Token { tok: Tok::PipePipe, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    return Err(AsError::at("unexpected character '|'", Span::new(start, start + 1)));
                }
            }
            '?' => {
                if i + 1 < chars.len() && chars[i + 1] == '?' {
                    tokens.push(Token { tok: Tok::QuestionQuestion, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    return Err(AsError::at(
                        "unexpected character '?' (the ?. and ? operators arrive in Milestone 3)",
                        Span::new(start, start + 1),
                    ));
                }
            }
```

Add lexer tests:

```rust
    #[test]
    fn lexes_multi_char_operators() {
        assert_eq!(
            kinds("a ** b == c != d <= e >= f && g || h ?? i"),
            vec![
                Tok::Ident("a".into()), Tok::StarStar, Tok::Ident("b".into()),
                Tok::EqEq, Tok::Ident("c".into()),
                Tok::BangEq, Tok::Ident("d".into()),
                Tok::Le, Tok::Ident("e".into()),
                Tok::Ge, Tok::Ident("f".into()),
                Tok::AmpAmp, Tok::Ident("g".into()),
                Tok::PipePipe, Tok::Ident("h".into()),
                Tok::QuestionQuestion, Tok::Ident("i".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn lexes_single_char_comparisons_and_bang() {
        assert_eq!(
            kinds("!a < b > c"),
            vec![
                Tok::Bang, Tok::Ident("a".into()),
                Tok::Lt, Tok::Ident("b".into()),
                Tok::Gt, Tok::Ident("c".into()),
                Tok::Eof,
            ]
        );
    }
```

- [ ] **Step 4: `src/ast.rs`** — extend operators.

`BinOp` becomes:

```rust
#[derive(Clone, Copy, Debug)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod, Pow,
    Lt, Le, Gt, Ge, Eq, Ne,
    And, Or, Coalesce,
}
```

`UnOp` becomes:

```rust
#[derive(Clone, Copy, Debug)]
pub enum UnOp {
    Neg,
    Not,
}
```

Extend the `BinOp` `Display` match with:

```rust
            BinOp::Pow => "**",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::Coalesce => "??",
```

Extend the `UnOp` `Display` match with `UnOp::Not => write!(f, "!"),`.

- [ ] **Step 5: `src/parser.rs`** — build the precedence ladder. Set `expr` to the loosest level and add the helper methods. Use a shared binary-fold helper to stay DRY:

```rust
    fn expr(&mut self) -> Result<Expr, AsError> {
        self.coalesce()
    }

    /// Build a left-associative binary node from an already-parsed left side.
    fn make_binary(left: Expr, op: BinOp, right: Expr) -> Expr {
        let span = Span::new(left.span.start, right.span.end);
        Expr { kind: ExprKind::Binary { op, lhs: Box::new(left), rhs: Box::new(right) }, span }
    }

    fn coalesce(&mut self) -> Result<Expr, AsError> {
        let mut left = self.logic_or()?;
        while *self.peek() == Tok::QuestionQuestion {
            self.advance();
            let right = self.logic_or()?;
            left = Self::make_binary(left, BinOp::Coalesce, right);
        }
        Ok(left)
    }

    fn logic_or(&mut self) -> Result<Expr, AsError> {
        let mut left = self.logic_and()?;
        while *self.peek() == Tok::PipePipe {
            self.advance();
            let right = self.logic_and()?;
            left = Self::make_binary(left, BinOp::Or, right);
        }
        Ok(left)
    }

    fn logic_and(&mut self) -> Result<Expr, AsError> {
        let mut left = self.equality()?;
        while *self.peek() == Tok::AmpAmp {
            self.advance();
            let right = self.equality()?;
            left = Self::make_binary(left, BinOp::And, right);
        }
        Ok(left)
    }

    fn equality(&mut self) -> Result<Expr, AsError> {
        let mut left = self.comparison()?;
        loop {
            let op = match self.peek() {
                Tok::EqEq => BinOp::Eq,
                Tok::BangEq => BinOp::Ne,
                _ => break,
            };
            self.advance();
            let right = self.comparison()?;
            left = Self::make_binary(left, op, right);
        }
        Ok(left)
    }

    fn comparison(&mut self) -> Result<Expr, AsError> {
        let mut left = self.additive()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.additive()?;
            left = Self::make_binary(left, op, right);
        }
        Ok(left)
    }
```

Change `additive` and `multiplicative` to use `make_binary` (replace their inline `Expr { kind: ExprKind::Binary … }` construction with `left = Self::make_binary(left, op, right);`). Change `multiplicative` to call `self.exponent()` instead of `self.unary()`, and add a right-associative `exponent` level that sits between multiplicative and unary:

```rust
    fn exponent(&mut self) -> Result<Expr, AsError> {
        let base = self.unary()?;
        if *self.peek() == Tok::StarStar {
            self.advance();
            // right-associative: 2 ** 3 ** 2 == 2 ** (3 ** 2)
            let exp = self.exponent()?;
            Ok(Self::make_binary(base, BinOp::Pow, exp))
        } else {
            Ok(base)
        }
    }
```

Update `unary` to also handle `!`:

```rust
    fn unary(&mut self) -> Result<Expr, AsError> {
        let start = self.span().start;
        let op = match self.peek() {
            Tok::Minus => Some(UnOp::Neg),
            Tok::Bang => Some(UnOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.unary()?;
            let span = Span::new(start, operand.span.end);
            return Ok(Expr {
                kind: ExprKind::Unary { op, expr: Box::new(operand) },
                span,
            });
        }
        self.postfix()
    }
```

Add parser tests:

```rust
    #[test]
    fn comparison_binds_looser_than_arithmetic() {
        assert_eq!(sexpr("1 + 2 < 3"), "(< (+ 1 2) 3)");
    }

    #[test]
    fn logical_and_binds_tighter_than_or() {
        assert_eq!(sexpr("a || b && c"), "(|| a (&& b c))");
    }

    #[test]
    fn coalesce_is_loosest() {
        assert_eq!(sexpr("a || b ?? c"), "(?? (|| a b) c)");
    }

    #[test]
    fn exponent_is_right_associative_and_tightest() {
        assert_eq!(sexpr("2 ** 3 ** 2"), "(** 2 (** 3 2))");
        assert_eq!(sexpr("2 * 3 ** 2"), "(* 2 (** 3 2))");
    }

    #[test]
    fn not_is_unary() {
        assert_eq!(sexpr("!a"), "(! a)");
    }
```

- [ ] **Step 6: `src/interp.rs`** — evaluate the new operators. Replace the `ExprKind::Unary` and `ExprKind::Binary` arms.

`Unary`:

```rust
            ExprKind::Unary { op, expr: operand } => {
                let v = self.eval_expr(operand).await?;
                match op {
                    UnOp::Neg => match v {
                        Value::Number(n) => Ok(Value::Number(-n)),
                        _ => Err(AsError::at("cannot negate a non-number", operand.span)),
                    },
                    UnOp::Not => Ok(Value::Bool(!v.is_truthy())),
                }
            }
```

`Binary` (short-circuit `&&`/`||`/`??` BEFORE evaluating the right side):

```rust
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinOp::And => {
                        let l = self.eval_expr(lhs).await?;
                        return if l.is_truthy() { self.eval_expr(rhs).await } else { Ok(l) };
                    }
                    BinOp::Or => {
                        let l = self.eval_expr(lhs).await?;
                        return if l.is_truthy() { Ok(l) } else { self.eval_expr(rhs).await };
                    }
                    BinOp::Coalesce => {
                        let l = self.eval_expr(lhs).await?;
                        return if l == Value::Nil { self.eval_expr(rhs).await } else { Ok(l) };
                    }
                    _ => {}
                }

                let l = self.eval_expr(lhs).await?;
                let r = self.eval_expr(rhs).await?;

                match op {
                    BinOp::Eq => return Ok(Value::Bool(l == r)),
                    BinOp::Ne => return Ok(Value::Bool(l != r)),
                    _ => {}
                }

                let (a, b) = match (&l, &r) {
                    (Value::Number(a), Value::Number(b)) => (*a, *b),
                    _ => return Err(AsError::at("operator requires two numbers", expr.span)),
                };
                let result = match op {
                    BinOp::Add => Value::Number(a + b),
                    BinOp::Sub => Value::Number(a - b),
                    BinOp::Mul => Value::Number(a * b),
                    BinOp::Div => Value::Number(a / b),
                    BinOp::Mod => Value::Number(a % b),
                    BinOp::Pow => Value::Number(a.powf(b)),
                    BinOp::Lt => Value::Bool(a < b),
                    BinOp::Le => Value::Bool(a <= b),
                    BinOp::Gt => Value::Bool(a > b),
                    BinOp::Ge => Value::Bool(a >= b),
                    BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or | BinOp::Coalesce => {
                        unreachable!("handled above")
                    }
                };
                Ok(result)
            }
```

Add interpreter tests:

```rust
    #[tokio::test]
    async fn comparison_and_equality() {
        assert_eq!(eval_to_value("1 < 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("2 == 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("1 != 2").await, Value::Bool(true));
        assert_eq!(eval_to_value("\"a\" == \"a\"").await, Value::Bool(true));
    }

    #[tokio::test]
    async fn exponent_evaluates() {
        assert_eq!(eval_to_value("2 ** 10").await, Value::Number(1024.0));
    }

    #[tokio::test]
    async fn short_circuit_and_coalesce() {
        assert_eq!(eval_to_value("false && nope").await, Value::Bool(false));
        assert_eq!(eval_to_value("true || nope").await, Value::Bool(true));
        assert_eq!(eval_to_value("nil ?? 5").await, Value::Number(5.0));
        assert_eq!(eval_to_value("3 ?? nope").await, Value::Number(3.0));
        assert_eq!(eval_to_value("!0").await, Value::Bool(false));
    }
```

- [ ] **Step 7: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 8: Commit**

```bash
git add src/value.rs src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add full value-operator set (** comparison equality logical ??)"
```

---

## Task 4: `let`/`const` declarations, identifier resolution, optional `;`

Adds statements beyond expression statements: `let`/`const`, the assignment token `=` (needed as the initializer separator), an optional `;` separator, statement-dispatch parsing, and wires the `Environment` through evaluation.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`, `src/lib.rs`.

- [ ] **Step 1: `src/token.rs`** — add (before `Eof`): `Eq, Semicolon, Let, Const,`.

- [ ] **Step 2: `src/lexer.rs`** — (a) change the `'='` arm (from Task 3, which errored on bare `=`) to emit `Tok::Eq` for the single-char case:

```rust
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::EqEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Eq, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
```

(b) Add a single-char `';'` arm with the other punctuation: `';' => push(&mut tokens, Tok::Semicolon, start, &mut i),`.

(c) Extend the keyword match: `"let" => Tok::Let, "const" => Tok::Const,`.

- [ ] **Step 3: `src/ast.rs`** — add the `Let` statement variant:

```rust
#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    Let { name: String, value: Expr, mutable: bool },
}
```

- [ ] **Step 4: `src/parser.rs`** — statement dispatch, optional-`;` skipping, and `let`/`const`. Replace `program` and add helpers:

```rust
    fn program(&mut self) -> Result<Vec<Stmt>, AsError> {
        let mut stmts = Vec::new();
        self.skip_semicolons();
        while *self.peek() != Tok::Eof {
            stmts.push(self.statement()?);
            self.skip_semicolons();
        }
        Ok(stmts)
    }

    /// `;` is an optional statement separator; consume any run of them.
    fn skip_semicolons(&mut self) {
        while *self.peek() == Tok::Semicolon {
            self.advance();
        }
    }

    fn statement(&mut self) -> Result<Stmt, AsError> {
        match self.peek() {
            Tok::Let => self.let_stmt(true),
            Tok::Const => self.let_stmt(false),
            _ => Ok(Stmt::Expr(self.expr()?)),
        }
    }

    fn let_stmt(&mut self, mutable: bool) -> Result<Stmt, AsError> {
        self.advance(); // consume `let` / `const`
        let name = match self.advance() {
            Tok::Ident(name) => name,
            other => {
                return Err(AsError::at(
                    format!("expected a variable name, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        self.eat(&Tok::Eq)?;
        let value = self.expr()?;
        Ok(Stmt::Let { name, value, mutable })
    }
```

- [ ] **Step 5: `src/interp.rs`** — thread the environment through evaluation. Add `use crate::env::Environment;`. Confirm the ast `use` reads `use crate::ast::{BinOp, Expr, ExprKind, Stmt, UnOp};`.

Replace `exec` and add `exec_stmt`:

```rust
    pub async fn exec(&mut self, program: &[Stmt], env: &Environment) -> Result<(), AsError> {
        for stmt in program {
            self.exec_stmt(stmt, env).await?;
        }
        Ok(())
    }

    async fn exec_stmt(&mut self, stmt: &Stmt, env: &Environment) -> Result<(), AsError> {
        match stmt {
            Stmt::Expr(e) => {
                self.eval_expr(e, env).await?;
            }
            Stmt::Let { name, value, mutable } => {
                let v = self.eval_expr(value, env).await?;
                env.define(name, v, *mutable).map_err(AsError::new)?;
            }
        }
        Ok(())
    }
```

Change `eval_expr` to take `&Environment` and resolve `Ident` via it (this supersedes Task 1's `eval_expr`):

```rust
    #[async_recursion(?Send)]
    pub async fn eval_expr(&mut self, expr: &Expr, env: &Environment) -> Result<Value, AsError> {
        match &expr.kind {
            ExprKind::Number(n) => Ok(Value::Number(*n)),
            ExprKind::Str(s) => Ok(Value::Str(s.as_str().into())),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Nil => Ok(Value::Nil),
            ExprKind::Ident(name) => env
                .get(name)
                .ok_or_else(|| AsError::at(format!("undefined variable '{}'", name), expr.span)),
            ExprKind::Unary { op, expr: operand } => {
                let v = self.eval_expr(operand, env).await?;
                match op {
                    UnOp::Neg => match v {
                        Value::Number(n) => Ok(Value::Number(-n)),
                        _ => Err(AsError::at("cannot negate a non-number", operand.span)),
                    },
                    UnOp::Not => Ok(Value::Bool(!v.is_truthy())),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                match op {
                    BinOp::And => {
                        let l = self.eval_expr(lhs, env).await?;
                        return if l.is_truthy() { self.eval_expr(rhs, env).await } else { Ok(l) };
                    }
                    BinOp::Or => {
                        let l = self.eval_expr(lhs, env).await?;
                        return if l.is_truthy() { Ok(l) } else { self.eval_expr(rhs, env).await };
                    }
                    BinOp::Coalesce => {
                        let l = self.eval_expr(lhs, env).await?;
                        return if l == Value::Nil { self.eval_expr(rhs, env).await } else { Ok(l) };
                    }
                    _ => {}
                }

                let l = self.eval_expr(lhs, env).await?;
                let r = self.eval_expr(rhs, env).await?;

                match op {
                    BinOp::Eq => return Ok(Value::Bool(l == r)),
                    BinOp::Ne => return Ok(Value::Bool(l != r)),
                    _ => {}
                }

                let (a, b) = match (&l, &r) {
                    (Value::Number(a), Value::Number(b)) => (*a, *b),
                    _ => return Err(AsError::at("operator requires two numbers", expr.span)),
                };
                let result = match op {
                    BinOp::Add => Value::Number(a + b),
                    BinOp::Sub => Value::Number(a - b),
                    BinOp::Mul => Value::Number(a * b),
                    BinOp::Div => Value::Number(a / b),
                    BinOp::Mod => Value::Number(a % b),
                    BinOp::Pow => Value::Number(a.powf(b)),
                    BinOp::Lt => Value::Bool(a < b),
                    BinOp::Le => Value::Bool(a <= b),
                    BinOp::Gt => Value::Bool(a > b),
                    BinOp::Ge => Value::Bool(a >= b),
                    BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or | BinOp::Coalesce => {
                        unreachable!("handled above")
                    }
                };
                Ok(result)
            }
            ExprKind::Call { callee, args } => {
                let name = match &callee.kind {
                    ExprKind::Ident(n) => n.clone(),
                    _ => return Err(AsError::new("only named builtins are callable in the skeleton")),
                };
                let mut values = Vec::new();
                for a in args {
                    values.push(self.eval_expr(a, env).await?);
                }
                self.call_builtin(&name, &values)
            }
        }
    }
```

Update the interpreter test module: add `use crate::env::Environment;`, replace the `eval_to_value` helper, and update the two `exec`-based tests to pass an env. New helper:

```rust
    use crate::env::Environment;

    async fn eval_to_value(src: &str) -> Value {
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        let (last, rest) = stmts.split_last().expect("at least one statement");
        interp.exec(rest, &env).await.unwrap();
        match last {
            Stmt::Expr(e) => interp.eval_expr(e, &env).await.unwrap(),
            _ => panic!("last statement must be an expression"),
        }
    }
```

Updated `print`/non-builtin tests:

```rust
    #[tokio::test]
    async fn print_writes_to_the_output_buffer() {
        let stmts = parse(&lex("print(1 + 2 * 3)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "7\n");
    }

    #[tokio::test]
    async fn calling_a_non_builtin_is_an_error() {
        let stmts = parse(&lex("nope(1)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        let err = interp.exec(&stmts, &env).await.unwrap_err();
        assert!(err.message.contains("is not a function"));
    }
```

New tests:

```rust
    #[tokio::test]
    async fn let_binding_resolves() {
        let stmts = parse(&lex("let x = 5\nprint(x + 1)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "6\n");
    }

    #[tokio::test]
    async fn undefined_variable_errors_with_span() {
        let stmts = parse(&lex("print(missing)").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        let err = interp.exec(&stmts, &env).await.unwrap_err();
        assert!(err.message.contains("undefined variable 'missing'"));
        assert!(err.span.is_some());
    }

    #[tokio::test]
    async fn optional_semicolons_are_accepted() {
        let stmts = parse(&lex("let x = 1; print(x);").unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "1\n");
    }
```

- [ ] **Step 6: `src/lib.rs`** — `run_source` creates a global environment:

```rust
use crate::env::Environment;
use crate::error::AsError;
use crate::interp::Interp;

/// Lex → parse → evaluate in a fresh global environment. Returns captured output.
pub async fn run_source(src: &str) -> Result<String, AsError> {
    let tokens = lexer::lex(src)?;
    let program = parser::parse(&tokens)?;
    let mut interp = Interp::new();
    let env = Environment::global();
    interp.exec(&program, &env).await?;
    Ok(interp.output)
}
```

- [ ] **Step 7: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 8: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs src/lib.rs
git commit -m "feat: add let/const, identifier resolution, and optional semicolons"
```

---

## Task 5: Assignment + compound assignment

Add `=` assignment (lowest precedence, right-associative) and compound forms `+= -= *= /=` (desugared to `x = x <op> e`), with mutability enforcement.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add (before `Eof`): `PlusEq, MinusEq, StarEq, SlashEq,`.

- [ ] **Step 2: `src/lexer.rs`** — make `+ - / ` two-char-aware (and `*` already handles `**`; add `*=`). Replace the `'+'`, `'-'`, `'/'` push arms and the `'*'` arm:

```rust
            '+' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::PlusEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Plus, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '-' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::MinusEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Minus, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '/' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::SlashEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Slash, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    tokens.push(Token { tok: Tok::StarStar, span: Span::new(start, start + 2) });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::StarEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Star, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
```

Add a lexer test:

```rust
    #[test]
    fn lexes_compound_assignment_operators() {
        assert_eq!(
            kinds("a += b -= c *= d /= e"),
            vec![
                Tok::Ident("a".into()), Tok::PlusEq, Tok::Ident("b".into()),
                Tok::MinusEq, Tok::Ident("c".into()),
                Tok::StarEq, Tok::Ident("d".into()),
                Tok::SlashEq, Tok::Ident("e".into()),
                Tok::Eof,
            ]
        );
    }
```

- [ ] **Step 3: `src/ast.rs`** — add the `Assign` variant to `ExprKind`:

```rust
    Assign { name: String, value: Box<Expr> },
```

Add its `Display` arm: `ExprKind::Assign { name, value } => write!(f, "(= {} {})", name, value),`.

- [ ] **Step 4: `src/parser.rs`** — add assignment as the lowest precedence (calling `coalesce` for its target). Set `expr` to `self.assignment()` and add:

```rust
    fn expr(&mut self) -> Result<Expr, AsError> {
        self.assignment()
    }

    fn assignment(&mut self) -> Result<Expr, AsError> {
        let target = self.coalesce()?;

        // Map a compound-assignment token to the binary op it desugars to.
        let compound = match self.peek() {
            Tok::Eq => None,
            Tok::PlusEq => Some(BinOp::Add),
            Tok::MinusEq => Some(BinOp::Sub),
            Tok::StarEq => Some(BinOp::Mul),
            Tok::SlashEq => Some(BinOp::Div),
            _ => return Ok(target),
        };
        self.advance(); // consume the assignment operator
        let value = self.assignment()?; // right-associative

        let name = match &target.kind {
            ExprKind::Ident(name) => name.clone(),
            _ => return Err(AsError::at("invalid assignment target", target.span)),
        };

        // For `x += e`, desugar the value to `x + e` (a fresh Binary over the
        // target identifier and the rhs), preserving spans for diagnostics.
        let span = Span::new(target.span.start, value.span.end);
        let value = match compound {
            None => value,
            Some(op) => {
                let target_ident = Expr {
                    kind: ExprKind::Ident(name.clone()),
                    span: target.span,
                };
                Self::make_binary(target_ident, op, value)
            }
        };

        Ok(Expr {
            kind: ExprKind::Assign { name, value: Box::new(value) },
            span,
        })
    }
```

Add parser tests:

```rust
    #[test]
    fn parses_assignment() {
        assert_eq!(sexpr("x = 5"), "(= x 5)");
    }

    #[test]
    fn assignment_is_right_associative() {
        assert_eq!(sexpr("x = y = 1"), "(= x (= y 1))");
    }

    #[test]
    fn compound_assignment_desugars() {
        assert_eq!(sexpr("x += 2"), "(= x (+ x 2))");
    }
```

- [ ] **Step 5: `src/interp.rs`** — evaluate assignment. Change the env import to `use crate::env::{AssignError, Environment};` and add an `ExprKind::Assign` arm to `eval_expr` (place near `Ident`):

```rust
            ExprKind::Assign { name, value } => {
                let v = self.eval_expr(value, env).await?;
                match env.assign(name, v.clone()) {
                    Ok(()) => Ok(v),
                    Err(AssignError::Undefined) => Err(AsError::at(
                        format!("cannot assign to undefined variable '{}'", name),
                        expr.span,
                    )),
                    Err(AssignError::Immutable) => Err(AsError::at(
                        format!("cannot assign to immutable binding '{}'", name),
                        expr.span,
                    )),
                }
            }
```

Add interpreter tests:

```rust
    #[tokio::test]
    async fn assignment_updates_a_mutable_binding() {
        let src = "let x = 1\nx = x + 4\nprint(x)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");
    }

    #[tokio::test]
    async fn compound_assignment_runs() {
        let src = "let x = 10\nx *= 3\nprint(x)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "30\n");
    }

    #[tokio::test]
    async fn assigning_to_const_errors() {
        let src = "const x = 1\nx = 2";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        let err = interp.exec(&stmts, &env).await.unwrap_err();
        assert!(err.message.contains("immutable"));
    }
```

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add assignment and compound assignment with mutability checks"
```

---

## Task 6: Block statements and `if`/`else`

Add `{ … }` blocks (nested scope) and `if (cond) { … } else { … }` (incl. `else if`).

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add (before `Eof`): `LBrace, RBrace, If, Else,`.

- [ ] **Step 2: `src/lexer.rs`** — add brace arms with the punctuation: `'{' => push(&mut tokens, Tok::LBrace, start, &mut i),` and `'}' => push(&mut tokens, Tok::RBrace, start, &mut i),`. Extend the keyword match with `"if" => Tok::If, "else" => Tok::Else,`.

- [ ] **Step 3: `src/ast.rs`** — extend `Stmt`:

```rust
#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    Let { name: String, value: Expr, mutable: bool },
    Block(Vec<Stmt>),
    If { cond: Expr, then_branch: Vec<Stmt>, else_branch: Option<Vec<Stmt>> },
}
```

- [ ] **Step 4: `src/parser.rs`** — extend `statement` dispatch and add `block`/`if_stmt`:

```rust
    fn statement(&mut self) -> Result<Stmt, AsError> {
        match self.peek() {
            Tok::Let => self.let_stmt(true),
            Tok::Const => self.let_stmt(false),
            Tok::LBrace => Ok(Stmt::Block(self.block()?)),
            Tok::If => self.if_stmt(),
            _ => Ok(Stmt::Expr(self.expr()?)),
        }
    }

    /// Parse `{ stmt* }` (with optional `;` separators) and return the inner statements.
    fn block(&mut self) -> Result<Vec<Stmt>, AsError> {
        self.eat(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        self.skip_semicolons();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            stmts.push(self.statement()?);
            self.skip_semicolons();
        }
        self.eat(&Tok::RBrace)?;
        Ok(stmts)
    }

    fn if_stmt(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::If)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        let then_branch = self.block()?;
        let else_branch = if *self.peek() == Tok::Else {
            self.advance();
            if *self.peek() == Tok::If {
                Some(vec![self.if_stmt()?]) // `else if`
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        Ok(Stmt::If { cond, then_branch, else_branch })
    }
```

- [ ] **Step 5: `src/interp.rs`** — add `Block` and `If` arms to `exec_stmt`:

```rust
            Stmt::Block(stmts) => {
                let child = env.child();
                self.exec(stmts, &child).await?;
            }
            Stmt::If { cond, then_branch, else_branch } => {
                if self.eval_expr(cond, env).await?.is_truthy() {
                    let child = env.child();
                    self.exec(then_branch, &child).await?;
                } else if let Some(else_stmts) = else_branch {
                    let child = env.child();
                    self.exec(else_stmts, &child).await?;
                }
            }
```

Add tests:

```rust
    #[tokio::test]
    async fn if_else_chooses_branch() {
        let src = "let x = 3\nif (x < 5) { print(\"small\") } else { print(\"big\") }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "small\n");
    }

    #[tokio::test]
    async fn else_if_chain() {
        let src = "let x = 7\nif (x < 5) { print(\"a\") } else if (x < 10) { print(\"b\") } else { print(\"c\") }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "b\n");
    }

    #[tokio::test]
    async fn block_scope_does_not_leak() {
        let src = "{ let y = 1 }\nprint(y)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        let err = interp.exec(&stmts, &env).await.unwrap_err();
        assert!(err.message.contains("undefined variable 'y'"));
    }
```

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add block statements and if/else (with else-if)"
```

---

## Task 7: `while` loops

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `While,` before `Eof`.
- [ ] **Step 2: `src/lexer.rs`** — extend keyword match with `"while" => Tok::While,`.
- [ ] **Step 3: `src/ast.rs`** — add to `Stmt`: `While { cond: Expr, body: Vec<Stmt> },`.
- [ ] **Step 4: `src/parser.rs`** — add `Tok::While => self.while_stmt(),` to `statement`, and:

```rust
    fn while_stmt(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::While)?;
        self.eat(&Tok::LParen)?;
        let cond = self.expr()?;
        self.eat(&Tok::RParen)?;
        let body = self.block()?;
        Ok(Stmt::While { cond, body })
    }
```

- [ ] **Step 5: `src/interp.rs`** — add a `Stmt::While` arm to `exec_stmt`:

```rust
            Stmt::While { cond, body } => {
                while self.eval_expr(cond, env).await?.is_truthy() {
                    let child = env.child();
                    self.exec(body, &child).await?;
                }
            }
```

Add a test:

```rust
    #[tokio::test]
    async fn while_loop_accumulates() {
        let src = "let i = 1\nlet sum = 0\nwhile (i <= 5) { sum += i\ni += 1 }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "15\n");
    }
```

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add while loops"
```

---

## Task 8: `for (i in a..b)` numeric-range loops

Adds the range-based `for` loop (spec §3 `for (i in 0..n)`). Needs only numbers — the `for (x of iterable)` form is deferred (see Deferrals table) because it requires arrays. The loop variable is bound fresh in a child scope each iteration; the range is half-open `[start, end)`.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add (before `Eof`): `DotDot, For, In,`.

- [ ] **Step 2: `src/lexer.rs`** — add a `'.'` arm (it must be `..`; a lone `.` is reserved for member access in M3):

```rust
            '.' => {
                if i + 1 < chars.len() && chars[i + 1] == '.' {
                    tokens.push(Token { tok: Tok::DotDot, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    return Err(AsError::at(
                        "unexpected character '.' (member access arrives in Milestone 3)",
                        Span::new(start, start + 1),
                    ));
                }
            }
```

Extend the keyword match with `"for" => Tok::For, "in" => Tok::In,`.

- [ ] **Step 3: `src/ast.rs`** — add to `Stmt`:

```rust
    ForRange { var: String, start: Expr, end: Expr, body: Vec<Stmt> },
```

- [ ] **Step 4: `src/parser.rs`** — add `Tok::For => self.for_stmt(),` to `statement`, and:

```rust
    fn for_stmt(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::For)?;
        self.eat(&Tok::LParen)?;
        let var = match self.advance() {
            Tok::Ident(name) => name,
            other => {
                return Err(AsError::at(
                    format!("expected a loop variable name, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        self.eat(&Tok::In)?;
        let start = self.expr()?;
        self.eat(&Tok::DotDot)?;
        let end = self.expr()?;
        self.eat(&Tok::RParen)?;
        let body = self.block()?;
        Ok(Stmt::ForRange { var, start, end, body })
    }
```

- [ ] **Step 5: `src/interp.rs`** — add a `Stmt::ForRange` arm to `exec_stmt`. Both bounds must evaluate to numbers; iterate integer steps over `[start, end)`:

```rust
            Stmt::ForRange { var, start, end, body } => {
                let start_v = self.eval_expr(start, env).await?;
                let end_v = self.eval_expr(end, env).await?;
                let (lo, hi) = match (start_v, end_v) {
                    (Value::Number(a), Value::Number(b)) => (a, b),
                    _ => return Err(AsError::at("for-range bounds must be numbers", start.span)),
                };
                let mut i = lo;
                while i < hi {
                    let child = env.child();
                    child.define(var, Value::Number(i), false).map_err(AsError::new)?;
                    self.exec(body, &child).await?;
                    i += 1.0;
                }
            }
```

Add tests:

```rust
    #[tokio::test]
    async fn for_range_iterates_half_open() {
        let src = "let sum = 0\nfor (i in 0..5) { sum += i }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        // 0 + 1 + 2 + 3 + 4
        assert_eq!(interp.output, "10\n");
    }

    #[tokio::test]
    async fn for_range_loop_var_is_scoped_per_iteration() {
        let src = "for (i in 0..3) { print(i) }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "0\n1\n2\n");
    }
```

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add for-range loops (for i in a..b)"
```

---

## Task 9: End-to-end demo program + CLI integration test

Prove the milestone works through the real binary with a program using variables, a loop, a conditional, and an operator.

**Files:** `examples/factorial.as` (new), `tests/cli.rs` (modify).

- [ ] **Step 1: Create `examples/factorial.as`**

```
let n = 5
let result = 1
for (i in 1..6) {
  result *= i
}
if (result > 100) {
  print("big")
} else {
  print("small")
}
print(result)
```

- [ ] **Step 2: Add an integration test to `tests/cli.rs`**

```rust
#[test]
fn runs_factorial_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/factorial.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // 1*2*3*4*5 = 120, which is > 100, then the value itself.
    assert_eq!(String::from_utf8_lossy(&output.stdout), "big\n120\n");
}
```

NOTE: integration tests run with the crate root as the working directory, so the relative path `examples/factorial.as` resolves. (The other tests in this file use an absolute temp path; this one uses the committed example deliberately.)

- [ ] **Step 3: Run** `cargo test` (incl. `runs_factorial_example`), then manually:

Run: `cargo run --quiet -- run examples/factorial.as`
Expected:
```
big
120
```
Paste the actual output. Also run `cargo clippy --all-targets`.

- [ ] **Step 4: Commit**

```bash
git add examples/factorial.as tests/cli.rs
git commit -m "test: add factorial end-to-end example (variables, for-range, if)"
```

---

## Definition of Done

- `cargo test` passes (all unit + integration tests); `cargo clippy --all-targets` is clean.
- `cargo run -- run examples/factorial.as` prints `big` then `120`.
- AScript now supports: `let`/`const`, lexical-scoped identifier resolution, the full value-operator set (`+ - * / % **`, `< <= > >= == !=`, `&& || !`, `??`), assignment + compound assignment with mutability enforcement, optional `;` separators, `{ }` blocks, `if`/`else`/`else if`, `while`, and `for (i in a..b)`.
- Runtime errors (undefined variable, bad assignment target, non-numeric operands, non-numeric range bounds) carry source spans.
- Everything in-theme from the spec is implemented; the only omissions are the dependency-blocked features enumerated in **Scope & Justified Deferrals**, each tagged with its owning milestone.

## Hand-off to Milestone 3 ("Functions & Data")

Unblocks the deferred items: user-defined functions (`fn` + closures capturing an `Environment`, `return`), arrays `[…]` / objects `{…}` / maps (new `Value` kinds), member access `.` / indexing `[]` / optional chaining `?.`, the `for (x of iterable)` form, template strings, and the `?` Result-propagation operator. Extension points prepared here: the `Environment` (functions capture it), spans on every expr (good diagnostics for the larger surface), the operator/precedence ladder (member/index slot into `postfix`), and `call_builtin`'s name-`match` — which M3 generalizes into evaluating the callee to a callable `Value` (per the M1 final-review note).
```
