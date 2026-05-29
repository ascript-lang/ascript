# AScript Milestone 3 — Functions & Control-Flow Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Add first-class functions to AScript: a control-flow `Flow` signal (`return`/`break`/`continue`), `fn` declarations with lexical closures, arrow-function expressions, and a generalized call mechanism that evaluates the callee to a callable `Value`. After this milestone, recursive functions, higher-order functions, and closures all work.

**Architecture:** Builds on M2. The interpreter's `exec`/`exec_stmt` change from returning `()` to returning a `Flow` enum so non-local control flow (`return` out of a function, `break`/`continue` out of a loop) propagates cleanly. Functions become a new `Value` kind capturing their defining `Environment` (closures). Builtins (`print`) become real bindings (`Value::Builtin`) resolved through the environment, so the call path is uniform: evaluate the callee expression → get a callable `Value` → invoke it.

**Tech Stack:** Rust 2021, tokio (current_thread), async-recursion. No new crates.

**Starting state (end of M2, on `main`):** Working interpreter with variables, operators, assignment, blocks, `if/else`, `while`, `for (i in a..b)`. `Value` = `Nil|Bool|Number|Str`. `print` is dispatched by callee-name in the `Call` arm (NOT a binding). 44 lib + 3 integration tests.

**Conventions:** spans are char offsets; single-threaded `Rc`/`RefCell`; statements delimited structurally with optional `;`; `eval_expr`/`exec`/`exec_stmt` are `#[async_recursion(?Send)]`.

## Scope & Justified Deferrals

Implements everything in spec §3 about functions/control flow. Deferred (dependency-blocked), with owning milestone:

| Deferred | Why | Milestone |
|---|---|---|
| Arrays/objects/maps, member access, indexing, `for-of`, template strings | Need new heap value kinds | **M4 — Data structures** |
| `?` Result operator, Result/panic tiers, `recover` | Need the error-model design | **M5 — Result & error model** |
| Type annotations on params/returns | Gradual-contract layer | **M6** |

Note: `break`/`continue` are not in the spec's terse grammar but are table-stakes
production loop control and fall out of the `Flow` signal; included here.

---

## Task 1: Control-flow signal — `return` / `break` / `continue`

Change `exec`/`exec_stmt` to return `Flow`; add the three control statements. `return` errors nowhere yet (functions arrive in Task 3) but works at top level (ends program); `break`/`continue` work in loops and error if they escape to the top level.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`, `src/lib.rs`.

- [ ] **Step 1: `src/token.rs`** — add (before `Eof`): `Return, Break, Continue,`.

- [ ] **Step 2: `src/lexer.rs`** — extend the keyword match with:

```rust
                    "return" => Tok::Return,
                    "break" => Tok::Break,
                    "continue" => Tok::Continue,
```

- [ ] **Step 3: `src/ast.rs`** — add to `Stmt`:

```rust
    Return(Option<Expr>),
    Break,
    Continue,
```

- [ ] **Step 4: `src/parser.rs`** — add dispatch + `return_stmt`. In `statement`:

```rust
            Tok::Return => self.return_stmt(),
            Tok::Break => {
                self.advance();
                Ok(Stmt::Break)
            }
            Tok::Continue => {
                self.advance();
                Ok(Stmt::Continue)
            }
```

Add the helper:

```rust
    fn return_stmt(&mut self) -> Result<Stmt, AsError> {
        self.advance(); // consume `return`
        // No value if the next token cannot begin an expression in this position.
        match self.peek() {
            Tok::RBrace | Tok::Eof | Tok::Semicolon => Ok(Stmt::Return(None)),
            _ => {
                let value = self.expr()?;
                Ok(Stmt::Return(Some(value)))
            }
        }
    }
```

- [ ] **Step 5: `src/interp.rs`** — add the `Flow` enum and rework `exec`/`exec_stmt`.

Add near the top (after the `use`s):

```rust
/// Non-local control-flow signal produced while executing statements.
pub enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}
```

Replace `exec` and `exec_stmt` (keep `#[async_recursion(?Send)]` on both):

