# Class Shape Validation & Force-Unwrap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add runtime-validated JSON→typed-instance support to AScript: a nullable type suffix `T?`, typed class fields with defaults, a `ClassName.from(obj, strict)` validating boundary (recursive), typed parsing (`resp.json(Class)` / `json.parse(text, Class)`), and a postfix `!` force-unwrap operator dual to `?`.

**Architecture:** Stays inside AScript's value-introspective, nominal type model. `check_type` remains a pure `(Value, Type) -> bool`. The only structural→nominal crossing is one `validate_into` core (an `Interp` method, since defaults eval lazily and nested-class names resolve via `class.def_env`); `.from` adapts it to panic, the typed-parse decoders adapt it to a `[val, err]` pair. `T?` lowers to a `Type::Optional` node (sugar for `T | nil`); `name?:` is an accepted field-only alias that lowers to the same node. Postfix `?`/`!` move from the `postfix()` loop into a new `unwrap_tier()` between `exponent()` and `unary()`, so they bind looser than `await`.

**Tech Stack:** Rust (the `ascript` interpreter), AScript (`.as` example files), tree-sitter (vendored grammar + `parser.c`), the docs static site (Markdown content + `app.js`).

**Reference spec:** `docs/superpowers/specs/2026-05-31-class-shape-validation-and-unwrap-design.md`

**Conventions for every task:**
- Run a single Rust test by substring: `cargo test <name>`. Full suite: `cargo test`. Core-only: `cargo test --no-default-features`.
- Clippy MUST be clean in BOTH configs before a phase review: `cargo clippy --all-targets` and `cargo clippy --no-default-features --all-targets`.
- Commit after each task with trailer:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- The work happens on branch `design/class-shape-validation` (already created; the spec lives there).
- Per CLAUDE.md: any new `ExprKind`/`Type` variant needs matching arms in interp (eval), `fmt.rs` (`write_expr_inner` / Display), and `ast.rs` (`Display`). After any `grammar.js` edit, regenerate `parser.c` with `tree-sitter generate --abi 14` from `docs/superpowers/specs/grammar/tree-sitter-ascript/`.

---

## Phase 1 — Nullable type suffix `T?`

Foundation: a general `Type::Optional(Box<Type>)` (sugar for `T | nil`) usable in every type position (`let`/`const`/param/return/field). Independent of classes; lands first because typed fields (Phase 3) use it.

### Task 1.1: AST `Type::Optional` variant + Display

**Files:**
- Modify: `src/ast.rs:50-66` (the `Type` enum)
- Modify: `src/ast.rs:77-106` (the `Type` `Display` impl)

- [ ] **Step 1: Add the variant.** In the `Type` enum (src/ast.rs:50-66), add a variant after `Future`:

```rust
    Future(Box<Type>),
    /// `T?` — nullable type, sugar for `T | nil`. Both `T?` and the class-field
    /// marker `name?:` lower to this node.
    Optional(Box<Type>),
```

- [ ] **Step 2: Add the Display arm.** In the `Type` `Display` impl (src/ast.rs:77-106), add after the `Future` arm:

```rust
            Type::Future(t) => write!(f, "future<{}>", t),
            Type::Optional(t) => write!(f, "{}?", t),
```

- [ ] **Step 3: Build to verify exhaustiveness.**

