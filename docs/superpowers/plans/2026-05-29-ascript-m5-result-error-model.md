# AScript Milestone 5 — Result & Error Model Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement AScript's two-tier error model (spec §6): recoverable errors as `[value, err]` Result values with `Ok`/`Err` constructors and the `?` propagation operator; unrecoverable programmer bugs as **panics** that abort unless caught by the `recover` boundary; and `assert`.

**Architecture:** The interpreter's eval/exec error channel changes from `Result<_, AsError>` to `Result<_, Control>` where `Control = Panic(AsError) | Propagate(Value)`. `Panic` is the existing abort path (undefined var, OOB index, member-of-nil, type errors, failed `assert`). `Propagate(value)` is produced by `?` and caught by `call_function` (becomes that function's return value) — this is how an expression-level `?` performs a function-level early return. `Ok`/`Err`/`recover`/`assert` are built-ins. Result values are ordinary 2-element arrays `[value, err]` (arrays exist since M4); error objects are `{ message: ... }` objects.

**Tech Stack:** Rust 2021, tokio (current_thread), async-recursion, indexmap. No new crates.

**Starting state (end of M4, on `main`):** Full language core + data structures. `eval_expr`/`eval_chain`/`exec`/`exec_stmt`/`assign_to`/`call_function` return `Result<_, AsError>`; `call_builtin` returns `Result<Value, AsError>`. `Flow = Normal|Return(Value)|Break|Continue` (statement control flow). `print` is the only builtin. The lexer's lone `?` errors with "the ? operator arrives in Milestone 5". 86 lib + 5 integration tests.

**Conventions:** spans char offsets; single-threaded `Rc`/`RefCell`; `?Send` async recursion; optional `;`.

## Spec §6 semantics decided

- **Result value:** `[value, err]` — a 2-element array. `Ok(v)` → `[v, nil]`. `Err(msg)` → `[nil, { message: msg }]` (an *error object*: an object with at least `message`).
- **`?` operator (postfix):** `expr?` evaluates `expr`, which MUST be a 2-element array `[value, err]` (else a panic "the ? operator requires a Result pair"). If `err != nil`, the enclosing function returns `[nil, err]` (via `Control::Propagate`). Otherwise `expr?` evaluates to `value` (element 0). At the top level (no enclosing function), a propagated `?` simply ends the program.
- **Panic tier:** unrecoverable programmer bugs — failed `assert`, OOB index, member-of-nil, calling a non-callable, `?` on a non-Result, arithmetic on non-numbers, undefined variable, etc. These are `Control::Panic(AsError)`; they unwind to the top level, print the error, and exit non-zero. They are NOT catchable in normal code.
- **`recover(fn)`:** calls `fn` with no arguments. If `fn` panics, returns `[nil, { message: <panic message> }]`; otherwise returns `[<fn result>, nil]`. The single host/REPL boundary that converts a panic into a Result. (A `?`-propagation inside `fn` is already converted to `fn`'s return value by `call_function`, so `recover` sees a normal success.)
- **`assert(cond, msg?)`:** panics with `msg` (or "assertion failed") when `cond` is falsy; returns `nil` otherwise.

## Scope & Justified Deferrals

| Deferred | Why | Milestone |
|---|---|---|
| Type-contract panics (`number` annotation mismatch, etc.) | Contracts don't exist yet | **M6** |
| `arr.get(i)` safe accessor | A stdlib array method | **M8** |
| Stack-trace rendering in panics | Needs the diagnostics layer | **M9** |

---

## Task 1: Refactor the eval/exec error channel to `Control`

Introduce `Control { Panic(AsError), Propagate(Value) }` and thread it through `eval_expr`/`eval_chain`/`exec`/`exec_stmt`/`assign_to`/`call_function`/`call_builtin`. Behavior is UNCHANGED (no `?`/`recover` yet — `Propagate` is never produced). This is mechanical plumbing.

**Files:** `src/interp.rs`, `src/lib.rs`.

- [ ] **Step 1: Add the `Control` enum** near the top of `src/interp.rs` (after the `use`s, alongside `Flow`):

```rust
/// Non-local exit from expression/statement evaluation.
#[derive(Debug)]
pub enum Control {
    /// An unrecoverable programmer error (spec §6 Tier 2). Aborts unless caught
    /// by `recover`. Carries the diagnostic.
    Panic(AsError),
    /// A `?`-operator early return: the enclosing function should return this
    /// `[nil, err]` Result pair.
    Propagate(Value),
}

impl From<AsError> for Control {
    fn from(e: AsError) -> Self {
        Control::Panic(e)
    }
}
```

- [ ] **Step 2: Change return types** of these methods from `Result<…, AsError>` to `Result<…, Control>`:
  - `eval_expr(&mut self, …) -> Result<Value, Control>`
  - `eval_chain(&mut self, …) -> Result<(Value, bool), Control>`
  - `exec(&mut self, …) -> Result<Flow, Control>`
  - `exec_stmt(&mut self, …) -> Result<Flow, Control>`
  - `assign_to(&mut self, …) -> Result<Value, Control>`
  - `call_function(&mut self, …) -> Result<Value, Control>`
  - `call_builtin(&mut self, …) -> Result<Value, Control>`

  Leave `read_member(&self, …) -> Result<Value, AsError>` and the free `array_index(…) -> Result<usize, AsError>` returning `AsError` — their callers use `?`, which auto-converts via `From<AsError> for Control`.

  At every site that does a BARE `return Err(AsError::at(...))` or `return Err(AsError::new(...))` inside a now-`Control`-returning method, append `.into()`: `return Err(AsError::at(...).into())`. Sites that use `?` (e.g. `self.read_member(...)?`, `.map_err(AsError::new)?`, `.ok_or_else(|| AsError::at(...))?`) need NO change — `?` converts.

  `call_builtin`'s `print` success returns `Ok(Value::Nil)` (unchanged); its unknown-name arm becomes `Err(AsError::at(...).into())` (or keep `Err(AsError::...)` and let the `?` at the call site convert — but since it's a direct return, use `.into()`).