```rust
    #[async_recursion(?Send)]
    pub async fn exec(&mut self, program: &[Stmt], env: &Environment) -> Result<Flow, AsError> {
        for stmt in program {
            match self.exec_stmt(stmt, env).await? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    #[async_recursion(?Send)]
    async fn exec_stmt(&mut self, stmt: &Stmt, env: &Environment) -> Result<Flow, AsError> {
        match stmt {
            Stmt::Expr(e) => {
                self.eval_expr(e, env).await?;
                Ok(Flow::Normal)
            }
            Stmt::Let { name, value, mutable } => {
                let v = self.eval_expr(value, env).await?;
                env.define(name, v, *mutable).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
            Stmt::Block(stmts) => {
                let child = env.child();
                self.exec(stmts, &child).await
            }
            Stmt::If { cond, then_branch, else_branch } => {
                if self.eval_expr(cond, env).await?.is_truthy() {
                    let child = env.child();
                    self.exec(then_branch, &child).await
                } else if let Some(else_stmts) = else_branch {
                    let child = env.child();
                    self.exec(else_stmts, &child).await
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::While { cond, body } => {
                while self.eval_expr(cond, env).await?.is_truthy() {
                    let child = env.child();
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                }
                Ok(Flow::Normal)
            }
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
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                    i += 1.0;
                }
                Ok(Flow::Normal)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval_expr(e, env).await?,
                    None => Value::Nil,
                };
                Ok(Flow::Return(v))
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
        }
    }
```

- [ ] **Step 6: `src/lib.rs`** — `run_source` handles the top-level flow. Replace the body of `run_source` after `interp.exec`:

```rust
pub async fn run_source(src: &str) -> Result<String, AsError> {
    let tokens = lexer::lex(src)?;
    let program = parser::parse(&tokens)?;
    let mut interp = Interp::new();
    let env = Environment::global();
    match interp.exec(&program, &env).await? {
        crate::interp::Flow::Break => return Err(AsError::new("'break' outside of a loop")),
        crate::interp::Flow::Continue => return Err(AsError::new("'continue' outside of a loop")),
        crate::interp::Flow::Normal | crate::interp::Flow::Return(_) => {}
    }
    Ok(interp.output)
}
```

- [ ] **Step 7: update the interp test helper** — `eval_to_value` currently does `interp.exec(rest, &env).await.unwrap();`. `exec` now returns `Flow`; the `.unwrap()` yields a `Flow` which is ignored — this still compiles and works. No change needed unless the compiler warns about an unused `Flow`; if so, bind it to `let _ = ...`. Verify and adjust minimally.

Add interpreter tests:

```rust
    #[tokio::test]
    async fn break_exits_loop_early() {
        let src = "let sum = 0\nfor (i in 0..10) { if (i == 5) { break }\nsum += i }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "10\n"); // 0+1+2+3+4
    }

    #[tokio::test]
    async fn continue_skips_iteration() {
        let src = "let sum = 0\nfor (i in 0..5) { if (i == 2) { continue }\nsum += i }\nprint(sum)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "8\n"); // 0+1+3+4
    }

    #[tokio::test]
    async fn break_in_while() {
        let src = "let i = 0\nwhile (true) { if (i >= 3) { break }\ni += 1 }\nprint(i)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = Environment::global();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "3\n");
    }
```

Also add a `tests/cli.rs`-independent check for the top-level error via `run_source` — add to the interp tests:

```rust
    #[tokio::test]
    async fn break_outside_loop_errors_at_top_level() {
        let err = crate::run_source("break").await.unwrap_err();
        assert!(err.message.contains("outside of a loop"));
    }
```

NOTE: the parser test helper `sexpr` and the `binary_span_covers_both_operands` test already have `_ => panic!(...)` arms for non-`Expr` statements (added in M2), so adding new `Stmt` variants does not break them.

- [ ] **Step 8: Run** `cargo test` (all pass + new tests) and `cargo clippy --all-targets` (clean; fix properly). 