Run: `cargo build`
Expected: compiles (any other exhaustive `match` on `Type` that lacks `..` will error — there should be none beyond `check_type`, which Phase 1.3 handles; if `check_type` errors here, that's expected and fixed in Task 1.3).

- [ ] **Step 4: Commit.**

```bash
git add src/ast.rs
git commit -m "feat(ast): add Type::Optional (T? nullable suffix) + Display

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.2: Parser — trailing `?` produces `Type::Optional`

**Files:**
- Modify: `src/parser.rs:336-385` (`parse_type_atom`)
- Test: `src/parser.rs` (inline `#[test]`)

- [ ] **Step 1: Write the failing test.** Add to the parser tests module in `src/parser.rs` (near `await_parses_at_unary_precedence`):

```rust
    #[test]
    fn optional_type_suffix_parses() {
        // `number?` in a let binding parses to Type::Optional(Number).
        let stmts = parse(&lex("let x: number? = nil").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Let { ty: Some(t), .. } => {
                assert_eq!(t.to_string(), "number?");
            }
            other => panic!("expected a typed let, got {other:?}"),
        }
    }
```

(If the `Stmt::Let` field name for the annotation is not `ty`, adjust to match `src/ast.rs`; confirm by reading the `Stmt::Let` variant.)

- [ ] **Step 2: Run it to confirm it fails.**

Run: `cargo test optional_type_suffix_parses`
Expected: FAIL — the type renders as `number` (the `?` is left unconsumed and the `= nil` parse breaks, or assertion mismatch).

- [ ] **Step 3: Implement.** In `parse_type_atom` (src/parser.rs:336-385), the function currently returns the matched atom directly. Wrap its result so a trailing `?` produces `Optional`. Change the function body to capture the atom into a variable and apply the suffix before returning. Replace the final `match self.advance() { ... }` so its `Ok(...)` results flow through a suffix check — concretely, rename the existing body to compute `let atom = match self.advance() { ... }?;` then:

```rust
    fn parse_type_atom(&mut self) -> Result<crate::ast::Type, AsError> {
        use crate::ast::Type;
        let span = self.span();
        let atom = match self.advance() {
            Tok::Nil => Type::Nil,
            Tok::Fn => Type::Fn,
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
                Type::Tuple(parts)
            }
            Tok::Ident(name) => match name.as_str() {
                "number" => Type::Number,
                "string" => Type::String,
                "bool" => Type::Bool,
                "any" => Type::Any,
                "object" => Type::Object,
                "error" => Type::Error,
                "array" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Type::Array(Box::new(inner))
                }
                "Result" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Type::Result(Box::new(inner))
                }
                "future" => {
                    self.eat(&Tok::Lt)?;
                    let inner = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Type::Future(Box::new(inner))
                }
                "map" => {
                    self.eat(&Tok::Lt)?;
                    let k = self.parse_type()?;
                    self.eat(&Tok::Comma)?;
                    let v = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Type::Map(Box::new(k), Box::new(v))
                }
                _ => Type::Named(name),
            },
            other => return Err(AsError::at(format!("expected a type, found {:?}", other), span)),
        };
        // `T?` nullable suffix (sugar for `T | nil`). Only reachable in type
        // position (after `:` / inside `<...>`), so it never collides with the
        // expression-level `?` (ternary / propagate).
        if *self.peek() == Tok::Question {
            self.advance();
            Ok(crate::ast::Type::Optional(Box::new(atom)))
        } else {
            Ok(atom)
        }
    }
```

- [ ] **Step 4: Run it to confirm it passes.**

Run: `cargo test optional_type_suffix_parses`
Expected: PASS

- [ ] **Step 5: Add a param/return position test.**

```rust
    #[test]
    fn optional_type_in_param_and_return() {
        let stmts = parse(&lex("fn f(a: string?): number? { return nil }").unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Fn { params, ret: Some(r), .. } => {
                assert_eq!(params[0].ty.as_ref().unwrap().to_string(), "string?");
                assert_eq!(r.to_string(), "number?");
            }
            other => panic!("expected a typed fn, got {other:?}"),
        }
    }
```

(Adjust the `Stmt::Fn` field names to match `src/ast.rs` if different.)

- [ ] **Step 6: Run, then commit.**

Run: `cargo test optional_type`
Expected: PASS (both)

```bash
git add src/parser.rs
git commit -m "feat(parser): parse trailing ? as Type::Optional in any type position

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.3: Interpreter — `check_type` accepts `Optional`

**Files:**
- Modify: `src/interp.rs:1843-1901` (`check_type`)
- Test: `src/interp.rs` (inline `#[tokio::test]`)

- [ ] **Step 1: Write the failing test.** Add to the interp tests module:

```rust
    #[tokio::test]
    async fn optional_type_accepts_value_and_nil() {
        // nil and a number both satisfy number?; a string does not.
        assert_eq!(eval_to_value("let x: number? = nil\nx").await, Value::Nil);
        assert_eq!(eval_to_value("let x: number? = 7\nx").await, Value::Number(7.0));
    }

    #[tokio::test]
    async fn optional_type_rejects_wrong_type() {
        let src = "let r = recover(() => { let x: number? = \"bad\"\n return nil })\nprint(r[1].message)";
        let out = run_to_output(src).await;
        assert!(out.contains("type contract violated"), "got: {out}");
    }
```

(Use whatever existing helpers the test module provides: `eval_to_value` for a value, and a helper that captures `print` output for the recover case. If `run_to_output` does not exist, mirror the existing `contract_failure_is_recoverable` test's approach exactly.)

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test optional_type_accepts_value_and_nil`
Expected: FAIL — `check_type` has no `Optional` arm (compile error: non-exhaustive match) or wrong result.

- [ ] **Step 3: Implement.** In `check_type` (src/interp.rs:1843-1901), add an arm before the closing brace (after the `Type::Future(_)` arm):

```rust
        // A value satisfies `future<T>` iff it is a future. ...
        Type::Future(_) => matches!(value, Value::Future(_)),
        // `T?` ≡ `T | nil`.
        Type::Optional(inner) => check_type(value, inner) || matches!(value, Value::Nil),
```

- [ ] **Step 4: Run to confirm pass.**

Run: `cargo test optional_type`
Expected: PASS

- [ ] **Step 5: Commit.**

```bash
git add src/interp.rs
git commit -m "feat(interp): check_type accepts Type::Optional (T or nil)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.4: Formatter round-trip for `T?`

The formatter renders types via `render_type` → `Type::to_string()` (src/fmt.rs:59-62), which now emits `T?` from the Display arm added in Task 1.1. No formatter code change is needed; this task adds a guard test.

**Files:**
- Test: `src/fmt.rs` (inline `#[test]`)

- [ ] **Step 1: Write the test.** Add to the `fmt` tests module:

```rust
    #[test]
    fn optional_type_round_trips() {
        // `T?` survives a format pass unchanged in let/param/return positions.
        let src = "let x: number? = nil\n";
        assert_eq!(format_str(src), src);
        let src2 = "fn f(a: string?): number? {\n  return nil\n}\n";
        assert_eq!(format_str(src2), src2);
    }
```

(Use the existing format-a-string helper the `fmt` test module already uses; match its name and the exact canonical indentation/spacing by first running `cargo run -- fmt` on a scratch file if unsure.)

- [ ] **Step 2: Run.**

Run: `cargo test optional_type_round_trips`
Expected: PASS (if the canonical spacing differs, adjust the expected string to the formatter's actual output — the point is idempotence: `format_str(format_str(src)) == format_str(src)`).

- [ ] **Step 3: Commit.**

```bash
git add src/fmt.rs
git commit -m "test(fmt): T? optional type round-trips

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.5: Tree-sitter — `optional_type` rule + regen + conformance

**Files:**
- Modify: `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js:405-427` (type rules)
- Create: `examples/optional_types.as`
- Regenerate: `docs/superpowers/specs/grammar/tree-sitter-ascript/src/parser.c`

- [ ] **Step 1: Edit the grammar.** In `grammar.js`, add `$.optional_type` to the `_type_atom` choice and define the rule. Change `_type_atom` (lines 411-420) to include it, and add the rule definition after `tuple_type` (line 427):

```javascript
    _type_atom: $ => choice(
      $.optional_type,
      $.primitive_type,
      $.array_type,
      $.map_type,
      $.result_type,
      $.future_type,
      $.tuple_type,
      $.identifier, // class / enum name
    ),
    // T? — nullable suffix (sugar for `T | nil`). Reachable only inside `_type`.
    optional_type: $ => prec(PREC.postfix, seq(
      choice(
        $.primitive_type, $.array_type, $.map_type, $.result_type,
        $.future_type, $.tuple_type, $.identifier,
      ),
      '?',
    )),
```

(Note: `optional_type`'s inner is the non-recursive atoms only, to avoid left-recursion on `_type_atom`. This matches `T?` and `array<T>?`; double-optional `T??` is not meaningful and intentionally not supported.)

- [ ] **Step 2: Regenerate the parser.**

Run:
```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14 && cd -
```
Expected: succeeds with no error. If it reports a conflict involving `?` at a type boundary, add the reported conflict pair to the `conflicts` array (grammar.js:48-58) and regenerate. Record any added conflict in the commit message.

- [ ] **Step 3: Create the example.** `examples/optional_types.as`:

```javascript
// Nullable type suffix `T?` (sugar for `T | nil`) in every type position.
let a: number? = nil
let b: number? = 42
assert(a == nil, "a is nil")
assert(b == 42, "b is 42")

fn pick(x: string?): string? {
  return x
}
assert(pick(nil) == nil, "pick nil")
assert(pick("hi") == "hi", "pick hi")

print("optional_types ok")
```

- [ ] **Step 4: Run the example.**

Run: `cargo build --release && target/release/ascript run examples/optional_types.as`
Expected output: `optional_types ok`

- [ ] **Step 5: Run conformance (both parsers accept the example).**

Run: `cargo test --test treesitter_conformance && cargo test --test frontend_conformance`
Expected: PASS

- [ ] **Step 6: Commit.**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/ examples/optional_types.as
git commit -m "feat(grammar): optional_type (T?) rule; example + conformance

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 1.6: Phase 1 Review

- [ ] **Step 1: Full suite + clippy.**

Run:
```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
```
Expected: all pass; clippy clean in both configs.

- [ ] **Step 2: Independent review.** Dispatch a fresh reviewer (subagent) with: the spec §3.1–§3.2 (the `T?` parts) and the Phase 1 diff. The reviewer must (a) run the four commands above, (b) confirm `T?` parses in let/param/return/field-of-`<>` positions, (c) confirm `check_type` treats `Optional(T)` as `T | nil`, (d) confirm both parsers accept `examples/optional_types.as`, (e) probe edge cases: `array<number?>`, `map<string, number?>`, `number? | string` (union of optional). Fix any findings before proceeding.

---

## Phase 2 — Postfix `!` force-unwrap + `?`/`!` precedence restructure

Add `ExprKind::Unwrap`; move `?` (`Try`) and new `!` (`Unwrap`) from `postfix()` into a new `unwrap_tier()` between `exponent()` and `unary()` so they bind looser than `await` (`await x!` ⇒ `(await x)!`) but tighter than every binary operator (`a! + b` ⇒ `(a!) + b`).

### Task 2.1: AST `ExprKind::Unwrap` + Display

**Files:**
- Modify: `src/ast.rs:14-46` (`ExprKind`)
- Modify: `src/ast.rs:261-316` (`ExprKind` `Display`)

- [ ] **Step 1: Add the variant.** After `Try(Box<Expr>)` (src/ast.rs around line 30):

```rust
    Try(Box<Expr>),
    /// `expr!` — force-unwrap a Tier-1 `[value, err]` pair: evaluates to `value`
    /// when `err == nil`, otherwise panics (carrying the original error's
    /// message). The dual of `Try` (`?`).
    Unwrap(Box<Expr>),
```

- [ ] **Step 2: Add the Display arm.** After the `Try` arm in the `ExprKind` Display impl (src/ast.rs:261-316):

```rust
            ExprKind::Try(e) => write!(f, "(? {})", e),
            ExprKind::Unwrap(e) => write!(f, "(unwrap {})", e),
```

- [ ] **Step 3: Build.**

Run: `cargo build`
Expected: fails ONLY in `src/interp.rs` (eval) and `src/fmt.rs` (write_expr_inner) with non-exhaustive `match` on `ExprKind` — those are handled in Tasks 2.3 and 2.4. (`ast.rs` itself compiles.) Confirm the errors are exactly those two files; if any other file matches `ExprKind` exhaustively, note it for that task.

- [ ] **Step 4: Commit** (allow the two known downstream errors to be fixed in later tasks; commit ast.rs alone after `cargo build -p` of the crate is not split — instead stage only ast.rs and commit; the tree will not build until 2.3/2.4, which is acceptable mid-phase).

```bash
git add src/ast.rs
git commit -m "feat(ast): add ExprKind::Unwrap (postfix !) + Display

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 2.2: Parser — `unwrap_tier()`, move `?`, wire `exponent()`

**Files:**
- Modify: `src/parser.rs` (the `exponent()` function; `postfix()` at 950-1026; add `unwrap_tier()`)
- Test: `src/parser.rs` (inline)

- [ ] **Step 1: Write failing tests.** Add to the parser tests module:

```rust
    #[test]
    fn unwrap_and_propagate_bind_looser_than_await() {
        // `!` and `?` apply to the resolved value, not the future.
        assert_eq!(sexpr("await f()!"), "(unwrap (await (call f)))");
        assert_eq!(sexpr("await f()?"), "(? (await (call f)))");
    }

    #[test]
    fn unwrap_binds_tighter_than_binary() {
        assert_eq!(sexpr("a! + b"), "(+ (unwrap a) b)");
        assert_eq!(sexpr("f()?"), "(? (call f))");
    }

    #[test]
    fn ternary_still_disambiguates_from_propagate() {
        // A `?` followed by `:` is still a ternary, not a Try.
        assert_eq!(sexpr("a ? b : c"), "(?: a b c)");
        assert_eq!(sexpr("g()? ? a : b"), "(?: (? (call g)) a b)");
    }
```

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test unwrap_and_propagate_bind_looser_than_await`
Expected: FAIL — currently `await f()!` errors (no postfix `!`) or `?` is parsed inside postfix (giving `(await (? (call f)))`).

- [ ] **Step 3: Add `unwrap_tier()`.** Insert this new function in `src/parser.rs` (place it directly above `unary()` at line 921):

```rust
    /// Postfix Result operators `?` (propagate) and `!` (force-unwrap). They sit
    /// LOOSER than `await`/unary but TIGHTER than every binary operator, so
    /// `await x!` parses as `(await x)!` and `a! + b` as `(a!) + b`. Left-assoc.
    fn unwrap_tier(&mut self) -> Result<Expr, AsError> {
        let mut expr = self.unary()?;
        loop {
            match self.peek() {
                Tok::Question => {
                    // Leave a ternary `?` for `ternary()` higher up.
                    if self.question_begins_ternary() {
                        break;
                    }
                    self.advance();
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::Try(Box::new(expr)), span };
                }
                Tok::Bang => {
                    self.advance();
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::Unwrap(Box::new(expr)), span };
                }
                _ => break,
            }
        }
        Ok(expr)
    }
```

- [ ] **Step 4: Wire `exponent()` to call `unwrap_tier()`.** In `exponent()`, change the base operand from `self.unary()?` to `self.unwrap_tier()?`:

```rust
    fn exponent(&mut self) -> Result<Expr, AsError> {
        let base = self.unwrap_tier()?;
        if *self.peek() == Tok::StarStar {
            self.advance();
            let exp = self.exponent()?; // right-associative
            Ok(Self::make_binary(base, BinOp::Pow, exp))
        } else {
            Ok(base)
        }
    }
```

- [ ] **Step 5: Remove the `?` arm from `postfix()`.** In `postfix()` (src/parser.rs:950-1026), delete the entire `Tok::Question => { ... }` arm (the block that builds `ExprKind::Try`). The `postfix()` loop must now handle only `LParen`, `LBracket`, `Dot`, `QuestionDot`, and `_ => break`.

- [ ] **Step 6: Confirm `self.unary()` has no other callers needing the change.**

Run: `grep -n "self.unary()" src/parser.rs`
Expected: callers are (a) inside `unary()` itself (prefix-operator operands — these STAY as `unary()`), and (b) the one in `exponent()` you just changed. If any OTHER caller exists, evaluate whether it should bind below the unwrap tier; for this grammar only `exponent()` should change.

- [ ] **Step 7: Run the tests.**

Run: `cargo test unwrap_ ; cargo test ternary_still_disambiguates ; cargo test await_parses_at_unary_precedence`
Expected: PASS (including the pre-existing `await_parses_at_unary_precedence` — `await f()` ⇒ `(await (call f))`, `await a + b` ⇒ `(+ (await a) b)` are unchanged).

- [ ] **Step 8: Commit.**

```bash
git add src/parser.rs
git commit -m "feat(parser): unwrap_tier — postfix ! and ? looser than await

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 2.3: Interpreter — eval `ExprKind::Unwrap`

**Files:**
- Modify: `src/interp.rs` (add an `eval_expr` arm next to `ExprKind::Try` at 980-1002)
- Test: `src/interp.rs` (inline)

- [ ] **Step 1: Write failing tests.**

```rust
    #[tokio::test]
    async fn unwrap_returns_value_on_ok_pair() {
        assert_eq!(eval_to_value("[42, nil]!").await, Value::Number(42.0));
        assert_eq!(eval_to_value("Ok(7)!").await, Value::Number(7.0));
    }

    #[tokio::test]
    async fn unwrap_panics_on_err_pair_preserving_message() {
        // `!` on an error pair panics; recover round-trips the original message.
        let src = "let r = recover(() => Err(\"boom\")!)\nprint(r[1].message)";
        let out = run_to_output(src).await;
        assert!(out.contains("boom"), "got: {out}");
    }

    #[tokio::test]
    async fn unwrap_on_non_pair_is_a_panic() {
        let src = "let r = recover(() => 5!)\nprint(r[1] != nil)";
        let out = run_to_output(src).await;
        assert!(out.contains("true"), "got: {out}");
    }
```

(Match the test module's actual output-capture helper; mirror `contract_failure_is_recoverable`.)

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test unwrap_returns_value_on_ok_pair`
Expected: FAIL — non-exhaustive match in `eval_expr` (compile error) until the arm is added.

- [ ] **Step 3: Implement.** Add this arm directly after the `ExprKind::Try` arm (src/interp.rs:980-1002):

```rust
            ExprKind::Unwrap(inner) => {
                let v = self.eval_expr(inner, env).await?;
                let arr = match &v {
                    Value::Array(a) if a.borrow().len() == 2 => a.clone(),
                    _ => {
                        return Err(AsError::at(
                            "the ! operator requires a Result pair [value, err]",
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
                    // Promote the Tier-1 error to a Tier-2 panic, preserving the
                    // original error's message so `recover` round-trips it.
                    let msg = match &err {
                        Value::Object(o) => match o.borrow().get("message") {
                            Some(Value::Str(s)) => s.to_string(),
                            _ => err.to_string(),
                        },
                        Value::Str(s) => s.to_string(),
                        _ => err.to_string(),
                    };
                    Err(AsError::at(msg, expr.span).into())
                }
            }
```

- [ ] **Step 4: Run.**

Run: `cargo test unwrap_`
Expected: PASS (all three)

- [ ] **Step 5: Commit.**

```bash
git add src/interp.rs
git commit -m "feat(interp): eval ExprKind::Unwrap (force-unwrap, panic-preserving msg)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 2.4: Formatter — `Unwrap` arm + `Try`/`Unwrap` inner precedence

**Files:**
- Modify: `src/fmt.rs:302-327` (`expr_prec`)
- Modify: `src/fmt.rs:466-469` (the `Try` arm) and add an `Unwrap` arm in `write_expr_inner`
- Test: `src/fmt.rs` (inline)

- [ ] **Step 1: Write failing tests.**

```rust
    #[test]
    fn unwrap_and_await_format_without_parens() {
        // `await x!` and `await x?` are canonical (no parens) — `!`/`?` are
        // looser than await, so the grouping is implicit.
        assert_eq!(format_expr("await f()!"), "await f()!");
        assert_eq!(format_expr("await f()?"), "await f()?");
        assert_eq!(format_expr("a! + b"), "a! + b");
        assert_eq!(format_expr("(a + b)!"), "(a + b)!");
    }
```

(Use the `fmt` test module's expression-formatting helper; if it formats whole statements, wrap as `let x = <expr>` and compare the RHS, or use the existing helper convention.)

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test unwrap_and_await_format_without_parens`
Expected: FAIL — non-exhaustive `match` in `write_expr_inner` (no `Unwrap` arm) → compile error; and/or `await f()?` formats as `(await f())?`.

- [ ] **Step 3: Update `expr_prec`.** In `expr_prec` (src/fmt.rs:302-327), add `Unwrap` alongside `Try` in the postfix group (both are tight as children, so they never get wrongly parenthesized inside binary/call contexts):

```rust
        ExprKind::Call { .. }
        | ExprKind::Index { .. }
        | ExprKind::Member { .. }
        | ExprKind::OptMember { .. }
        | ExprKind::Try(_)
        | ExprKind::Unwrap(_) => PREC_POSTFIX,
```

- [ ] **Step 4: Update the `Try` arm and add the `Unwrap` arm.** In `write_expr_inner`, replace the `Try` arm (src/fmt.rs:466-469) and add `Unwrap`. The inner operand is written at `PREC_UNARY` (not `PREC_POSTFIX`): this is the fix that makes `await x?` / `await x!` render WITHOUT parens (an `Await`/`Unary` inner at prec 10 is not `< PREC_UNARY`), while a binary inner (prec < 10) still gets parenthesized:

```rust
        ExprKind::Try(inner) => {
            write_expr(out, inner, PREC_UNARY);
            out.push('?');
        }
        ExprKind::Unwrap(inner) => {
            write_expr(out, inner, PREC_UNARY);
            out.push('!');
        }
```

(Do NOT introduce a separate low `PREC_TRY` constant for `expr_prec`: making `Try`/`Unwrap` low-precedence as children would wrongly parenthesize `a? + b` → `(a?) + b`. The correct change is only the inner write precedence above.)

- [ ] **Step 5: Run.**

Run: `cargo test unwrap_and_await_format_without_parens && cargo test --test cli`
Expected: PASS. Also confirm idempotence on existing examples: `cargo run -- fmt --check` is not a flag here; instead the conformance/example runs in later tasks guard this.

- [ ] **Step 6: Commit.**

```bash
git add src/fmt.rs
git commit -m "feat(fmt): format Unwrap; ?/! inner at unary prec (no spurious parens)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 2.5: Tree-sitter — `unwrap_expression` + propagate precedence + regen

**Files:**
- Modify: `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js` (PREC, `_postfix_expression`, `propagate_expression`, add `unwrap_expression`, `conflicts`)
- Create: `examples/force_unwrap.as`
- Regenerate `parser.c`

- [ ] **Step 1: Edit the grammar.** Add an `unwrap` precedence tier. The postfix `?`/`!` must bind looser than `unary` but tighter than every binary operator (the tightest of which is `exp`). Tree-sitter precedences MUST be integers, so insert `unwrap` directly above `exp` and shift `unary`/`postfix`/`primary` up by one. Replace the tail of the `PREC` object (grammar.js:20-35) from `exp` onward:

```javascript
  exp: 11,    // right-associative
  unwrap: 12, // postfix ? and ! — looser than unary/await, tighter than binary
  unary: 13,
  postfix: 14, // call, member, index, optional-member
  primary: 15,
```

(All rules reference `PREC.*` symbolically, so renumbering is transparent. Note `optional_type` from Task 1.5 used `PREC.postfix` — still valid, now 14.)

Add `$.unwrap_expression` to `_postfix_expression` (grammar.js:315-322) and give `propagate_expression` the `unwrap` precedence:

```javascript
    _postfix_expression: $ => choice(
      $.call_expression,
      $.member_expression,
      $.optional_member_expression,
      $.index_expression,
      $.unwrap_expression,
      $.propagate_expression,
      $._primary_expression,
    ),
```

Replace `propagate_expression` (grammar.js:348-358) and add `unwrap_expression` after it:

```javascript
    propagate_expression: $ => prec(PREC.unwrap, seq(
      field('operand', $._postfix_expression),
      '?',
    )),
    // expr! — force-unwrap (dual of ?). Position-disambiguated from prefix `!`
    // (operand precedes it) and from `!=` (a single token).
    unwrap_expression: $ => prec(PREC.unwrap, seq(
      field('operand', $._postfix_expression),
      '!',
    )),
```

- [ ] **Step 2: Regenerate.**

Run:
```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14 && cd -
```
Expected: succeeds. The existing `[$._expression, $.propagate_expression]` conflict (grammar.js:48-58) must remain. If generate reports a new conflict (e.g. involving `unwrap_expression`), add the reported pair to `conflicts` and regenerate. Record additions in the commit message.

- [ ] **Step 3: Create the example.** `examples/force_unwrap.as`:

```javascript
// Postfix `!` force-unwrap (dual of `?`), and its interaction with await/recover.
fn half(n) {
  if (n % 2 != 0) { return Err("odd") }
  return Ok(n / 2)
}

// `!` unwraps a Result pair; on a value pair it yields the value.
assert(half(8)! == 4, "half(8)! == 4")

// On an error pair, `!` panics; `recover` round-trips the message.
let r = recover(() => half(3)!)
assert(r[1] != nil, "half(3)! panics")
assert(r[1].message == "odd", "message preserved")

print("force_unwrap ok")
```

- [ ] **Step 4: Run the example.**

Run: `cargo build --release && target/release/ascript run examples/force_unwrap.as`
Expected output: `force_unwrap ok`

- [ ] **Step 5: Conformance.**

Run: `cargo test --test treesitter_conformance && cargo test --test frontend_conformance`
Expected: PASS

- [ ] **Step 6: Commit.**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/ examples/force_unwrap.as
git commit -m "feat(grammar): unwrap_expression (postfix !); ? at unwrap precedence

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 2.6: Phase 2 Review

- [ ] **Step 1: Full suite + clippy** (all four commands from Task 1.6). Expected: all pass.
- [ ] **Step 2: Independent review.** Reviewer gets spec §5 and the Phase 2 diff. Must: run the four commands; confirm `await f()!` ⇒ `(await f())!`, `a! + b` ⇒ `(a!) + b`, ternary still parses, `f()?` unchanged; confirm `!` on an error pair panics and `recover` yields the original message; confirm both parsers accept `examples/force_unwrap.as`; probe `!` on a non-pair, chained `f()!?`, and that `a != b` still parses as inequality (not unwrap). Fix findings.

---

## Phase 3 — Typed class fields

Add `FieldDecl` to `Stmt::Class`, store a `FieldSchema` on the runtime `Class`, parse both `name?: T` and `name: T?`, check declared-field types on assignment (incl. inside `init`), apply defaults at construction, format fields (canonical `name: T?`, fields before methods), grammar, and LSP `PROPERTY` symbols.

### Task 3.1: AST — `FieldDecl` + `fields` on `Stmt::Class`

**Files:**
- Modify: `src/ast.rs:161-167` (`Stmt::Class`)
- Modify: `src/ast.rs` (add `FieldDecl` struct near `MethodDecl` at 178-190)

- [ ] **Step 1: Add the struct.** Near `MethodDecl` (src/ast.rs:178-190):

```rust
#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub name: String,
    pub ty: Type,
    /// Lazily-evaluated default (in the class def env) when the field is absent.
    pub default: Option<Expr>,
    pub span: Span,
    pub name_span: Span,
}
```

- [ ] **Step 2: Add `fields` to `Stmt::Class`** (src/ast.rs:161-167):

```rust
    Class {
        name: String,
        superclass: Option<String>,
        fields: Vec<FieldDecl>,
        methods: Vec<MethodDecl>,
        span: Span,
        name_span: Span,
    },
```

- [ ] **Step 3: Build.**

Run: `cargo build`
Expected: fails at the `Stmt::Class { ... }` construction in `src/parser.rs` (missing `fields`) — fixed in Task 3.2. Matches on `Stmt::Class` that use `..` (interp, fmt, lsp) still compile. Confirm the only construction error is in parser.

- [ ] **Step 4: Commit.**

```bash
git add src/ast.rs
git commit -m "feat(ast): FieldDecl + Stmt::Class.fields

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.2: Parser — class-body field declarations (both spellings)

**Files:**
- Modify: `src/parser.rs:248-311` (`class_decl`)
- Test: `src/parser.rs` (inline)

- [ ] **Step 1: Write failing tests.**

```rust
    #[test]
    fn class_fields_both_spellings_parse() {
        let src = "class U {\n  id: number\n  nick: string?\n  avatar?: string\n  role: string = \"guest\"\n  fn init() {}\n}";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        match &stmts[0] {
            Stmt::Class { fields, methods, .. } => {
                assert_eq!(fields.len(), 4);
                assert_eq!(fields[0].name, "id");
                assert_eq!(fields[0].ty.to_string(), "number");
                // `string?` and `avatar?` both lower to Optional.
                assert_eq!(fields[1].ty.to_string(), "string?");
                assert_eq!(fields[2].name, "avatar");
                assert_eq!(fields[2].ty.to_string(), "string?");
                // default present
                assert!(fields[3].default.is_some());
                assert_eq!(methods.len(), 1);
            }
            other => panic!("expected a class, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test class_fields_both_spellings_parse`
Expected: FAIL (currently the class body only parses methods; `id: number` is unexpected).

- [ ] **Step 3: Implement.** In `class_decl` (src/parser.rs:248-311), the body loop currently assumes every member starts with `async`/`fn`. Replace the body loop so each member is either a field declaration or a method. A member is a method iff it starts with `async` or `fn`; otherwise it is a field declaration `Ident ["?"] ":" type ["=" expr]`. Replace the `let mut methods = Vec::new();` block and the `while` loop with:

```rust
        self.eat(&Tok::LBrace)?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            // A member starting with `async` or `fn` is a method; otherwise a field.
            if *self.peek() == Tok::Async || *self.peek() == Tok::Fn {
                let mstart = self.span().start;
                let is_async = if *self.peek() == Tok::Async {
                    self.advance();
                    true
                } else {
                    false
                };
                self.eat(&Tok::Fn)?;
                let is_generator = if *self.peek() == Tok::Star {
                    self.advance();
                    true
                } else {
                    false
                };
                let mname_span = self.span();
                let mname = match self.advance() {
                    Tok::Ident(n) => n,
                    other => return Err(AsError::at(format!("expected method name, found {:?}", other), self.tokens[self.pos - 1].span)),
                };
                let params = self.param_list()?;
                let ret = if *self.peek() == Tok::Colon {
                    self.advance();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                let body = self.block()?;
                let mspan = Span::new(mstart, self.prev_end());
                methods.push(crate::ast::MethodDecl {
                    name: mname,
                    params,
                    ret,
                    body,
                    is_async,
                    is_generator,
                    span: mspan,
                    name_span: mname_span,
                });
            } else {
                // Field declaration: Ident ["?"] ":" type ["=" expr]
                let fstart = self.span().start;
                let fname_span = self.span();
                let fname = match self.advance() {
                    Tok::Ident(n) => n,
                    other => return Err(AsError::at(format!("expected a field name or method, found {:?}", other), self.tokens[self.pos - 1].span)),
                };
                // `name?:` marker — lower to Optional below.
                let marker_optional = if *self.peek() == Tok::Question {
                    self.advance();
                    true
                } else {
                    false
                };
                self.eat(&Tok::Colon)?;
                let mut ty = self.parse_type()?;
                if marker_optional && !matches!(ty, crate::ast::Type::Optional(_)) {
                    ty = crate::ast::Type::Optional(Box::new(ty));
                }
                let default = if *self.peek() == Tok::Eq {
                    self.advance();
                    Some(self.expr()?)
                } else {
                    None
                };
                let fspan = Span::new(fstart, self.prev_end());
                fields.push(crate::ast::FieldDecl { name: fname, ty, default, span: fspan, name_span: fname_span });
            }
        }
        self.eat(&Tok::RBrace)?;
        let span = Span::new(start, self.prev_end());
        Ok(Stmt::Class { name, superclass, fields, methods, span, name_span })
```

(Note: a field declaration must be terminated by the next member or `}` — there is no separator token required, matching the method style. `name?: T` sets `marker_optional`; `name: T?` already yields `Type::Optional`; the `!matches!` guard avoids double-wrapping if someone writes `name?: T?`.)

- [ ] **Step 4: Run.**

Run: `cargo test class_fields_both_spellings_parse`
Expected: PASS

- [ ] **Step 5: Commit.**

```bash
git add src/parser.rs
git commit -m "feat(parser): parse class field declarations (name?: T and name: T?)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.3: Runtime `Class.fields` schema + populate in `Stmt::Class` exec

**Files:**
- Modify: `src/value.rs:72-77` (`Class` struct; add `FieldSchema`)
- Modify: `src/interp.rs:720-746` (`Stmt::Class` execution)

- [ ] **Step 1: Add `FieldSchema` + `fields` to `Class`.** In `src/value.rs`, near the `Class` struct (72-77):

```rust
#[derive(Clone)]
pub struct FieldSchema {
    pub ty: crate::ast::Type,
    pub default: Option<crate::ast::Expr>,
}

pub struct Class {
    pub name: String,
    pub superclass: Option<Rc<Class>>,
    pub fields: IndexMap<String, FieldSchema>,
    pub methods: IndexMap<String, Rc<Method>>,
    pub def_env: Environment,
}
```

- [ ] **Step 2: Populate it in the interpreter.** In `Stmt::Class` exec (src/interp.rs:720-746), destructure `fields` and build the schema map. Replace the arm:

```rust
            Stmt::Class { name, superclass, fields, methods, .. } => {
                let parent = match superclass {
                    Some(sup_name) => match env.get(sup_name) {
                        Some(Value::Class(c)) => Some(c),
                        Some(_) => return Err(AsError::new(format!("'{}' is not a class", sup_name)).into()),
                        None => return Err(AsError::new(format!("undefined superclass '{}'", sup_name)).into()),
                    },
                    None => None,
                };
                let mut field_map = indexmap::IndexMap::new();
                for fd in fields {
                    field_map.insert(
                        fd.name.clone(),
                        crate::value::FieldSchema { ty: fd.ty.clone(), default: fd.default.clone() },
                    );
                }
                let mut method_map = indexmap::IndexMap::new();
                for m in methods {
                    method_map.insert(m.name.clone(), std::rc::Rc::new(crate::value::Method {
                        params: m.params.clone(),
                        ret: m.ret.clone(),
                        body: m.body.clone(),
                        is_async: m.is_async,
                    }));
                }
                let class = Value::Class(std::rc::Rc::new(crate::value::Class {
                    name: name.clone(),
                    superclass: parent,
                    fields: field_map,
                    methods: method_map,
                    def_env: env.clone(),
                }));
                env.define(name, class, false).map_err(AsError::new)?;
                Ok(Flow::Normal)
            }
```

- [ ] **Step 3: Build.**

Run: `cargo build`
Expected: compiles (no behavior change yet).

- [ ] **Step 4: Commit.**

```bash
git add src/value.rs src/interp.rs
git commit -m "feat(value): Class.fields schema; populate on class definition

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.4: Interpreter — check declared field types on assignment + defaults at construction

**Files:**
- Modify: `src/interp.rs:1756-1769` (member-assignment for `Value::Instance`)
- Modify: `src/interp.rs:1495-1527` (`construct`)
- Test: `src/interp.rs` (inline)

- [ ] **Step 1: Write failing tests.**

```rust
    #[tokio::test]
    async fn declared_field_type_checked_on_assignment() {
        // Assigning a wrong-typed declared field panics (recoverable).
        let src = "class C { id: number\n fn init(v) { self.id = v } }\n\
                   let r = recover(() => C(\"bad\"))\nprint(r[1].message)";
        let out = run_to_output(src).await;
        assert!(out.contains("type contract violated"), "got: {out}");
    }

    #[tokio::test]
    async fn declared_field_default_applied_at_construction() {
        let src = "class C { role: string = \"guest\"\n fn init() {} }\n\
                   let c = C()\nprint(c.role)";
        let out = run_to_output(src).await;
        assert!(out.contains("guest"), "got: {out}");
    }

    #[tokio::test]
    async fn undeclared_field_stays_dynamic() {
        // A field the class did not declare is unchecked.
        let src = "class C { fn init() { self.x = 1\n self.x = \"now a string\" } }\n\
                   let c = C()\nprint(c.x)";
        let out = run_to_output(src).await;
        assert!(out.contains("now a string"), "got: {out}");
    }
```

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test declared_field_type_checked_on_assignment`
Expected: FAIL (assignment is currently unchecked; `default_applied` fails too).

- [ ] **Step 3: Add a field-schema lookup helper.** Add a free function in `src/interp.rs` (near `check_type`):

```rust
/// Look up the declared schema for `field` on `class` or any superclass.
fn lookup_field_schema(
    class: &std::rc::Rc<crate::value::Class>,
    field: &str,
) -> Option<crate::value::FieldSchema> {
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        if let Some(s) = c.fields.get(field) {
            return Some(s.clone());
        }
        cur = c.superclass.clone();
    }
    None
}
```

- [ ] **Step 4: Check on assignment.** In the member-assignment arm for `Value::Instance` (src/interp.rs:1756-1769), check the declared type before inserting:

```rust
                Value::Instance(inst) => {
                    let class = inst.borrow().class.clone();
                    if let Some(schema) = lookup_field_schema(&class, name) {
                        if !check_type(&value, &schema.ty) {
                            return Err(contract_panic(&schema.ty, &value, object.span));
                        }
                    }
                    inst.borrow_mut().fields.insert(name.clone(), value.clone());
                    Ok(value)
                }
```

(Note: do not hold the `inst.borrow()` across the insert — `class` is cloned out first.)

- [ ] **Step 5: Apply defaults at construction.** In `construct` (src/interp.rs:1495-1527), after creating the instance and BEFORE running `init`, pre-populate declared fields that have a default (collecting base-class-first so subclass defaults override). Insert after `let inst_val = Value::Instance(instance);`:

```rust
        // Pre-populate declared-field defaults (base-class first so a subclass
        // default overrides). `init` may then override; .from (Task 4) handles
        // its own defaults. Defaults eval lazily in the class def env.
        {
            let mut chain = Vec::new();
            let mut cur = Some(class.clone());
            while let Some(c) = cur {
                chain.push(c.clone());
                cur = c.superclass.clone();
            }
            for c in chain.into_iter().rev() {
                for (fname, schema) in &c.fields {
                    if let Some(def) = &schema.default {
                        let dv = self.eval_expr(def, &c.def_env).await?;
                        if !check_type(&dv, &schema.ty) {
                            return Err(contract_panic(&schema.ty, &dv, span));
                        }
                        if let Value::Instance(i) = &inst_val {
                            i.borrow_mut().fields.insert(fname.clone(), dv);
                        }
                    }
                }
            }
        }
```

(Confirm `construct` is `async` and has `self` — it is, per src/interp.rs:1495. Do not hold the `i.borrow_mut()` across the `.await` for the default eval: the eval happens into `dv` first, then the borrow+insert is synchronous.)

- [ ] **Step 6: Run.**

Run: `cargo test declared_field_ ; cargo test undeclared_field_stays_dynamic`
Expected: PASS (all three)

- [ ] **Step 7: Commit.**

```bash
git add src/interp.rs
git commit -m "feat(interp): check declared field types on assign; apply defaults at construct

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.5: Formatter — `write_field` + class arm (fields before methods, canonical `name: T?`)

**Files:**
- Modify: `src/fmt.rs:203-217` (`Stmt::Class` arm)
- Modify: `src/fmt.rs` (add `write_field`)
- Test: `src/fmt.rs` (inline)

- [ ] **Step 1: Write failing tests.**

```rust
    #[test]
    fn class_fields_format_canonically() {
        // `name?: T` normalizes to `name: T?`; fields print before methods.
        let src = "class U {\n  id: number\n  nick?: string\n  role: string = \"guest\"\n  fn init() {}\n}\n";
        let want = "class U {\n  id: number\n  nick: string?\n  role: string = \"guest\"\n  fn init() {}\n}\n";
        assert_eq!(format_str(src), want);
    }
```

(Confirm the formatter's canonical indentation by running `cargo run -- fmt` on a scratch class; adjust `want` to match exact spacing if the body indent differs.)

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test class_fields_format_canonically`
Expected: FAIL — non-exhaustive isn't an issue (the arm uses `..`), but fields are dropped (not emitted) → output lacks them.

- [ ] **Step 3: Add `write_field`.** In `src/fmt.rs`, near `write_method` (259-274):

```rust
fn write_field(out: &mut String, fd: &crate::ast::FieldDecl, level: usize) {
    indent(out, level);
    out.push_str(&fd.name);
    out.push_str(": ");
    out.push_str(&render_type(&fd.ty)); // Type::Optional renders as `T?` (canonical)
    if let Some(def) = &fd.default {
        out.push_str(" = ");
        write_expr(out, def, PREC_ASSIGN);
    }
    out.push('\n');
}
```

- [ ] **Step 4: Emit fields in the class arm.** In the `Stmt::Class` arm (src/fmt.rs:203-217), destructure `fields` and emit them before methods:

```rust
                Stmt::Class { name, superclass, fields, methods, .. } => {
                    indent(out, level);
                    out.push_str("class ");
                    out.push_str(name);
                    if let Some(sup) = superclass {
                        out.push_str(" extends ");
                        out.push_str(sup);
                    }
                    out.push_str(" {\n");
                    for fd in fields {
                        write_field(out, fd, level + 1);
                    }
                    for m in methods {
                        write_method(out, m, level + 1);
                    }
                    indent(out, level);
                    out.push_str("}\n");
                }
```

(The `name?: T` → `name: T?` normalization is automatic: the parser lowered the marker to `Type::Optional`, and `render_type` prints `T?`.)

- [ ] **Step 5: Run.**

Run: `cargo test class_fields_format_canonically`
Expected: PASS

- [ ] **Step 6: Commit.**

```bash
git add src/fmt.rs
git commit -m "feat(fmt): emit class field declarations (canonical name: T?, before methods)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.6: Tree-sitter — `field_declaration` in class bodies + regen

**Files:**
- Modify: `docs/superpowers/specs/grammar/tree-sitter-ascript/grammar.js:155-171` (class rules)
- Create: `examples/typed_fields.as`
- Regenerate `parser.c`

- [ ] **Step 1: Edit the grammar.** Replace `class_body` (grammar.js:162) and add `class_member` + `field_declaration`:

```javascript
    class_body: $ => seq('{', repeat($.class_member), '}'),
    class_member: $ => choice($.field_declaration, $.method_definition),
    field_declaration: $ => seq(
      field('name', $.identifier),
      optional('?'),                    // `name?:` marker (lowers to T | nil)
      ':',
      field('type', $._type),           // also covers `name: T?`
      optional(seq('=', field('default', $._expression))),
    ),
```

- [ ] **Step 2: Regenerate.**

Run:
```bash
cd docs/superpowers/specs/grammar/tree-sitter-ascript && tree-sitter generate --abi 14 && cd -
```
Expected: succeeds. A method starts with `async`/`fn`; a field starts with an identifier — disjoint first tokens, so no conflict is expected. If one is reported, add it to `conflicts` and regenerate; record it.

- [ ] **Step 3: Create the example.** `examples/typed_fields.as`:

```javascript
// Typed class fields: required, optional (T?), and defaulted.
class User {
  id: number
  name: string
  nickname: string?       // optional
  role: string = "guest"  // optional with default
  fn init(id, name) {
    self.id = id
    self.name = name
  }
}

let u = User(1, "Ada")
assert(u.id == 1, "id")
assert(u.name == "Ada", "name")
assert(u.nickname == nil, "nickname defaults to nil")
assert(u.role == "guest", "role default applied")

print("typed_fields ok")
```

- [ ] **Step 4: Run the example.**

Run: `cargo build --release && target/release/ascript run examples/typed_fields.as`
Expected output: `typed_fields ok`

- [ ] **Step 5: Conformance.**

Run: `cargo test --test treesitter_conformance && cargo test --test frontend_conformance`
Expected: PASS

- [ ] **Step 6: Commit.**

```bash
git add docs/superpowers/specs/grammar/tree-sitter-ascript/ examples/typed_fields.as
git commit -m "feat(grammar): class field declarations; example + conformance

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.7: LSP — declared fields as `PROPERTY` symbols

**Files:**
- Modify: `src/lsp/analysis.rs:156-175` (`document_symbols` `Stmt::Class` arm)
- Test: `tests/lsp.rs`

- [ ] **Step 1: Write the failing test.** Add to `tests/lsp.rs` a document-symbols assertion that a class with fields yields `PROPERTY` children. Follow the existing `tests/lsp.rs` pattern for invoking document symbols on a source string; assert the class symbol's children include the field names with `SymbolKind::PROPERTY`. (If `tests/lsp.rs` drives the server over stdio, mirror an existing symbol test exactly; if it calls `analysis::document_symbols` directly, assert on the returned tree.)

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test --test lsp`
Expected: FAIL — fields are not emitted as symbols.

- [ ] **Step 3: Implement.** Replace the `Stmt::Class` arm (src/lsp/analysis.rs:156-175):

```rust
        Stmt::Class { name, fields, methods, span, name_span, .. } => {
            let mut children: Vec<DocumentSymbol> = fields
                .iter()
                .map(|fd| {
                    symbol(
                        fd.name.clone(),
                        SymbolKind::PROPERTY,
                        span_range(fd.span, index),
                        span_range(fd.name_span, index),
                        None,
                    )
                })
                .collect();
            children.extend(methods.iter().map(|m| {
                symbol(
                    m.name.clone(),
                    SymbolKind::METHOD,
                    span_range(m.span, index),
                    span_range(m.name_span, index),
                    None,
                )
            }));
            out.push(symbol(
                name.clone(),
                SymbolKind::CLASS,
                span_range(*span, index),
                span_range(*name_span, index),
                Some(children),
            ));
        }
```

- [ ] **Step 4: Run.**

Run: `cargo test --test lsp`
Expected: PASS

- [ ] **Step 5: Commit.**

```bash
git add src/lsp/analysis.rs tests/lsp.rs
git commit -m "feat(lsp): class fields as PROPERTY document symbols

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 3.8: Phase 3 Review

- [ ] **Step 1: Full suite + clippy** (four commands). Expected: pass.
- [ ] **Step 2: Independent review.** Reviewer gets spec §3 and the Phase 3 diff. Must: run the four commands; confirm both field spellings parse and format to canonical `name: T?`; confirm assignment type-checking fires inside `init` and is recoverable; confirm undeclared fields stay dynamic; confirm defaults apply at construction and are themselves type-checked; confirm fields appear as LSP `PROPERTY` symbols; confirm `examples/typed_fields.as` runs and both parsers accept it; probe inheritance (a subclass field + a superclass field). Fix findings.

---

## Phase 4 — `ClassName.from(obj, strict)` validating boundary

The structural→nominal crossing. One non-panicking `validate_into` core (recurses into nested class / `array<Class>` / `map<K,Class>`), exposed as a class associated function via a new `Value::ClassMethod` (mirroring `Value::GeneratorMethod`).

### Task 4.1: `validate_into` core + `coerce_field` recursion

**Files:**
- Modify: `src/interp.rs` (add `validate_into` and `coerce_field` methods)

- [ ] **Step 1: Add the core methods.** Add to the `impl Interp` block in `src/interp.rs` (near `construct`). These do not panic; they return `Result<Value, AsError>`:

```rust
    /// Validate a raw object against a class's declared fields, producing a
    /// checked instance. Recurses into nested class / array<Class> / map<K,Class>
    /// fields. Does NOT run `init`. Non-panicking: returns Err on mismatch.
    #[async_recursion::async_recursion(?Send)]
    async fn validate_into(
        &self,
        class: &std::rc::Rc<crate::value::Class>,
        obj: &Value,
        strict: bool,
        path: &str,
        span: Span,
    ) -> Result<Value, AsError> {
        let map = match obj {
            Value::Object(m) => m.clone(),
            _ => {
                return Err(AsError::at(
                    format!("{} expects an object, got {}", display_path(path, &class.name), type_name(obj)),
                    span,
                ))
            }
        };
        // Declared fields, base-class first (subclass last so it wins on name clash).
        let mut chain = Vec::new();
        let mut cur = Some(class.clone());
        while let Some(c) = cur {
            chain.push(c.clone());
            cur = c.superclass.clone();
        }
        let mut schema: indexmap::IndexMap<String, crate::value::FieldSchema> = indexmap::IndexMap::new();
        for c in chain.into_iter().rev() {
            for (n, s) in &c.fields {
                schema.insert(n.clone(), s.clone());
            }
        }

        let mut inst_fields = indexmap::IndexMap::new();
        for (fname, fs) in &schema {
            let field_path = if path.is_empty() {
                format!("{}.{}", class.name.to_lowercase(), fname)
            } else {
                format!("{}.{}", path, fname)
            };
            let raw = map.borrow().get(fname).cloned();
            let mut val = raw.unwrap_or(Value::Nil);
            if val == Value::Nil {
                if let Some(def) = &fs.default {
                    val = self
                        .eval_expr(def, &class.def_env)
                        .await
                        .map_err(|c| control_to_aserror(c, span))?;
                }
            }
            val = self.coerce_field(&fs.ty, val, &class.def_env, strict, &field_path, span).await?;
            if !check_type(&val, &fs.ty) {
                return Err(AsError::at(
                    format!("type contract violated at {}: expected {}, got {}", field_path, fs.ty, type_name(&val)),
                    span,
                ));
            }
            inst_fields.insert(fname.clone(), val);
        }

        if strict {
            for k in map.borrow().keys() {
                if !schema.contains_key(k) {
                    return Err(AsError::at(
                        format!("unexpected key '{}' for {} (strict)", k, display_path(path, &class.name)),
                        span,
                    ));
                }
            }
        }

        Ok(Value::Instance(std::rc::Rc::new(std::cell::RefCell::new(crate::value::Instance {
            class: class.clone(),
            fields: inst_fields,
        }))))
    }

    /// Recursively coerce a raw value to match a declared field type: a raw
    /// Object whose field type is a class becomes that class's validated
    /// instance; arrays/maps of a class recurse element/value-wise; Optional
    /// passes non-nil through to the inner type. Everything else is unchanged.
    #[async_recursion::async_recursion(?Send)]
    async fn coerce_field(
        &self,
        ty: &crate::ast::Type,
        val: Value,
        env: &Environment,
        strict: bool,
        path: &str,
        span: Span,
    ) -> Result<Value, AsError> {
        use crate::ast::Type;
        match ty {
            Type::Optional(inner) => {
                if val == Value::Nil {
                    Ok(Value::Nil)
                } else {
                    self.coerce_field(inner, val, env, strict, path, span).await
                }
            }
            Type::Named(name) => match (&val, env.get(name)) {
                (Value::Object(_), Some(Value::Class(c))) => {
                    self.validate_into(&c, &val, strict, path, span).await
                }
                _ => Ok(val),
            },
            Type::Array(elem) => match &val {
                Value::Array(a) => {
                    let items: Vec<Value> = a.borrow().clone();
                    let mut out = Vec::with_capacity(items.len());
                    for (i, it) in items.into_iter().enumerate() {
                        let p = format!("{}[{}]", path, i);
                        out.push(self.coerce_field(elem, it, env, strict, &p, span).await?);
                    }
                    Ok(Value::Array(std::rc::Rc::new(std::cell::RefCell::new(out))))
                }
                _ => Ok(val),
            },
            Type::Map(_, vty) => match &val {
                Value::Map(m) => {
                    let entries: Vec<(crate::value::MapKey, Value)> =
                        m.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    let out = std::rc::Rc::new(std::cell::RefCell::new(indexmap::IndexMap::new()));
                    for (k, v) in entries {
                        let p = format!("{}[{}]", path, k.to_value());
                        let cv = self.coerce_field(vty, v, env, strict, &p, span).await?;
                        out.borrow_mut().insert(k, cv);
                    }
                    Ok(Value::Map(out))
                }
                _ => Ok(val),
            },
            _ => Ok(val),
        }
    }
```

Add the two small free helpers near `check_type`:

```rust
fn display_path(path: &str, class_name: &str) -> String {
    if path.is_empty() {
        format!("{}.from", class_name)
    } else {
        path.to_string()
    }
}

fn control_to_aserror(c: Control, span: Span) -> AsError {
    match c {
        Control::Panic(e) => e,
        Control::Propagate(_) => AsError::at("unexpected ? propagation in a field default", span),
    }
}
```

(Confirm the exact `Map` value type: the extract shows `Value::Map(m)` with `m.borrow().iter()` yielding `(MapKey, Value)` and `MapKey::to_value()`. If the concrete container/clone differs, adjust to match `src/value.rs`. `MapKey` and `IndexMap` are already imported in interp.rs.)

- [ ] **Step 2: Build.**

Run: `cargo build`
Expected: compiles (methods unused so far → may warn; the next task wires them up, so the warning is temporary — do not add `#[allow]`).

- [ ] **Step 3: Commit.**

```bash
git add src/interp.rs
git commit -m "feat(interp): validate_into + coerce_field (recursive shape validation core)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 4.2: `Value::ClassMethod` + `.from` dispatch

**Files:**
- Modify: `src/value.rs` (add `Value::ClassMethod` variant)
- Modify: `src/interp.rs` (`type_name`; `read_member` Class arm; `call_value` arm)
- Test: `src/interp.rs` (inline)

- [ ] **Step 1: Write failing tests.**

```rust
    #[tokio::test]
    async fn from_builds_validated_instance() {
        let src = "class U { id: number\n name: string }\n\
                   let o = { id: 1, name: \"Ada\" }\n\
                   let u = U.from(o)\nprint(u.id)\nprint(u.name)";
        let out = run_to_output(src).await;
        assert!(out.contains("1") && out.contains("Ada"), "got: {out}");
    }

    #[tokio::test]
    async fn from_rejects_wrong_type_with_field_path() {
        let src = "class U { id: number\n name: string }\n\
                   let r = recover(() => U.from({ id: \"x\", name: \"Ada\" }))\nprint(r[1].message)";
        let out = run_to_output(src).await;
        assert!(out.contains("u.id") && out.contains("type contract violated"), "got: {out}");
    }

    #[tokio::test]
    async fn from_optional_and_default() {
        let src = "class U { id: number\n nick: string?\n role: string = \"guest\" }\n\
                   let u = U.from({ id: 2 })\nprint(u.nick == nil)\nprint(u.role)";
        let out = run_to_output(src).await;
        assert!(out.contains("true") && out.contains("guest"), "got: {out}");
    }
```

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test from_builds_validated_instance`
Expected: FAIL — `U.from` reads as `nil` (Class member access unsupported) so the call fails.

- [ ] **Step 3: Add the Value variant.** In `src/value.rs`, add to the `Value` enum (mirroring `GeneratorMethod`):

```rust
    /// A class associated function bound to its class, e.g. `User.from`.
    ClassMethod(Rc<Class>, &'static str),
```

- [ ] **Step 4: Build to find the required arms.**

Run: `cargo build`
Expected: non-exhaustive `match` errors point at every place `Value` is matched exhaustively (e.g. `type_name`, any `PartialEq`/`Display`/`Clone` not derived, `call_value`). For EACH error, add a `ClassMethod` arm mirroring how `GeneratorMethod` is handled at that site:
- `type_name` (src/interp.rs:1805-1829): `Value::ClassMethod(..) => "function",`
- Any `Value` `Display`/`to_string`: render like other callables (e.g. `Value::ClassMethod(c, m) => write!(f, "<class method {}.{}>", c.name, m)` — match the style used for `GeneratorMethod`).
- If `Value` has a manual `PartialEq` (it does — classes compare by identity), add `(ClassMethod(a, an), ClassMethod(b, bn)) => Rc::ptr_eq(a, b) && an == bn` (mirror the `GeneratorMethod` identity arm).

- [ ] **Step 5: Read-member returns the ClassMethod.** In `read_member` (src/interp.rs:1072-1157), add an arm for `Value::Class` before the final `_`:

```rust
        Value::Class(c) => match name {
            "from" => Ok(Value::ClassMethod(c.clone(), "from")),
            other => Err(AsError::at(
                format!("class {} has no static member '{}'", c.name, other),
                span,
            )),
        },
```

- [ ] **Step 6: Dispatch the call.** In `call_value` (the match containing `Value::Builtin(name) => self.call_builtin(...)` at src/interp.rs:1163), add an arm:

```rust
            Value::ClassMethod(c, "from") => {
                let obj = args.first().cloned().unwrap_or(Value::Nil);
                let strict = matches!(args.get(1), Some(Value::Bool(true)));
                self.validate_into(&c, &obj, strict, "", span)
                    .await
                    .map_err(Control::from)
            }
            Value::ClassMethod(c, other) => Err(AsError::at(
                format!("class {} has no static member '{}'", c.name, other),
                span,
            )
            .into()),
```

(Confirm `AsError: Into<Control>` / `Control::from(AsError)` exists — the extract shows `.into()` used throughout and `AsError` converts into `Control::Panic`. Use `.map_err(Control::Panic)` if `Control::from` is not defined.)

- [ ] **Step 7: Run.**

Run: `cargo test from_builds_validated_instance ; cargo test from_rejects_wrong_type_with_field_path ; cargo test from_optional_and_default`
Expected: PASS

- [ ] **Step 8: Commit.**

```bash
git add src/value.rs src/interp.rs
git commit -m "feat(interp): Value::ClassMethod — ClassName.from(obj, strict) dispatch

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 4.3: `strict` + recursion tests + runnable example

**Files:**
- Test: `src/interp.rs` (inline)
- Create: `examples/shape_validation.as`

- [ ] **Step 1: Write recursion + strict tests.**

```rust
    #[tokio::test]
    async fn from_recurses_into_nested_class() {
        let src = "class Addr { zip: number }\nclass U { id: number\n addr: Addr }\n\
                   let u = U.from({ id: 1, addr: { zip: 90210 } })\nprint(u.addr.zip)";
        let out = run_to_output(src).await;
        assert!(out.contains("90210"), "got: {out}");
    }

    #[tokio::test]
    async fn from_nested_path_in_error() {
        let src = "class Addr { zip: number }\nclass U { id: number\n addr: Addr }\n\
                   let r = recover(() => U.from({ id: 1, addr: { zip: \"x\" } }))\nprint(r[1].message)";
        let out = run_to_output(src).await;
        assert!(out.contains("u.addr.zip"), "got: {out}");
    }

    #[tokio::test]
    async fn from_recurses_into_array_of_class() {
        let src = "class Tag { v: number }\nclass U { tags: array<Tag> }\n\
                   let u = U.from({ tags: [{ v: 1 }, { v: 2 }] })\nprint(u.tags[1].v)";
        let out = run_to_output(src).await;
        assert!(out.contains("2"), "got: {out}");
    }

    #[tokio::test]
    async fn from_strict_rejects_extra_keys() {
        let src = "class U { id: number }\n\
                   let r = recover(() => U.from({ id: 1, extra: true }, true))\nprint(r[1].message)";
        let out = run_to_output(src).await;
        assert!(out.contains("unexpected key 'extra'"), "got: {out}");
        // Lenient (default) ignores extras:
        let src2 = "class U { id: number }\nlet u = U.from({ id: 1, extra: true })\nprint(u.id)";
        let out2 = run_to_output(src2).await;
        assert!(out2.contains("1"), "got: {out2}");
    }
```

- [ ] **Step 2: Run.**

Run: `cargo test from_recurses ; cargo test from_nested_path_in_error ; cargo test from_strict_rejects_extra_keys`
Expected: PASS (these exercise the Task 4.1 recursion; if `map<K,Class>` recursion needs its own test, add one mirroring the array test using `map<string, Tag>`).

- [ ] **Step 3: Create the example.** `examples/shape_validation.as`:

```javascript
// ClassName.from(obj): validate a raw object into a checked instance,
// recursing into nested class fields, with optional/defaulted fields,
// and recoverable failures.
class Address {
  street: string
  zip: number
}

class User {
  id: number
  name: string
  nickname: string?
  role: string = "guest"
  address: Address
}

let good = { id: 1, name: "Ada", address: { street: "1 Lovelace Way", zip: 90210 } }
let u = User.from(good)
assert(u.id == 1, "id")
assert(u.role == "guest", "role default")
assert(u.nickname == nil, "nickname optional")
assert(u.address.zip == 90210, "nested validated")

// A shape mismatch is a recoverable panic carrying a field path.
let r = recover(() => User.from({ id: 1, name: "Bug", address: { street: "x", zip: "nope" } }))
assert(r[1] != nil, "bad zip rejected")
assert(r[1].message != nil, "error has a message")

print("shape_validation ok")
```

- [ ] **Step 4: Run the example.**

Run: `cargo build --release && target/release/ascript run examples/shape_validation.as`
Expected output: `shape_validation ok`

- [ ] **Step 5: Conformance** (the example is new `.as`):

Run: `cargo test --test treesitter_conformance && cargo test --test frontend_conformance`
Expected: PASS

- [ ] **Step 6: Commit.**

```bash
git add src/interp.rs examples/shape_validation.as
git commit -m "test(interp): .from recursion + strict; runnable shape_validation example

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 4.4: Phase 4 Review

- [ ] **Step 1: Full suite + clippy** (four commands). Expected: pass (no leftover unused-method warnings).
- [ ] **Step 2: Independent review.** Reviewer gets spec §4 and the Phase 4 diff. Must: run the four commands; confirm `.from` builds a real `Instance` (passes a later `: ClassName` contract), recurses into nested class / `array<Class>` / `map<K,Class>`, applies defaults and accepts optional/absent, reports a field path on mismatch, `strict=true` rejects extras while default ignores them, and the whole thing composes with `recover`; confirm `.from` does NOT run `init` (e.g. an `init` with a side effect like a field set is bypassed); confirm `examples/shape_validation.as` runs. Probe: `.from` on a non-object; a missing required field; inheritance (subclass `.from` validates inherited fields). Fix findings.

---

## Phase 5 — Typed parse: `resp.json(Class)` and `json.parse(text, Class)`

Thin decoders over `validate_into`, fusing parse failure and shape mismatch into one Tier-1 `[val, err]` pair. The class rides in as an ordinary value argument — no generics.

### Task 5.1: `json.parse(text, Class)`

**Files:**
- Modify: `src/interp.rs` (the `call_stdlib` intercept at src/interp.rs:1704-1705)
- Test: `src/interp.rs` (inline)

- [ ] **Step 1: Write failing tests.**

```rust
    #[tokio::test]
    async fn json_parse_with_class_validates() {
        let src = "import * as json from \"std/json\"\n\
                   class U { id: number\n name: string }\n\
                   let [u, err] = json.parse(\"{\\\"id\\\":1,\\\"name\\\":\\\"Ada\\\"}\", U)\n\
                   print(err == nil)\nprint(u.id)\nprint(u.name)";
        let out = run_to_output(src).await;
        assert!(out.contains("true") && out.contains("Ada"), "got: {out}");
    }

    #[tokio::test]
    async fn json_parse_with_class_fuses_errors() {
        // shape mismatch comes back as a Tier-1 err, not a panic
        let src = "import * as json from \"std/json\"\n\
                   class U { id: number }\n\
                   let [u, err] = json.parse(\"{\\\"id\\\":\\\"x\\\"}\", U)\n\
                   print(u == nil)\nprint(err != nil)";
        let out = run_to_output(src).await;
        assert!(out.contains("true"), "got: {out}");
        // bad JSON also comes back as err (parse channel)
        let src2 = "import * as json from \"std/json\"\nclass U { id: number }\n\
                    let [u, err] = json.parse(\"{not json\", U)\nprint(err != nil)";
        let out2 = run_to_output(src2).await;
        assert!(out2.contains("true"), "got: {out2}");
    }
```

(Confirm how `std/json` is imported/called in existing tests — match that exact import syntax. If qualified calls use `json.parse` after `import * as json`, the dispatch still routes through `call_stdlib("json","parse",...)`.)

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test json_parse_with_class_validates`
Expected: FAIL — the 2nd arg is currently ignored; `u` is a raw object, `u.id` works but `err == nil` may pass while the validation/`Instance` behavior is absent; the fuses_errors test fails (no validation).

- [ ] **Step 3: Implement the intercept.** In `call_stdlib` (the `_` arm of `call_builtin`, src/interp.rs:1704-1705), special-case `json.parse` with a class 2nd arg BEFORE delegating. Replace:

```rust
                if let Some((module, func)) = other.split_once('.') {
                    self.call_stdlib(module, func, args, span).await
```

with a version that intercepts (locate the body of `call_stdlib`; add at its top, before it routes to `crate::stdlib::call`):

```rust
    async fn call_stdlib(&self, module: &str, func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        // Typed parse: json.parse(text, Class) — parse, then validate, fusing
        // a parse failure and a shape mismatch into one Tier-1 [val, err] pair.
        if module == "json" && func == "parse" {
            if let Some(Value::Class(c)) = args.get(1) {
                let parsed = crate::stdlib::call(module, func, &args[..1], span)?; // [val, err]
                if let Value::Array(a) = &parsed {
                    let (val, err) = {
                        let b = a.borrow();
                        (b[0].clone(), b[1].clone())
                    };
                    if err != Value::Nil {
                        return Ok(parsed); // parse error stays in the err channel
                    }
                    return match self.validate_into(c, &val, false, "", span).await {
                        Ok(inst) => Ok(make_pair(inst, Value::Nil)),
                        Err(e) => Ok(make_pair(Value::Nil, make_error(Value::Str(e.message.into())))),
                    };
                }
            }
        }
        crate::stdlib::call(module, func, args, span)
    }
```

(If `call_stdlib` does not already exist as a separate method — the extract shows `self.call_stdlib(module, func, args, span)` being CALLED, so it does — add the intercept at its top. Keep the existing delegation as the final line.)

- [ ] **Step 4: Run.**

Run: `cargo test json_parse_with_class`
Expected: PASS (both)

- [ ] **Step 5: Commit.**

```bash
git add src/interp.rs
git commit -m "feat(json): json.parse(text, Class) — typed parse, fused error channel

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 5.2: `resp.json(Class)`

**Files:**
- Modify: `src/stdlib/net_http.rs:1300-1351` (`call_http_response_method`)
- Test: `tests/cli.rs` or a `net`-gated inline test (see note)

- [ ] **Step 1: Use the class arg.** In `call_http_response_method` (src/stdlib/net_http.rs:1300-1351), rename `_args` to `args` in the signature, and in the `"json"` branch, validate against a class when one is passed. Replace the `"json"` arm:

```rust
                    "json" => match resp.bytes().await {
                        Ok(b) => match serde_json::from_slice::<serde_json::Value>(&b) {
                            Ok(jv) => {
                                let val = crate::stdlib::json::to_ascript(&jv);
                                if let Some(Value::Class(c)) = args.first() {
                                    match self.validate_into(c, &val, false, "", span).await {
                                        Ok(inst) => Ok(make_pair(inst, Value::Nil)),
                                        Err(e) => Ok(err_pair(e.message)),
                                    }
                                } else {
                                    Ok(make_pair(val, Value::Nil))
                                }
                            }
                            Err(e) => Ok(err_pair(format!("response.json failed: {}", e))),
                        },
                        Err(e) => Ok(err_pair(format!("response.json failed: {}", e))),
                    },
```

(Confirm `err_pair`'s signature — the extract shows `err_pair(format!(...))` taking a `String`/`impl Into`. If `validate_into` is not visible from this module, it is `pub(crate)` on `Interp`; mark it `pub(crate)` in Task 4.1 if needed. `self` here is `&Interp` — `call_http_response_method` is an `Interp` method, so `validate_into` is reachable.)

- [ ] **Step 2: Make `validate_into` reachable.** If Step 1 fails to compile because `validate_into` is private, change its declaration (Task 4.1) to `pub(crate) async fn validate_into`. Re-run `cargo build`.

- [ ] **Step 3: Test.** Add a test exercising `resp.json(Class)` against the in-repo test HTTP server used by `examples/advanced/http_*` (see `tests/cli.rs` / the advanced examples for how a local server is spun up). If a unit-level HTTP fixture is impractical, instead add an `examples/advanced/typed_http.as` that starts the bundled server, fetches a JSON route, and `User.from`-validates via `await resp.json(User)`, then assert the example runs (Step 4). At minimum, add an inline `#[cfg(feature = "net")]` test that constructs a response value if the test harness supports it; otherwise rely on the example run.

- [ ] **Step 4: Build + targeted run.**

Run: `cargo build --release`
Expected: compiles. If an example was added, run it and confirm expected output.

- [ ] **Step 5: Commit.**

```bash
git add src/stdlib/net_http.rs
git commit -m "feat(http): resp.json(Class) — typed parse, fused error channel

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 5.3: Phase 5 Review

- [ ] **Step 1: Full suite + clippy** (four commands; note `json.parse(text, Class)` must also work under `--no-default-features`, since `data`/json is a feature — confirm the intercept compiles in core-only if json is gated, or guard it with the same `#[cfg]`). Expected: pass.
- [ ] **Step 2: Independent review.** Reviewer gets spec §4.5 and the Phase 5 diff. Must: run the four commands; confirm `json.parse(text)` and `resp.json()` with NO class behave exactly as before (raw value); confirm `json.parse(text, Class)` returns `[instance, nil]` on a valid payload and `[nil, err]` on either a parse failure or a shape mismatch (one fused channel); confirm `await resp.json(User)?` unwraps to an instance; confirm `(await json.parse(...))` form is unaffected. Probe feature-gating (core-only build). Fix findings.

---

## Phase 6 — Documentation, headline examples, final verification

All prose docs, the renderer audit, README/CLAUDE.md, and a final whole-suite + clippy gate. (Auto-`init` is intentionally out of scope and not mentioned.)

### Task 6.1: Language guide — types, classes, errors pages

**Files:**
- Modify: `docs/content/language/*` (the types/contracts page, the classes page, the error-tier page — locate exact filenames by listing the dir)

- [ ] **Step 1: List the content files.**

Run: `ls docs/content/language/`
Expected: a set of `.md` pages; identify the types/contracts, classes, and errors pages.

- [ ] **Step 2: Edit the types/contracts page.** Add a section documenting the nullable suffix `T?` (≡ `T | nil`), valid in every type position (`let`/`const`/param/return/field), with examples `let port: number? = nil` and `fn lookup(k: string): User?`.

- [ ] **Step 3: Edit the classes page.** Document typed fields: required (`id: number`), optional (`nickname: string?` and the alias `nickname?: string`, noting both lower to the same thing and the formatter normalizes to `name: T?`), and defaults (`role: string = "guest"`). Document `ClassName.from(obj, strict = false)`: validates a raw object into a checked instance, recurses into nested class / `array<Class>` / `map<K,Class>` fields, applies defaults, panics (recoverable) on mismatch with a field path; `strict` rejects unknown keys. Note `.from` does not run `init`.

- [ ] **Step 4: Edit the error-tier page.** Add postfix `!` next to `?` and `recover`: `expr!` force-unwraps a `[value, err]` pair, panicking (carrying the original message) on error — the dual of `?`. Note the precedence rule: `?`/`!` bind looser than `await`, so `await resp.json()!` and `await resp.json(User)?` need no parens.

- [ ] **Step 5: Commit.**

```bash
git add docs/content/language/
git commit -m "docs: language guide — T?, typed fields/.from, force-unwrap !

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 6.2: Stdlib reference — net + data pages

**Files:**
- Modify: `docs/content/stdlib/net.md` (resp.json(Class))
- Modify: `docs/content/stdlib/data.md` (json.parse(text, Class))

- [ ] **Step 1: Edit `net.md`.** In the HTTP response section, document the optional class argument: `resp.json(Class)` decodes the body and validates it against `Class`, returning `[instance, err]` where `err` carries either a parse failure or a shape mismatch. Show `let user = await resp.json(User)?`.

- [ ] **Step 2: Edit `data.md`.** In the json section, document `json.parse(text, Class)` analogously, with a fused-error example.

- [ ] **Step 3: Commit.**

```bash
git add docs/content/stdlib/net.md docs/content/stdlib/data.md
git commit -m "docs(stdlib): document resp.json(Class) and json.parse(text, Class)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 6.3: Renderer audit + README + CLAUDE.md

**Files:**
- Audit only: `docs/assets/app.js`
- Modify: `README.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Audit the docs renderer.** Confirm `docs/assets/app.js`'s `highlightAScript()` (around lines 166-251) needs NO change: its operator regex already includes `!` and `?`, and field declarations / `T?` use existing tokens. Verify by serving the docs and viewing a page that contains the new syntax:

Run:
```bash
cd docs && python3 -m http.server >/dev/null 2>&1 & echo "serving"; sleep 1
```
Then open `http://localhost:8000/reader.html`, view a page with a class-field / `!` code block, and confirm highlighting is sane. Stop the server afterward (`kill %1`). No code edit expected; if highlighting is broken, fix the regex/keyword lists in `app.js`.

- [ ] **Step 2: Update README.** Add the new capabilities to the feature/stdlib summary: nullable `T?`, typed class fields + `.from` validation, typed parse, force-unwrap `!`.

- [ ] **Step 3: Update CLAUDE.md.** Record: the new `ExprKind::Unwrap` (needs arms in interp eval, `fmt.rs`, `ast.rs` Display); `Type::Optional`; the `?`/`!`-looser-than-`await` precedence rule (parser `unwrap_tier`, formatter inner-prec); the `class_body` grammar now allows field declarations; `Value::ClassMethod` for `ClassName.from`.

- [ ] **Step 4: Commit.**

```bash
git add README.md CLAUDE.md
git commit -m "docs: README + CLAUDE.md — shape validation, force-unwrap, T?

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 6.4: Headline end-to-end example (HTTP + typed parse)

**Files:**
- Create: `examples/advanced/typed_json_api.as`

- [ ] **Step 1: Write the example.** Mirror the structure of an existing `examples/advanced/http_*.as` (which spins up the bundled server with `serve({maxRequests:N})` and fetches from it). The example must: define `User`/`Address` classes with typed/optional/default/nested fields; fetch a JSON route; validate with `let user = await resp.json(User)?` inside a `Result`-returning function; print the result; and demonstrate a recovery path on a malformed payload. Keep it fully error-handled (advanced examples are production-shaped).

- [ ] **Step 2: Run it.**

Run: `cargo build --release && target/release/ascript run examples/advanced/typed_json_api.as`
Expected: runs to completion, printing the validated user and the recovered error (define the exact expected lines in the example's own asserts/prints).

- [ ] **Step 3: Conformance.**

Run: `cargo test --test treesitter_conformance && cargo test --test frontend_conformance`
Expected: PASS (the example must parse in both parsers).

- [ ] **Step 4: Commit.**

```bash
git add examples/advanced/typed_json_api.as
git commit -m "docs(examples): typed_json_api.as — HTTP + resp.json(User) validation

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task 6.5: Final verification + holistic review

- [ ] **Step 1: Format all new examples and confirm idempotence.**

Run:
```bash
for f in examples/optional_types.as examples/force_unwrap.as examples/typed_fields.as examples/shape_validation.as examples/advanced/typed_json_api.as; do target/release/ascript fmt "$f"; done
git diff --stat
```
Expected: either no diff, or only canonical normalization (e.g. `name?:` → `name: T?`); re-run each example after formatting to confirm it still runs and conformance still passes.

- [ ] **Step 2: Whole suite + clippy, both configs.**

Run:
```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
```
Expected: all pass; clippy clean in both configs.

- [ ] **Step 3: Spec coverage sweep.** Re-read the spec (`docs/superpowers/specs/2026-05-31-class-shape-validation-and-unwrap-design.md`) §§3–8 and confirm each requirement maps to a completed task: `T?` (Phase 1), `!` + precedence (Phase 2), typed fields + both spellings + defaults + assignment checks + LSP symbols + formatter (Phase 3), `.from` + recursion + strict + non-panicking core (Phase 4), typed parse both decoders (Phase 5), all docs + renderer audit + examples (Phase 6). List any gap and add a task for it.

- [ ] **Step 4: Holistic independent review.** Dispatch a fresh reviewer over the FULL diff (`git diff main...design/class-shape-validation`) with the spec. The reviewer runs all four gate commands and every new example, exercises the headline one-liner (`recover(() => User.from(await resp.json()!))?` and `await resp.json(User)?`), and checks cross-cutting concerns: no `RefCell` borrow held across an `.await` (clippy enforces, but eyeball `validate_into`/`coerce_field`/`construct`), the `?`/`!` precedence change didn't regress any existing parse, and the formatter is idempotent on the whole `examples/` tree. Fix findings, then the branch is ready for the CLAUDE.md merge workflow (`merge --no-ff`).