- [ ] **Step 3: `call_function` catches `Propagate`.** Replace its body's `self.exec(&func.body, &call_env).await?` match with an explicit match (it must intercept `Propagate`):

```rust
        match self.exec(&func.body, &call_env).await {
            Ok(Flow::Return(v)) => Ok(v),
            Ok(Flow::Normal) => Ok(Value::Nil),
            Ok(Flow::Break) => Err(AsError::at("'break' outside of a loop", span).into()),
            Ok(Flow::Continue) => Err(AsError::at("'continue' outside of a loop", span).into()),
            // A `?` inside the body wants THIS function to return the pair.
            Err(Control::Propagate(v)) => Ok(v),
            Err(Control::Panic(e)) => Err(Control::Panic(e)),
        }
```

- [ ] **Step 4: `src/lib.rs` — `run_source` converts `Control` back to `AsError`.** Replace the `match interp.exec(...)` block:

```rust
    match interp.exec(&program, &env).await {
        Ok(crate::interp::Flow::Break) => Err(AsError::new("'break' outside of a loop")),
        Ok(crate::interp::Flow::Continue) => Err(AsError::new("'continue' outside of a loop")),
        Ok(crate::interp::Flow::Normal) | Ok(crate::interp::Flow::Return(_)) => Ok(interp.output),
        // A panic aborts the program with its diagnostic.
        Err(crate::interp::Control::Panic(e)) => Err(e),
        // A top-level `?` propagation simply ends the program.
        Err(crate::interp::Control::Propagate(_)) => Ok(interp.output),
    }
```

(Note: this returns `interp.output` in the Ok/propagate arms — make sure the function returns `Result<String, AsError>` and `interp.output` is moved/cloned appropriately. Since `interp` is owned locally, returning `interp.output` by move is fine; restructure so the value is produced after the match if borrow-checker complains, e.g. compute the result in the match then `Ok(interp.output)` after.)

- [ ] **Step 5: Update interp tests that inspect errors.** Tests that do `interp.exec(...).await.unwrap_err()` now get a `Control`, not an `AsError`. Add a test helper to the `#[cfg(test)] mod tests`:

```rust
    /// Extract the panic's AsError from a Control (test helper).
    fn panic_of(c: Control) -> AsError {
        match c {
            Control::Panic(e) => e,
            Control::Propagate(_) => panic!("expected a panic, got a `?` propagation"),
        }
    }
```

Update each error-asserting test to wrap its `unwrap_err()` in `panic_of(...)` before accessing `.message`/`.span` — e.g.:

```rust
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("undefined variable 'missing'"));
```