- [ ] **Step 9: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs src/lib.rs
git commit -m "feat: add control-flow signal with return, break, and continue"
```

---

## Task 2: Callable values — `Value::Builtin` and generalized call dispatch

Make `print` a real binding (`Value::Builtin`) in the global environment, and change the `Call` arm to evaluate the callee expression to a `Value` and dispatch on it. This is the seam user functions plug into next.

**Files:** `src/value.rs`, `src/interp.rs`, `src/lib.rs`.

- [ ] **Step 1: `src/value.rs`** — add a `Builtin` variant and switch to a manual `PartialEq`/`Debug` (a derived `Debug` would later recurse through closures; do it now).

Replace the `Value` enum + derives with:

```rust
use std::fmt;
use std::rc::Rc;

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    Str(Rc<str>),
    /// A native built-in function, dispatched by name in the interpreter.
    Builtin(Rc<str>),
}

impl Value {
    /// Spec §4: only `nil` and `false` are falsy.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            // Built-ins are equal iff they name the same function.
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "Nil"),
            Value::Bool(b) => write!(f, "Bool({})", b),
            Value::Number(n) => write!(f, "Number({})", n),
            Value::Str(s) => write!(f, "Str({:?})", s),
            Value::Builtin(name) => write!(f, "Builtin({:?})", name),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Number(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{}", s),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
        }
    }
}
```

Keep the existing `#[cfg(test)] mod tests` (truthiness/equality/display tests still hold). Add:

```rust
    #[test]
    fn builtins_compare_by_name_and_are_truthy() {
        assert_eq!(Value::Builtin("print".into()), Value::Builtin("print".into()));
        assert_ne!(Value::Builtin("print".into()), Value::Builtin("len".into()));
        assert!(Value::Builtin("print".into()).is_truthy());
        assert_eq!(Value::Builtin("print".into()).to_string(), "<builtin print>");
    }
```

- [ ] **Step 2: `src/interp.rs`** — add a `global_env()` constructor that installs builtins, and generalize the `Call` arm.

Add a free function (after the `Flow` enum):

```rust
/// A fresh global environment with the built-in functions installed.
pub fn global_env() -> Environment {
    let env = Environment::global();
    env.define("print", Value::Builtin("print".into()), false)
        .expect("global env starts empty");
    env
}
```

Replace the `ExprKind::Call` arm of `eval_expr` so it evaluates the callee to a `Value`:

```rust
            ExprKind::Call { callee, args } => {
                let callee_v = self.eval_expr(callee, env).await?;
                let mut values = Vec::new();
                for a in args {
                    values.push(self.eval_expr(a, env).await?);
                }
                match callee_v {
                    Value::Builtin(name) => self.call_builtin(&name, &values, expr.span),
                    _ => Err(AsError::at("value is not callable", callee.span)),
                }
            }
```

`call_builtin` keeps its current signature `(&mut self, name: &str, args: &[Value], span: Span)` from the M2 polish fix. Confirm it still maps an unknown name to a span-carrying "is not a function" error — leave it as is.

- [ ] **Step 3: `src/interp.rs` tests** — `print` is now resolved via the environment, so every interp test that runs a program calling `print` must use `global_env()` instead of `Environment::global()`.

Replace the `eval_to_value` helper and update each `#[tokio::test]` that constructs `Environment::global()` AND calls `print` (or relies on builtins) to use `global_env()` instead. Helper:

```rust
    async fn eval_to_value(src: &str) -> Value {
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let (last, rest) = stmts.split_last().expect("at least one statement");
        interp.exec(rest, &env).await.unwrap();
        match last {
            Stmt::Expr(e) => interp.eval_expr(e, &env).await.unwrap(),
            _ => panic!("last statement must be an expression"),
        }
    }
```

For the program-style tests (`print_writes_to_the_output_buffer`, `let_binding_resolves`, `optional_semicolons_are_accepted`, `assignment_updates_a_mutable_binding`, `compound_assignment_runs`, `if_else_chooses_branch`, `else_if_chain`, `while_loop_accumulates`, `for_range_*`, `break_*`, `continue_*`, etc.) replace `let env = Environment::global();` with `let env = global_env();`. The tests that deliberately do NOT use builtins and check errors (`undefined_variable_errors_with_span`, `calling_a_non_builtin_is_an_error`, `assigning_to_const_errors`, `block_scope_does_not_leak`, `call_site_errors_carry_a_span`) can keep `Environment::global()`, but using `global_env()` everywhere is simpler and harmless — prefer `global_env()` uniformly EXCEPT where a test asserts an undefined-variable error on a name that must NOT be defined.

