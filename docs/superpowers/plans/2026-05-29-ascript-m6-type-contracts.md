# AScript Milestone 6 — Gradual Type Contracts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement spec §5 gradual, runtime-checked type contracts: optional type annotations on `let`/`const` bindings, function parameters, and function return types, verified at runtime where values cross those boundaries. A failed contract is a **panic** (the `Control::Panic` tier from M5).

**Architecture:** A `Type` AST enum + a parser for type annotations (after `:`). Functions store typed params (`Vec<Param>`) and an optional return type. The interpreter runs a recursive `check_type(value, &Type) -> bool` at three sites: a typed `let`/`const` binding, on entry to each typed parameter, and on a typed function's return. Failure → `Control::Panic`. Annotations are KEPT (not erased) and checked eagerly to full declared depth (`array<number>` checks every element).

**Tech Stack:** Rust 2021, tokio (current_thread), async-recursion, indexmap. No new crates.

**Starting state (end of M5, on `main`):** Full language core + data structures + Result/error model. Function params are `Vec<String>`; `Stmt::Let { name, value, mutable }`; `Stmt::Fn { name, params, body }`; `ExprKind::Arrow { params, body }`; `Value::Function(Rc<Function>)` with `Function { name, params, body, closure }`. The `Colon` token exists (M4). Lone `|` currently ERRORS in the lexer. 94 lib + 6 integration tests.

**Conventions:** spans char offsets; single-threaded `Rc`/`RefCell`; `?Send` async recursion.

## Spec §5 semantics decided

- **Optional & runtime-checked:** omitting an annotation = `any` (no check). Annotations are checked at runtime as contracts, never statically.
- **Check sites:** typed `let`/`const` value; typed function parameter (on call); typed function return (on return).
- **Failure = panic:** `Control::Panic(AsError)` with message "type contract violated: expected `<T>`, got `<value>`". Catchable only via `recover` (M5).
- **Parametric depth:** `array<T>` verifies the value is an array AND every element satisfies `T`; `Result<T>` = `[T, error]`; tuples `[A, B]` check length + each position; unions `A | B` match either. `any` opts out.
- **Types supported now:** `number`, `string`, `bool`, `nil`, `any`, `fn`, `object`, `error` (= `object | nil`), `array<T>`, `Result<T>`, tuple `[T, …]`, union `A | B`.

## Scope & Justified Deferrals

| Deferred | Why | Milestone |
|---|---|---|
| `map<K,V>` type | The `Map` value kind doesn't exist yet | **M8** |
| Class-name / enum-name types | Classes & enums don't exist yet | **M7** |
| Static type *checking* (compile-time) | Spec §5 is explicitly runtime-only — not deferred, it's a non-goal | — |