Affected tests (search for `.unwrap_err()`): `undefined_variable_errors_with_span`, `calling_an_undefined_name_is_an_error`, `call_site_errors_carry_a_span`, `assigning_to_const_errors`, `block_scope_does_not_leak`, `out_of_bounds_index_errors`, `member_of_nil_errors`, `for_of_non_iterable_errors`, `parentheses_break_the_optional_chain`, `string_concatenation` (its two error cases use `.is_err()` which still works — no change needed there). Update all that access `.message`/`.span` via `panic_of`.

- [ ] **Step 6: Run** `cargo test` — all 86 lib + 5 integration pass UNCHANGED (behavior identical). `cargo clippy --all-targets` clean.

- [ ] **Step 7: Commit**

```bash
git add src/interp.rs src/lib.rs
git commit -m "refactor: introduce Control error channel (Panic vs Propagate)"
```

---

## Task 2: `Ok` / `Err` / `assert` built-ins + error objects

**Files:** `src/interp.rs`.

- [ ] **Step 1: Register the new builtins** in `global_env()`:

```rust
pub fn global_env() -> Environment {
    let env = Environment::global();
    for name in ["print", "Ok", "Err", "assert", "recover"] {
        env.define(name, Value::Builtin(name.into()), false)
            .expect("global env starts empty");
    }
    env
}
```

(`recover` is wired in Task 4; registering its name now is harmless — calling it before Task 4 would hit the unknown-arm, but no test does so until Task 4.)

- [ ] **Step 2: Implement `Ok`, `Err`, `assert` in `call_builtin`.** Add arms (helpers build arrays/objects via `Rc`/`RefCell`/`IndexMap`):

```rust
            "Ok" => {
                let value = args.first().cloned().unwrap_or(Value::Nil);
                Ok(make_pair(value, Value::Nil))
            }
            "Err" => {
                let msg = args.first().cloned().unwrap_or(Value::Nil);
                Ok(make_pair(Value::Nil, make_error(msg)))
            }
            "assert" => {
                let cond = args.first().cloned().unwrap_or(Value::Nil);
                if cond.is_truthy() {
                    Ok(Value::Nil)
                } else {
                    let msg = match args.get(1) {
                        Some(Value::Str(s)) => s.to_string(),
                        Some(v) => v.to_string(),
                        None => "assertion failed".to_string(),
                    };
                    Err(AsError::at(msg, span).into())
                }
            }
```

Add free helpers in `interp.rs`:

```rust
/// Build a `[value, err]` Result pair.
fn make_pair(value: Value, err: Value) -> Value {
    Value::Array(std::rc::Rc::new(std::cell::RefCell::new(vec![value, err])))
}

/// Build an error object `{ message: <msg> }`.
fn make_error(msg: Value) -> Value {
    let mut map = indexmap::IndexMap::new();
    map.insert("message".to_string(), msg);
    Value::Object(std::rc::Rc::new(std::cell::RefCell::new(map)))
}
```

- [ ] **Step 2b: Tests** (in interp tests):

```rust
    #[tokio::test]
    async fn ok_and_err_construct_result_pairs() {
        let src = "let r = Ok(5)\nprint(r[0])\nprint(r[1])\nlet e = Err(\"boom\")\nprint(e[0])\nprint(e[1].message)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "5\nnil\nnil\nboom\n");
    }

    #[tokio::test]
    async fn assert_passes_and_panics() {
        // passing assert returns nil
        let ok = "assert(1 < 2)\nprint(\"ok\")";
        let stmts = parse(&lex(ok).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "ok\n");

        // failing assert panics with the message
        let bad = "assert(false, \"nope\")";
        let stmts = parse(&lex(bad).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("nope"));
    }
```

- [ ] **Step 3: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 4: Commit**

```bash
git add src/interp.rs
git commit -m "feat: add Ok, Err, and assert built-ins with error objects"
```

---

## Task 3: The `?` propagation operator

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `Question,` before `Eof`.

- [ ] **Step 2: `src/lexer.rs`** — change the `'?'` arm so a lone `?` now emits `Tok::Question` (keep `??`→QuestionQuestion and `?.`→QuestionDot):

```rust
            '?' => {
                if i + 1 < chars.len() && chars[i + 1] == '?' {
                    tokens.push(Token { tok: Tok::QuestionQuestion, span: Span::new(start, start + 2) });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '.' {
                    tokens.push(Token { tok: Tok::QuestionDot, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Question, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
```

Add a lexer test:

```rust
    #[test]
    fn lexes_question_variants() {
        assert_eq!(kinds("a ? b ?? c?.d"),
            vec![
                Tok::Ident("a".into()), Tok::Question,
                Tok::Ident("b".into()), Tok::QuestionQuestion,
                Tok::Ident("c".into()), Tok::QuestionDot, Tok::Ident("d".into()),
                Tok::Eof,
            ]);
    }
```

- [ ] **Step 3: `src/ast.rs`** — add a try/propagation expression:

```rust
    Try(Box<Expr>),
```

`Display`: `ExprKind::Try(e) => write!(f, "(? {})", e),`

- [ ] **Step 4: `src/parser.rs`** — add `?` as a postfix suffix in the `postfix` loop (alongside call/index/member/optmember):

```rust
                Tok::Question => {
                    self.advance();
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::Try(Box::new(expr)), span };
                }
```

Add a parser test:

```rust
    #[test]
    fn parses_try_operator() {
        assert_eq!(sexpr("readFile(p)?"), "(? (call readFile p))");
    }
```

- [ ] **Step 5: `src/interp.rs`** — evaluate `Try`. Add a `Try` arm to `eval_expr`. It does NOT go through `eval_chain` (a `?` ends a chain), so handle it directly:

```rust
            ExprKind::Try(inner) => {
                let v = self.eval_expr(inner, env).await?;
                // Must be a 2-element Result pair [value, err].
                let arr = match &v {
                    Value::Array(a) if a.borrow().len() == 2 => a.clone(),
                    _ => {
                        return Err(AsError::at(
                            "the ? operator requires a Result pair [value, err]",
                            expr.span,
                        )
                        .into())
                    }
                };
                let (value, err) = {
                    let b = arr.borrow();
                    (b[0].clone(), b[1].clone())
                };
                if err == Value::Nil {
                    Ok(value)
                } else {
                    // Early-return [nil, err] from the enclosing function.
                    Err(Control::Propagate(make_pair(Value::Nil, err)))
                }
            }
```

NOTE: `make_pair` is defined in Task 2. Confirm the `arr.borrow()` is dropped before constructing the new pair (the block scoping above does this).

Add interpreter tests:

```rust
    #[tokio::test]
    async fn question_unwraps_ok_and_propagates_err() {
        // A function that uses `?`: returns the value on Ok, propagates [nil, err] on Err.
        let src = "
fn parse(x) {
  if (x < 0) { return Err(\"negative\") }
  return Ok(x * 2)
}
fn run(x) {
  let v = parse(x)?
  return Ok(v + 1)
}
let good = run(5)
print(good[0])
print(good[1])
let bad = run(-1)
print(bad[0])
print(bad[1].message)
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // run(5): parse->Ok(10), v=10, returns Ok(11) -> [11, nil]
        // run(-1): parse->Err, ? propagates [nil, {message:"negative"}]
        assert_eq!(interp.output, "11\nnil\nnil\nnegative\n");
    }

    #[tokio::test]
    async fn question_on_non_result_panics() {
        let src = "let x = 5\nlet y = x?";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = panic_of(interp.exec(&stmts, &env).await.unwrap_err());
        assert!(err.message.contains("requires a Result pair"));
    }
```

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add the ? Result-propagation operator"
```

---

## Task 4: `recover` boundary

`recover(fn)` invokes `fn` with no args and converts a panic into a Result `[nil, errObj]`.

**Files:** `src/interp.rs`.

- [ ] **Step 1: Factor a `call_value` helper** so both the `Call` arm and `recover` can invoke a callable `Value`. In `eval_chain`'s `Call` arm, the dispatch logic (`match callee_v { Builtin => call_builtin, Function => call_function, _ => not callable }`) moves into:

```rust
    async fn call_value(&mut self, callee: Value, args: Vec<Value>, span: Span) -> Result<Value, Control> {
        match callee {
            Value::Builtin(name) => self.call_builtin(&name, &args, span),
            Value::Function(func) => self.call_function(&func, args, span).await,
            _ => Err(AsError::at("value is not callable", span).into()),
        }
    }
```

(Add `#[async_recursion(?Send)]`.) Update `eval_chain`'s `Call` arm to `self.call_value(callee_v, values, expr.span).await` (preserve the existing not-callable span behavior — use `callee.span` if that was the prior behavior; otherwise `expr.span` is acceptable — keep it consistent with the pre-existing tests, which expect "value is not callable"; verify the `(1)(2)` test still passes).

- [ ] **Step 2: Implement `recover` in `call_builtin`:**