Add a test that `print` resolves as a value:

```rust
    #[tokio::test]
    async fn print_is_a_resolvable_builtin_value() {
        assert_eq!(eval_to_value("print").await, Value::Builtin("print".into()));
    }
```

- [ ] **Step 4: `src/lib.rs`** — `run_source` must use `global_env()` so `print` is bound:

Change `let env = Environment::global();` to `let env = crate::interp::global_env();` in `run_source`.

- [ ] **Step 5: Run** `cargo test` (all pass) and `cargo clippy --all-targets` (clean).

- [ ] **Step 6: Commit**

```bash
git add src/value.rs src/interp.rs src/lib.rs
git commit -m "feat: make builtins first-class Values and generalize call dispatch"
```

---

## Task 3: User functions (`fn` declarations, closures, `return`, recursion)

Add `fn name(params) { body }` declarations producing a closure `Value`, and calling them: bind args to params in a fresh scope chained to the closure's captured environment, execute the body, and consume `Flow::Return`.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/value.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `Fn,` before `Eof`.

- [ ] **Step 2: `src/lexer.rs`** — extend the keyword match with `"fn" => Tok::Fn,`.

- [ ] **Step 3: `src/value.rs`** — add a `Function` value carrying a closure. Add the import and a `Function` struct, the enum variant, and update `PartialEq`/`Debug`/`Display`/`is_truthy`.

At the top of `value.rs` add:

```rust
use crate::ast::Stmt;
use crate::env::Environment;
use std::cell::RefCell;
```

Add the function representation:

```rust
/// A user-defined function with its captured (closure) environment.
pub struct Function {
    pub name: Option<String>,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub closure: Environment,
}
```

Add a variant to `Value`:

```rust
    Function(Rc<Function>),
```

Update `is_truthy` — functions are truthy (the `matches!` already returns true for anything not `Nil`/`Bool(false)`, so no change needed; confirm).

Update `PartialEq` — functions compare by identity:

```rust
            (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
```

Update `Debug`:

```rust
            Value::Function(func) => {
                write!(f, "Function({})", func.name.as_deref().unwrap_or("<anonymous>"))
            }
```

Update `Display`:

```rust
            Value::Function(func) => match &func.name {
                Some(n) => write!(f, "<function {}>", n),
                None => write!(f, "<function>"),
            },
```

NOTE: `RefCell` import is added now because the closure environment may be mutated; it is used by `env.rs` already — only add the import to `value.rs` if the compiler needs it (it likely does NOT; remove unused imports to keep clippy clean).

- [ ] **Step 4: `src/ast.rs`** — add a function-declaration statement:

```rust
    Fn { name: String, params: Vec<String>, body: Vec<Stmt> },
```

- [ ] **Step 5: `src/parser.rs`** — add `Tok::Fn => self.fn_decl(),` to `statement`, plus a shared parameter-list parser:

```rust
    fn fn_decl(&mut self) -> Result<Stmt, AsError> {
        self.eat(&Tok::Fn)?;
        let name = match self.advance() {
            Tok::Ident(name) => name,
            other => {
                return Err(AsError::at(
                    format!("expected a function name, found {:?}", other),
                    self.tokens[self.pos - 1].span,
                ))
            }
        };
        let params = self.param_list()?;
        let body = self.block()?;
        Ok(Stmt::Fn { name, params, body })
    }

    /// Parse `( ident, ident, … )` — a comma-separated list of parameter names.
    fn param_list(&mut self) -> Result<Vec<String>, AsError> {
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                match self.advance() {
                    Tok::Ident(name) => params.push(name),
                    other => {
                        return Err(AsError::at(
                            format!("expected a parameter name, found {:?}", other),
                            self.tokens[self.pos - 1].span,
                        ))
                    }
                }
                if *self.peek() == Tok::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.eat(&Tok::RParen)?;
        Ok(params)
    }
```