When M7 adds classes/enums and M8 adds maps, they extend the `Type` enum + `check_type` with the new kinds (the parser's `parse_type_atom` gets new arms).

---

## Task 1: Type AST, annotation parsing, and the `Param` refactor

Add the `Type` enum and parse annotations; refactor function params from `Vec<String>` to `Vec<Param>` (name + optional type) and add optional return types. NO enforcement yet — annotations are parsed and stored; untyped code behaves identically.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/value.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `Pipe,` before `Eof` (for union types `A | B`).

- [ ] **Step 2: `src/lexer.rs`** — change the `'|'` arm so `||`→`PipePipe` and a lone `|`→`Pipe` (was an error):

```rust
            '|' => {
                if i + 1 < chars.len() && chars[i + 1] == '|' {
                    tokens.push(Token { tok: Tok::PipePipe, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Pipe, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
```

- [ ] **Step 3: `src/ast.rs`** — add the `Type` enum and `Param`, and thread types into `Stmt::Let`, `Stmt::Fn`, and `ExprKind::Arrow`.

```rust
/// A type annotation (spec §5). Checked at runtime as a contract.
#[derive(Clone, Debug)]
pub enum Type {
    Number,
    String,
    Bool,
    Nil,
    Any,
    Fn,
    Object,
    Error, // object | nil
    Array(Box<Type>),
    Result(Box<Type>),
    Tuple(Vec<Type>),
    Union(Box<Type>, Box<Type>),
}

/// A function parameter: a name with an optional type annotation.
#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Number => write!(f, "number"),
            Type::String => write!(f, "string"),
            Type::Bool => write!(f, "bool"),
            Type::Nil => write!(f, "nil"),
            Type::Any => write!(f, "any"),
            Type::Fn => write!(f, "fn"),
            Type::Object => write!(f, "object"),
            Type::Error => write!(f, "error"),
            Type::Array(t) => write!(f, "array<{}>", t),
            Type::Result(t) => write!(f, "Result<{}>", t),
            Type::Tuple(ts) => {
                write!(f, "[")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t)?;
                }
                write!(f, "]")
            }
            Type::Union(a, b) => write!(f, "{} | {}", a, b),
        }
    }
}
```

Change the variants:
- `Stmt::Let { name: String, ty: Option<Type>, value: Expr, mutable: bool }`
- `Stmt::Fn { name: String, params: Vec<Param>, ret: Option<Type>, body: Vec<Stmt> }`
- `ExprKind::Arrow { params: Vec<Param>, body: Box<ArrowBody> }`

- [ ] **Step 4: `src/parser.rs`** — add `parse_type`, parse annotations in `let_stmt`, `param_list` (→ `Vec<Param>`), and `fn_decl` (return type), and update arrow construction to use `Vec<Param>`.

```rust
    fn parse_type(&mut self) -> Result<crate::ast::Type, AsError> {
        let mut t = self.parse_type_atom()?;
        while *self.peek() == Tok::Pipe {
            self.advance();
            let rhs = self.parse_type_atom()?;
            t = crate::ast::Type::Union(Box::new(t), Box::new(rhs));
        }
        Ok(t)
    }

    fn parse_type_atom(&mut self) -> Result<crate::ast::Type, AsError> {
        use crate::ast::Type;
        let span = self.span();
        match self.advance() {
            Tok::Nil => Ok(Type::Nil),
            Tok::Fn => Ok(Type::Fn),
            Tok::LBracket => {
                // tuple type [T1, T2, ...]
                let mut parts = Vec::new();
                if *self.peek() != Tok::RBracket {
                    loop {
                        parts.push(self.parse_type()?);
                        if *self.peek() == Tok::Comma {
                            self.advance();
                            if *self.peek() == Tok::RBracket {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                self.eat(&Tok::RBracket)?;
                Ok(Type::Tuple(parts))
            }
            Tok::Ident(name) => match name.as_str() {
                "number" => Ok(Type::Number),
                "string" => Ok(Type::String),
                "bool" => Ok(Type::Bool),
                "any" => Ok(Type::Any),
                "object" => Ok(Type::Object),
                "error" => Ok(Type::Error),
                "array" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Ok(Type::Array(Box::new(inner)))
                }
                "Result" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Ok(Type::Result(Box::new(inner)))
                }
                "map" => Err(AsError::at(
                    "map<K,V> type annotations arrive in Milestone 8",
                    span,
                )),
                other => Err(AsError::at(
                    format!("unknown type '{}' (class/enum types arrive in Milestone 7)", other),
                    span,
                )),
            },
            other => Err(AsError::at(format!("expected a type, found {:?}", other), span)),
        }
    }
```

In `let_stmt`, after reading the name, parse an optional annotation:

```rust
        let ty = if *self.peek() == Tok::Colon {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.eat(&Tok::Eq)?;
        let value = self.expr()?;
        Ok(Stmt::Let { name, ty, value, mutable })
```

Change `param_list` to return `Vec<Param>`:

```rust
    fn param_list(&mut self) -> Result<Vec<crate::ast::Param>, AsError> {
        self.eat(&Tok::LParen)?;
        let mut params = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                let name = match self.advance() {
                    Tok::Ident(name) => name,
                    other => {
                        return Err(AsError::at(
                            format!("expected a parameter name, found {:?}", other),
                            self.tokens[self.pos - 1].span,
                        ))
                    }
                };
                let ty = if *self.peek() == Tok::Colon {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(crate::ast::Param { name, ty });
                if *self.peek() == Tok::Comma {
                    self.advance();
                    if *self.peek() == Tok::RParen {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        self.eat(&Tok::RParen)?;
        Ok(params)
    }
```

In `fn_decl`, after `param_list`, parse an optional return type:

```rust
        let params = self.param_list()?;
        let ret = if *self.peek() == Tok::Colon {
            self.advance();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.block()?;
        Ok(Stmt::Fn { name, params, ret, body })
```

Arrow construction (`try_arrow`): the single-param form `x => …` builds `vec![Param { name, ty: None }]`; the parenthesized form uses `param_list()` directly (now `Vec<Param>`). Update both sites.

- [ ] **Step 5: `src/value.rs`** — `Function` stores typed params + return type:

```rust
pub struct Function {
    pub name: Option<String>,
    pub params: Vec<crate::ast::Param>,
    pub ret: Option<crate::ast::Type>,
    pub body: Vec<Stmt>,
    pub closure: Environment,
}
```

(Add `use crate::ast::Param;`/`Type` as needed, or use fully-qualified paths.)

- [ ] **Step 6: `src/interp.rs`** — update construction & param binding (NO checking yet).
  - `Stmt::Fn` arm: build `Function { name, params: params.clone(), ret: ret.clone(), body: body.clone(), closure: env.clone() }`.
  - `ExprKind::Arrow` arm: build `Function { name: None, params: params.clone(), ret: None, body: body_stmts, closure: env.clone() }`.
  - `call_function`: arity check uses `func.params.len()`; bind each param via `param.name`: `call_env.define(&param.name, arg, true)`. (Iterate `func.params.iter().zip(args)`.)
  - `Stmt::Let` arm: destructure now includes `ty` — ignore it for now (`Stmt::Let { name, ty: _, value, mutable }`).

- [ ] **Step 7: Tests** — parsing only (no enforcement). Add a parser test:

```rust
    #[test]
    fn parses_type_annotations() {
        assert!(parse(&lex("let x: number = 5").unwrap()).is_ok());
        assert!(parse(&lex("fn add(a: number, b: number): number { return a + b }").unwrap()).is_ok());
        assert!(parse(&lex("let xs: array<number> = [1, 2]").unwrap()).is_ok());
        assert!(parse(&lex("let r: Result<string> = Ok(\"x\")").unwrap()).is_ok());
        assert!(parse(&lex("let u: number | nil = nil").unwrap()).is_ok());
        assert!(parse(&lex("let t: [number, string] = [1, \"a\"]").unwrap()).is_ok());
    }
```

And an interp test confirming untyped + typed code still RUNS (no enforcement yet, so even a "wrong" annotation runs):

```rust
    #[tokio::test]
    async fn typed_code_runs_without_enforcement_yet() {
        let src = "let x: number = 5\nfn f(a: number): number { return a + 1 }\nprint(f(x))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "6\n");
    }
```

- [ ] **Step 8: Run** `cargo test` — all 94 lib + 6 integration pass (untyped code unaffected) + the new tests. `cargo clippy --all-targets` clean.

- [ ] **Step 9: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/value.rs src/interp.rs
git commit -m "feat: parse type annotations and refactor params to typed Param"
```

---

## Task 2: `check_type` + `let`/`const` contract enforcement

**Files:** `src/interp.rs`.

- [ ] **Step 1: Add the recursive `check_type`** (free fn in `interp.rs`):

```rust
/// Runtime contract check (spec §5). Eagerly checks parametric types to full depth.
fn check_type(value: &Value, ty: &crate::ast::Type) -> bool {
    use crate::ast::Type;
    match ty {
        Type::Any => true,
        Type::Number => matches!(value, Value::Number(_)),
        Type::String => matches!(value, Value::Str(_)),
        Type::Bool => matches!(value, Value::Bool(_)),
        Type::Nil => matches!(value, Value::Nil),
        Type::Object => matches!(value, Value::Object(_)),
        Type::Fn => matches!(value, Value::Function(_) | Value::Builtin(_)),
        Type::Error => matches!(value, Value::Object(_) | Value::Nil),
        Type::Array(elem) => match value {
            Value::Array(a) => a.borrow().iter().all(|v| check_type(v, elem)),
            _ => false,
        },
        Type::Result(inner) => match value {
            Value::Array(a) => {
                let b = a.borrow();
                b.len() == 2 && check_type(&b[0], inner) && check_type(&b[1], &Type::Error)
            }
            _ => false,
        },
        Type::Tuple(types) => match value {
            Value::Array(a) => {
                let b = a.borrow();
                b.len() == types.len() && b.iter().zip(types.iter()).all(|(v, t)| check_type(v, t))
            }
            _ => false,
        },
        Type::Union(a, b) => check_type(value, a) || check_type(value, b),
    }
}

/// Build a contract-violation panic.
fn contract_panic(ty: &crate::ast::Type, value: &Value, span: Span) -> Control {
    AsError::at(
        format!("type contract violated: expected {}, got {} ({})", ty, type_name(value), value),
        span,
    )
    .into()
}
```

(Note: `check_type`'s `borrow()` recursion is sync — no await, no borrow-across-await. A cyclic value can't reach `check_type` because contracts run at bind/param/return on freshly-produced values, and cyclic values can only be formed later via index assignment.)

- [ ] **Step 2: Enforce at `let`/`const`** in the `Stmt::Let` arm of `exec_stmt`:

```rust
            Stmt::Let { name, ty, value, mutable } => {
                let v = self.eval_expr(value, env).await?;
                if let Some(ty) = ty {
                    if !check_type(&v, ty) {
                        return Err(contract_panic(ty, &v, value.span));
                    }
                }
                env.define(name, v, *mutable).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
```

- [ ] **Step 3: Tests:**

```rust
    #[tokio::test]
    async fn let_contract_passes_and_fails() {
        // passes
        let src = "let x: number = 5\nprint(x)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");

        // fails
        let bad = "let x: number = \"oops\"";
        let stmts = parse(&lex(bad).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
        assert!(err.message.contains("expected number"));
    }

    #[tokio::test]
    async fn parametric_and_union_contracts() {
        // array<number> with a bad element fails
        let bad = "let xs: array<number> = [1, \"two\", 3]";
        let stmts = parse(&lex(bad).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        assert!(interp.exec(&stmts, &env).await.is_err());

        // union passes for either member
        let ok = "let a: number | nil = nil\nlet b: number | nil = 7\nprint(b)";
        let stmts = parse(&lex(ok).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "7\n");

        // Result<number>: Ok(5) passes, Ok("x") fails
        let r = "let r: Result<number> = Ok(5)\nprint(r[0])";
        let stmts = parse(&lex(r).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");
    }
```

- [ ] **Step 4: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 5: Commit**

```bash
git add src/interp.rs
git commit -m "feat: enforce type contracts on let/const bindings"
```

---

## Task 3: Parameter & return contract enforcement

**Files:** `src/interp.rs`.

- [ ] **Step 1: Enforce in `call_function`.** When binding each parameter, check its type; before returning, check the return type. Update `call_function`:

```rust
        // bind + check params
        let call_env = func.closure.child();
        for (param, arg) in func.params.iter().zip(args.into_iter()) {
            if let Some(ty) = &param.ty {
                if !check_type(&arg, ty) {
                    return Err(contract_panic(ty, &arg, span));
                }
            }
            call_env.define(&param.name, arg, true).map_err(AsError::new)?;
        }
        // execute, then check the return type
        let result = match self.exec(&func.body, &call_env).await {
            Ok(Flow::Return(v)) => v,
            Ok(Flow::Normal) => Value::Nil,
            Ok(Flow::Break) => return Err(AsError::at("'break' outside of a loop", span).into()),
            Ok(Flow::Continue) => return Err(AsError::at("'continue' outside of a loop", span).into()),
            Err(Control::Propagate(v)) => v,
            Err(Control::Panic(e)) => return Err(Control::Panic(e)),
        };
        if let Some(ty) = &func.ret {
            if !check_type(&result, ty) {
                return Err(contract_panic(ty, &result, span));
            }
        }
        Ok(result)
```

(Keep the existing arity check before this. The `span` here is the call-site span — acceptable for the diagnostic.)

- [ ] **Step 2: Tests:**

```rust
    #[tokio::test]
    async fn param_contract_enforced() {
        let src = "fn double(n: number): number { return n * 2 }\nprint(double(\"x\"))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
        assert!(err.message.contains("expected number"));
    }

    #[tokio::test]
    async fn return_contract_enforced() {
        // returns a string but annotated number
        let src = "fn f(): number { return \"nope\" }\nf()";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("type contract violated"));
    }

    #[tokio::test]
    async fn typed_function_happy_path() {
        let src = "fn add(a: number, b: number): number { return a + b }\nprint(add(2, 3))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\n");
    }

    #[tokio::test]
    async fn contract_failure_is_recoverable() {
        // a contract panic is catchable by recover (it's a Panic, M5)
        let src = "fn f(n: number) { return n }\nlet r = recover(() => f(\"bad\"))\nprint(r[0])\nprint(r[1].message)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert!(interp.output.starts_with("nil\n"));
        assert!(interp.output.contains("type contract violated"));
    }
```

- [ ] **Step 3: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 4: Commit**

```bash
git add src/interp.rs
git commit -m "feat: enforce type contracts on function parameters and returns"
```

---

## Task 4: End-to-end demo + integration test

**Files:** `examples/typed.as` (new), `tests/cli.rs` (modify).

- [ ] **Step 1: Create `examples/typed.as`**

```
fn area(width: number, height: number): number {
  return width * height
}

fn greet(name: string): string {
  return `hello, ${name}`
}

let dims: array<number> = [3, 4, 5]
let total: number = 0
for (d of dims) {
  total += d
}

print(area(3, 4))
print(greet("Ada"))
print(total)

// a contract violation, caught by recover
let r = recover(() => area("wide", 4))
print(r[1].message)
```

- [ ] **Step 2: Integration test in `tests/cli.rs`**

```rust
#[test]
fn runs_typed_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg("examples/typed.as").output().unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("12\n"));            // area(3,4)
    assert!(out.contains("hello, Ada"));      // greet
    assert!(out.contains("12\n"));            // total 3+4+5=12
    assert!(out.contains("type contract violated")); // recovered contract panic
}
```

- [ ] **Step 3: Run** `cargo test` (incl. `runs_typed_example`), then `cargo run --quiet -- run examples/typed.as` (paste output: expect 12, "hello, Ada", 12, then the contract-violation message). `cargo clippy --all-targets`.

- [ ] **Step 4: Commit**

```bash
git add examples/typed.as tests/cli.rs
git commit -m "test: add gradual-type-contracts end-to-end example"
```

---

## Definition of Done

- `cargo test` passes (all unit + integration); `cargo clippy --all-targets` clean.
- `cargo run -- run examples/typed.as` shows the typed happy-path results and a recovered contract violation.
- AScript supports optional type annotations on `let`/`const`, params, and returns, checked at runtime as contracts; failures panic (and are `recover`-able); parametric (`array<T>`/`Result<T>`/tuple) and union types check to full depth. `any`/omitted = no check.

## Hand-off to Milestone 7 ("Classes & enums + match")

Adds `class`/`extends`/`super`/`self`/`init`, simple enums, and the `match` expression. Class and enum NAMES become new `Type` variants (`Type::Named(String)`) — `parse_type_atom`'s "unknown type" arm becomes a `Named` lookup, and `check_type` gains an instance-of check. The `Colon` token and `parse_type` are reused. Class instances are tagged objects; enums are tagged values (see spec §8).