```rust
            "recover" => {
                let callee = args.first().cloned().unwrap_or(Value::Nil);
                match self.call_value(callee, Vec::new(), span).await {
                    Ok(v) => Ok(make_pair(v, Value::Nil)),
                    Err(Control::Panic(e)) => Ok(make_pair(Value::Nil, make_error(Value::Str(e.message.into())))),
                    // A `?` propagation inside `fn` is already converted to fn's return
                    // value by call_function, so this is unreachable in practice; pass it through.
                    Err(Control::Propagate(v)) => Err(Control::Propagate(v)),
                }
            }
```

NOTE: `call_builtin` must be `async` for this (it `.await`s `call_value`). If `call_builtin` is currently synchronous, make it `async fn call_builtin(...)` and add `.await` at its call site in `call_value`, and `#[async_recursion(?Send)]` (it now indirectly recurses: call_builtin → call_value → call_function → exec → eval_expr → ... → call_builtin). Update the call site accordingly.

- [ ] **Step 3: Tests:**

```rust
    #[tokio::test]
    async fn recover_catches_a_panic() {
        // A function that panics (index out of bounds) is recovered into [nil, err].
        let src = "
fn boom() {
  let a = [1]
  return a[10]
}
let r = recover(boom)
print(r[0])
print(r[1].message)
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "nil\n"); // r[0] is nil
        // r[1].message contains the panic text
        assert!(interp.output.contains("nil\n"));
    }

    #[tokio::test]
    async fn recover_passes_through_success() {
        let src = "
fn good() { return 42 }
let r = recover(good)
print(r[0])
print(r[1])
";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "42\nnil\n");
    }
```

(Fix the first test's assertion to check the actual message — after writing, run it and assert the real `r[1].message` content, e.g. `print(r[1].message)` then assert the output contains "out of bounds".)

- [ ] **Step 4: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 5: Commit**

```bash
git add src/interp.rs
git commit -m "feat: add recover boundary converting panics into Result values"
```

---

## Task 5: End-to-end demo + integration test

**Files:** `examples/result.as` (new), `tests/cli.rs` (modify).

- [ ] **Step 1: Create `examples/result.as`**

```
fn safeDivide(a, b) {
  if (b == 0) { return Err("division by zero") }
  return Ok(a / b)
}

fn compute(a, b, c) {
  let x = safeDivide(a, b)?
  let y = safeDivide(x, c)?
  return Ok(y)
}

let good = compute(100, 5, 2)
print(good[0])

let bad = compute(100, 0, 2)
print(bad[0])
print(bad[1].message)

fn willPanic() {
  let arr = [1, 2]
  return arr[99]
}
let recovered = recover(willPanic)
print(recovered[0])
print(recovered[1].message)
```

- [ ] **Step 2: Integration test in `tests/cli.rs`**

```rust
#[test]
fn runs_result_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg("examples/result.as").output().unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    // compute(100,5,2): 100/5=20, 20/2=10 -> good[0]=10
    assert!(out.contains("10\n"));
    // compute(100,0,2): first ? propagates -> bad[0]=nil, message "division by zero"
    assert!(out.contains("division by zero"));
    // recover catches the OOB panic
    assert!(out.contains("out of bounds"));
}
```

- [ ] **Step 3: Run** `cargo test` (incl. `runs_result_example`), then `cargo run --quiet -- run examples/result.as` (paste output). `cargo clippy --all-targets`.

- [ ] **Step 4: Commit**

```bash
git add examples/result.as tests/cli.rs
git commit -m "test: add Result/error-model end-to-end example"
```

---

## Definition of Done

- `cargo test` passes (all unit + integration); `cargo clippy --all-targets` clean.
- `cargo run -- run examples/result.as` shows the divide success (10), the `?`-propagated error ("division by zero"), and the recovered panic ("out of bounds").
- AScript supports: `Ok`/`Err`, the `?` propagation operator, `assert`, the panic tier (unrecoverable, aborts), and `recover` (panic → Result). Two error tiers per spec §6.

## Hand-off to Milestone 6 ("Gradual type contracts")

Adds type annotations on `let`/`const`, function params, and returns, checked at runtime as contracts; failed contracts are **panics** (the `Control::Panic` tier built here). The `error`/`Result<T>` annotation types (spec §5) reference the Result pair shape established here. The `Colon` token (from M4 objects) is reused for `name: Type` annotations.