- [ ] **Step 6: `src/interp.rs`** — handle the `Fn` declaration and invoke user functions in `Call`.

Add to `exec_stmt`:

```rust
            Stmt::Fn { name, params, body } => {
                let func = Value::Function(std::rc::Rc::new(crate::value::Function {
                    name: Some(name.clone()),
                    params: params.clone(),
                    body: body.clone(),
                    closure: env.clone(),
                }));
                env.define(name, func, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
```

Extend the `Call` arm's `match callee_v` to handle user functions. Replace it with:

```rust
                match callee_v {
                    Value::Builtin(name) => self.call_builtin(&name, &values, expr.span),
                    Value::Function(func) => self.call_function(&func, values, expr.span).await,
                    _ => Err(AsError::at("value is not callable", callee.span)),
                }
```

Add the `call_function` method:

```rust
    #[async_recursion(?Send)]
    async fn call_function(
        &mut self,
        func: &crate::value::Function,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, AsError> {
        if args.len() != func.params.len() {
            return Err(AsError::at(
                format!(
                    "{} expected {} argument(s), got {}",
                    func.name.as_deref().unwrap_or("function"),
                    func.params.len(),
                    args.len()
                ),
                span,
            ));
        }
        // New scope chained to the closure's captured environment.
        let call_env = func.closure.child();
        for (param, arg) in func.params.iter().zip(args.into_iter()) {
            call_env.define(param, arg, true).map_err(AsError::new)?;
        }
        match self.exec(&func.body, &call_env).await? {
            Flow::Return(v) => Ok(v),
            Flow::Normal => Ok(Value::Nil),
            Flow::Break => Err(AsError::at("'break' outside of a loop", span)),
            Flow::Continue => Err(AsError::at("'continue' outside of a loop", span)),
        }
    }
```

Ensure `use crate::span::Span;` is present in `interp.rs` (added during the M2 polish fix). Add interpreter tests:

```rust
    #[tokio::test]
    async fn calls_a_user_function() {
        let src = "fn add(a, b) { return a + b }\nprint(add(2, 3))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");
    }

    #[tokio::test]
    async fn recursion_works() {
        let src = "fn fact(n) { if (n <= 1) { return 1 }\nreturn n * fact(n - 1) }\nprint(fact(5))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "120\n");
    }

    #[tokio::test]
    async fn closures_capture_their_environment() {
        // makeAdder returns a function that closes over `x`.
        let src = "fn makeAdder(x) { fn adder(y) { return x + y }\nreturn adder }\nlet add10 = makeAdder(10)\nprint(add10(5))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "15\n");
    }

    #[tokio::test]
    async fn arity_mismatch_errors() {
        let src = "fn f(a, b) { return a }\nf(1)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = interp.exec(&stmts, &env).await.unwrap_err();
        assert!(err.message.contains("expected 2 argument"));
    }

    #[tokio::test]
    async fn function_without_return_yields_nil() {
        let src = "fn noop() { let x = 1 }\nprint(noop())";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "nil\n");
    }
```

- [ ] **Step 7: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 8: Commit**

```bash
git add src/token.rs src/lexer.rs src/value.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add user-defined functions with closures, return, and recursion"
```

---

## Task 4: Arrow-function expressions

Add `(params) => expr` and `(params) => { block }` and the single-param `x => expr` form, all producing anonymous closures.

**Files:** `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `FatArrow,` before `Eof`.

- [ ] **Step 2: `src/lexer.rs`** — add a `=>` form to the `'='` arm. The `'='` arm currently distinguishes `==` from `=`; extend it to also recognize `=>`:

```rust
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token { tok: Tok::EqEq, span: Span::new(start, start + 2) });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '>' {
                    tokens.push(Token { tok: Tok::FatArrow, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Eq, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
```

Add a lexer test:

```rust
    #[test]
    fn lexes_fat_arrow() {
        assert_eq!(
            kinds("x => x"),
            vec![Tok::Ident("x".into()), Tok::FatArrow, Tok::Ident("x".into()), Tok::Eof]
        );
    }
```

- [ ] **Step 3: `src/ast.rs`** — add an arrow-function expression node:

```rust
    Arrow { params: Vec<String>, body: Box<ArrowBody> },
```

and the body type + its `Display`:

```rust
#[derive(Clone, Debug)]
pub enum ArrowBody {
    Expr(Box<Expr>),
    Block(Vec<Stmt>),
}
```

Add to the `ExprKind` `Display` match:

```rust
            ExprKind::Arrow { params, .. } => write!(f, "(arrow [{}])", params.join(" ")),
```

- [ ] **Step 4: `src/parser.rs`** — parse arrow functions. Arrow functions are tricky because `(a, b)` looks like a parenthesized expression until the `=>` appears. Handle both the single-identifier form (`x => …`) and the parenthesized form at the start of `assignment` (the lowest precedence, since arrows bind loosely).

In `assignment`, BEFORE parsing the target, check for an arrow:

```rust
    fn assignment(&mut self) -> Result<Expr, AsError> {
        // Arrow functions: `x => …` or `(a, b) => …`. Detect without breaking
        // ordinary parenthesized expressions by checking ahead for `=>`.
        if let Some(arrow) = self.try_arrow()? {
            return Ok(arrow);
        }

        let target = self.coalesce()?;
        // … (existing assignment/compound-assignment logic unchanged) …
```

Add `try_arrow` and an `arrow_body` helper:

```rust
    /// Attempt to parse an arrow function at the current position. Returns
    /// `Ok(None)` (without consuming) if what follows is not an arrow.
    fn try_arrow(&mut self) -> Result<Option<Expr>, AsError> {
        let start = self.span().start;
        // Single-parameter form: `ident => …`
        if let Tok::Ident(name) = self.peek().clone() {
            if self.tokens[self.pos + 1].tok == Tok::FatArrow {
                self.advance(); // ident
                self.advance(); // =>
                let body = self.arrow_body()?;
                let end = self.prev_end();
                return Ok(Some(Expr {
                    kind: ExprKind::Arrow { params: vec![name], body: Box::new(body) },
                    span: Span::new(start, end),
                }));
            }
            return Ok(None);
        }
        // Parenthesized form: `( params ) => …`. Scan ahead to find the matching
        // `)` and check whether `=>` follows; only then commit to arrow parsing.
        if *self.peek() == Tok::LParen && self.parens_then_arrow() {
            let params = self.param_list()?;
            self.eat(&Tok::FatArrow)?;
            let body = self.arrow_body()?;
            let end = self.prev_end();
            return Ok(Some(Expr {
                kind: ExprKind::Arrow { params, body: Box::new(body) },
                span: Span::new(start, end),
            }));
        }
        Ok(None)
    }

    /// Look ahead from a `(` to its matching `)` and report whether the next
    /// token after the `)` is `=>`. Does not consume tokens.
    fn parens_then_arrow(&self) -> bool {
        let mut depth = 0usize;
        let mut i = self.pos;
        while i < self.tokens.len() {
            match self.tokens[i].tok {
                Tok::LParen => depth += 1,
                Tok::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            self.tokens.get(i + 1).map(|t| &t.tok),
                            Some(Tok::FatArrow)
                        );
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn arrow_body(&mut self) -> Result<crate::ast::ArrowBody, AsError> {
        if *self.peek() == Tok::LBrace {
            Ok(crate::ast::ArrowBody::Block(self.block()?))
        } else {
            Ok(crate::ast::ArrowBody::Expr(Box::new(self.assignment()?)))
        }
    }
```

Add `use crate::ast::ArrowBody;`-style references via the fully-qualified path as above (or add to the `use crate::ast::{…}` line). Add parser tests:

```rust
    #[test]
    fn parses_single_param_arrow() {
        assert_eq!(sexpr("x => x + 1"), "(arrow [x])");
    }

    #[test]
    fn parses_multi_param_arrow() {
        assert_eq!(sexpr("(a, b) => a + b"), "(arrow [a b])");
    }

    #[test]
    fn parenthesized_non_arrow_still_works() {
        assert_eq!(sexpr("(1 + 2) * 3"), "(* (+ 1 2) 3)");
    }
```

- [ ] **Step 5: `src/interp.rs`** — evaluate an `Arrow` to a `Value::Function`. Add an `ExprKind::Arrow` arm to `eval_expr`:

```rust
            ExprKind::Arrow { params, body } => {
                let body_stmts = match body.as_ref() {
                    crate::ast::ArrowBody::Block(stmts) => stmts.clone(),
                    crate::ast::ArrowBody::Expr(e) => vec![Stmt::Return(Some((**e).clone()))],
                };
                Ok(Value::Function(std::rc::Rc::new(crate::value::Function {
                    name: None,
                    params: params.clone(),
                    body: body_stmts,
                    closure: env.clone(),
                })))
            }
```

(An expression-bodied arrow is desugared to a single `return <expr>` so it reuses the exact same call machinery as `fn`.)

Add interpreter tests:

```rust
    #[tokio::test]
    async fn arrow_expression_body() {
        let src = "let double = x => x * 2\nprint(double(21))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "42\n");
    }

    #[tokio::test]
    async fn arrow_multi_param_and_closure() {
        let src = "let base = 100\nlet f = (a, b) => a + b + base\nprint(f(1, 2))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "103\n");
    }

    #[tokio::test]
    async fn arrow_block_body_with_return() {
        let src = "let f = (n) => { if (n > 0) { return \"pos\" }\nreturn \"nonpos\" }\nprint(f(5))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "pos\n");
    }
```

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add arrow-function expressions"
```

---

## Task 5: End-to-end demo + integration test

**Files:** `examples/functions.as` (new), `tests/cli.rs` (modify).

- [ ] **Step 1: Create `examples/functions.as`**

```
fn fib(n) {
  if (n < 2) { return n }
  return fib(n - 1) + fib(n - 2)
}

let nums = 0
for (i in 0..10) {
  if (fib(i) % 2 == 0) { continue }
  nums += 1
}

let triple = x => x * 3
print(fib(10))
print(triple(7))
print(nums)
```

- [ ] **Step 2: Add an integration test to `tests/cli.rs`**

```rust
#[test]
fn runs_functions_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/functions.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // fib(10) = 55; triple(7) = 21; count of odd fib(0..10) = fib values
    // [0,1,1,2,3,5,8,13,21,34] -> odd ones: 1,1,3,5,13,21 -> 6
    assert_eq!(String::from_utf8_lossy(&output.stdout), "55\n21\n6\n");
}
```

- [ ] **Step 3: Run** `cargo test` (incl. `runs_functions_example`), then `cargo run --quiet -- run examples/functions.as` (expect `55`, `21`, `6`). Paste output. Run `cargo clippy --all-targets`.

- [ ] **Step 4: Commit**

```bash
git add examples/functions.as tests/cli.rs
git commit -m "test: add functions end-to-end example (recursion, closures, control flow)"
```

---

## Definition of Done

- `cargo test` passes (all unit + integration); `cargo clippy --all-targets` clean.
- `cargo run -- run examples/functions.as` prints `55`, `21`, `6`.
- AScript supports: `fn` declarations, arrow functions, closures capturing their environment, recursion, `return`/`break`/`continue`, arity checking, and callables as first-class values.
- Builtins (`print`) are first-class `Value`s resolved through the environment.

## Hand-off to Milestone 4 ("Data structures")

Adds arrays `[…]`, objects `{…}`, maps (new `Value` kinds), member access `.`, indexing `[]`, optional chaining `?.`, member/index l-value assignment, `for (x of iterable)` (a sibling `Stmt::ForOf`), and template strings. The `postfix` parser method is the insertion point for `.`/`[]`/`?.`; the `Call` dispatch already evaluates the callee to a `Value`, so method calls (`obj.f()`) compose once member access exists. `Value`'s manual `PartialEq`/`Debug` already anticipate non-derivable heap values.
