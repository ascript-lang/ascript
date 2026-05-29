# AScript Milestone 4 — Data Structures Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Add compound data to AScript: array literals `[…]`, object literals `{…}`, member access `.`, indexing `[]`, optional chaining `?.`, l-value assignment to members/indices, `for (x of iterable)` iteration, and template strings. After this milestone you can build and traverse real data structures.

**Architecture:** Builds on M3. Adds two heap `Value` kinds (`Array` = `Rc<RefCell<Vec<Value>>>`, `Object` = `Rc<RefCell<IndexMap<String, Value>>>` for insertion order). Member/index access plug into the parser's `postfix()` loop. Assignment generalizes from a name to an l-value expression (`target: Box<Expr>`). Cycle-safe `Display` renders nested structures.

**Tech Stack:** Rust 2021, tokio (current_thread), async-recursion, **+ `indexmap`** (insertion-ordered objects). No other new crates.

**Starting state (end of M3, on `main`):** Working interpreter with the full language core through functions/closures/arrows. `Value` = `Nil|Bool|Number|Str|Builtin|Function`, manual `PartialEq`/`Debug`/`Display`. `ExprKind::Assign { name: String, value }`. `postfix()` loops only on `Tok::LParen` (calls). Lexer: lone `.` and `?` and `[`/`]` currently ERROR. 62 lib + 4 integration tests.

**Conventions:** spans are char offsets; single-threaded `Rc`/`RefCell`; optional `;`; `eval_expr`/`exec`/`exec_stmt`/`call_function` are `#[async_recursion(?Send)]`.

## Spec semantics decided for this milestone

- **Equality (spec §4):** arrays/objects compare by **identity** (`Rc::ptr_eq`); scalars stay structural. `[1] == [1]` is `false`.
- **Truthiness:** arrays/objects are truthy (only `nil`/`false` are falsy).
- **Indexing:** `arr[i]` requires `i` to be a non-negative integer `< len`; out of range → a runtime error ("index out of bounds"). (Spec §6 classifies this as a *panic*; the panic/Result tiers land in M5, which will reclassify — for now it aborts via the normal error path, which is behaviorally equivalent: the program stops with a diagnostic.)
- **Object member read:** a missing key reads as `nil` (Lua-like). Reading a member of `nil` errors ("cannot read property 'x' of nil") unless via `?.` (which yields `nil`). Reading a member of a non-object/non-nil errors.
- **`for (x of …)`** iterates: arrays (elements) and strings (characters as single-char strings). Objects are not directly iterable (use stdlib helpers in M8).
- **Display:** `[1, 2, 3]` and `{key: value}`; nested strings are quoted (`[1, "two"]`, `{a: "x"}`); top-level `print("x")` stays raw. Cycle-safe (`[...]`/`{...}` when a structure contains itself).
- **Object literal vs block:** in statement position `{` is a block; in expression position `{` is an object literal. A bare object-literal statement must be parenthesized: `({a: 1})`.

## Scope & Justified Deferrals

| Deferred | Why | Milestone |
|---|---|---|
| `Map` value kind + `std/map` | No map literal syntax in the spec; maps are constructed by the stdlib, which doesn't exist yet | **M8 — Core collections** |
| `?` Result operator, panic/Result tiers, `recover` | Error-model design | **M5** |
| Array/object methods (`push`, `keys`, `map`, …) | Provided by `std/array`/`std/object` | **M8** |
| Type annotations (`array<T>`, `object`) | Gradual-contract layer | **M6** |

---

## Task 1: Generalize assignment to an l-value expression

Refactor `ExprKind::Assign { name: String, value }` into `ExprKind::Assign { target: Box<Expr>, value }`. Behavior is identical for identifier targets; this prepares for `arr[i] = v` and `obj.k = v`. Do this FIRST.

**Files:** `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/ast.rs`** — change the `Assign` variant:

```rust
    Assign { target: Box<Expr>, value: Box<Expr> },
```

Update its `Display` arm (render the target expression):

```rust
            ExprKind::Assign { target, value } => write!(f, "(= {} {})", target, value),
```

- [ ] **Step 2: `src/parser.rs`** — `assignment()` keeps the parsed l.h.s. expression as the target instead of extracting a name. Replace the assignment body that builds `Assign`:

```rust
    fn assignment(&mut self) -> Result<Expr, AsError> {
        if let Some(arrow) = self.try_arrow()? {
            return Ok(arrow);
        }

        let target = self.coalesce()?;

        let compound = match self.peek() {
            Tok::Eq => None,
            Tok::PlusEq => Some(BinOp::Add),
            Tok::MinusEq => Some(BinOp::Sub),
            Tok::StarEq => Some(BinOp::Mul),
            Tok::SlashEq => Some(BinOp::Div),
            _ => return Ok(target),
        };
        self.advance(); // assignment operator
        let value = self.assignment()?; // right-associative

        // Only assignable expressions are valid targets. (Index/Member added later
        // in this milestone are also assignable; identifiers always are.)
        if !is_assignable(&target) {
            return Err(AsError::at("invalid assignment target", target.span));
        }

        let span = Span::new(target.span.start, value.span.end);
        let value = match compound {
            None => value,
            Some(op) => {
                // x += e  =>  x = (x + e). Re-uses the target expression as the lhs.
                let lhs = target.clone();
                Self::make_binary(lhs, op, value)
            }
        };

        Ok(Expr {
            kind: ExprKind::Assign { target: Box::new(target), value: Box::new(value) },
            span,
        })
    }
```

Add a free helper near the bottom of `parser.rs` (outside the `impl`):

```rust
/// Whether an expression can be the target of an assignment.
fn is_assignable(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Ident(_))
}
```

(`is_assignable` is extended in later tasks to accept `Index`/`Member`.)

NOTE: `Expr` already derives `Clone` (used by `target.clone()`).

- [ ] **Step 3: `src/interp.rs`** — evaluate assignment by resolving the target as a place. Replace the `ExprKind::Assign` arm:

```rust
            ExprKind::Assign { target, value } => {
                let v = self.eval_expr(value, env).await?;
                self.assign_to(target, v, env).await
            }
```

Add an `assign_to` method (handles identifier targets now; `Index`/`Member` arms are added in later tasks):

```rust
    #[async_recursion(?Send)]
    async fn assign_to(&mut self, target: &Expr, value: Value, env: &Environment) -> Result<Value, AsError> {
        match &target.kind {
            ExprKind::Ident(name) => match env.assign(name, value.clone()) {
                Ok(()) => Ok(value),
                Err(AssignError::Undefined) => Err(AsError::at(
                    format!("cannot assign to undefined variable '{}'", name),
                    target.span,
                )),
                Err(AssignError::Immutable) => Err(AsError::at(
                    format!("cannot assign to immutable binding '{}'", name),
                    target.span,
                )),
            },
            _ => Err(AsError::at("invalid assignment target", target.span)),
        }
    }
```

- [ ] **Step 4: Run** `cargo test` (all 62 lib + 4 integration still pass — assignment behavior is unchanged for identifiers) and `cargo clippy --all-targets` (clean).

- [ ] **Step 5: Commit**

```bash
git add src/ast.rs src/parser.rs src/interp.rs
git commit -m "refactor: generalize assignment target to an l-value expression"
```

---

## Task 2: Arrays — literals, indexing (read + write), display, equality

**Files:** `src/token.rs`, `src/lexer.rs`, `src/value.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add (before `Eof`): `LBracket, RBracket,`.

- [ ] **Step 2: `src/lexer.rs`** — add bracket arms with the other punctuation:

```rust
            '[' => push(&mut tokens, Tok::LBracket, start, &mut i),
            ']' => push(&mut tokens, Tok::RBracket, start, &mut i),
```

- [ ] **Step 3: `src/value.rs`** — add the `Array` value kind. Add imports and the variant, and update `is_truthy` (no change needed), `PartialEq`, `Debug`, `Display`.

Add import: `use std::cell::RefCell;`

Add the variant:

```rust
    Array(Rc<RefCell<Vec<Value>>>),
```

`PartialEq` — identity for arrays:

```rust
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
```

`Debug` — shallow (cycle-safe):

```rust
            Value::Array(a) => write!(f, "Array(len {})", a.borrow().len()),
```

`Display` — switch to a cycle-safe recursive helper. Replace the `Display` impl with a thin wrapper that delegates to an inherent method threading a "seen pointers" set:

```rust
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_display(f, &mut Vec::new())
    }
}

impl Value {
    fn write_display(&self, f: &mut fmt::Formatter<'_>, seen: &mut Vec<usize>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Number(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{}", s),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::Function(func) => match &func.name {
                Some(n) => write!(f, "<function {}>", n),
                None => write!(f, "<function>"),
            },
            Value::Array(a) => {
                let ptr = Rc::as_ptr(a) as usize;
                if seen.contains(&ptr) {
                    return write!(f, "[...]");
                }
                seen.push(ptr);
                write!(f, "[")?;
                for (i, v) in a.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    v.write_element(f, seen)?;
                }
                write!(f, "]")?;
                seen.pop();
                Ok(())
            }
        }
    }

    /// Like `write_display`, but quotes bare strings (used for nested elements
    /// so `[1, "two"]` shows the quotes while top-level `print("x")` stays raw).
    fn write_element(&self, f: &mut fmt::Formatter<'_>, seen: &mut Vec<usize>) -> fmt::Result {
        match self {
            Value::Str(s) => write!(f, "{:?}", s),
            _ => self.write_display(f, seen),
        }
    }
}
```

(Remove the old `Display` match body — it is fully replaced by `write_display`.)

Add tests:

```rust
    #[test]
    fn arrays_compare_by_identity_and_display() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let a = Value::Array(Rc::new(RefCell::new(vec![Value::Number(1.0), Value::Str("two".into())])));
        assert_eq!(a.to_string(), "[1, \"two\"]");
        // identity: a clone of the SAME Rc is equal; a fresh array is not
        assert_eq!(a.clone(), a);
        let b = Value::Array(Rc::new(RefCell::new(vec![Value::Number(1.0)])));
        assert_ne!(a, b);
        assert!(a.is_truthy());
    }
```

- [ ] **Step 4: `src/ast.rs`** — add array literal and index expressions to `ExprKind`:

```rust
    Array(Vec<Expr>),
    Index { object: Box<Expr>, index: Box<Expr> },
```

`Display` arms:

```rust
            ExprKind::Array(items) => {
                write!(f, "[")?;
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", it)?;
                }
                write!(f, "]")
            }
            ExprKind::Index { object, index } => write!(f, "(index {} {})", object, index),
```

- [ ] **Step 5: `src/parser.rs`** — parse array literals in `primary`, indexing in `postfix`, and mark `Index` assignable.

In `primary`, add an arm for `[`:

```rust
            Tok::LBracket => {
                let mut items = Vec::new();
                if *self.peek() != Tok::RBracket {
                    loop {
                        items.push(self.expr()?);
                        if *self.peek() == Tok::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.eat(&Tok::RBracket)?;
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr { kind: ExprKind::Array(items), span });
            }
```

(Place this arm inside the `match self.advance()` in `primary` — note `tok_span` is captured before `advance()` as it already is for other arms; the array arm consumes the `[` via `advance()` and then parses items.)

In `postfix`, add an indexing suffix alongside the call suffix. Change the `while` loop to a `match`-based loop:

```rust
    fn postfix(&mut self) -> Result<Expr, AsError> {
        let mut expr = self.primary()?;
        loop {
            match self.peek() {
                Tok::LParen => {
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
                    expr = Expr { kind: ExprKind::Call { callee: Box::new(expr), args }, span };
                }
                Tok::LBracket => {
                    self.advance();
                    let index = self.expr()?;
                    self.eat(&Tok::RBracket)?;
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr {
                        kind: ExprKind::Index { object: Box::new(expr), index: Box::new(index) },
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }
```

Extend `is_assignable`:

```rust
fn is_assignable(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Ident(_) | ExprKind::Index { .. })
}
```

- [ ] **Step 6: `src/interp.rs`** — evaluate array literals, index reads, and index writes.

Add a `use std::cell::RefCell;` and `use std::rc::Rc;` if not present.

`eval_expr` arms:

```rust
            ExprKind::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval_expr(item, env).await?);
                }
                Ok(Value::Array(Rc::new(RefCell::new(values))))
            }
            ExprKind::Index { object, index } => {
                let obj = self.eval_expr(object, env).await?;
                let idx = self.eval_expr(index, env).await?;
                match obj {
                    Value::Array(arr) => {
                        let i = array_index(&idx, expr.span)?;
                        let arr = arr.borrow();
                        arr.get(i)
                            .cloned()
                            .ok_or_else(|| AsError::at(format!("index {} out of bounds (len {})", i, arr.len()), expr.span))
                    }
                    _ => Err(AsError::at("cannot index a non-array value", object.span)),
                }
            }
```

Add the `Index` arm to `assign_to`:

```rust
            ExprKind::Index { object, index } => {
                let obj = self.eval_expr(object, env).await?;
                let idx = self.eval_expr(index, env).await?;
                match obj {
                    Value::Array(arr) => {
                        let i = array_index(&idx, target.span)?;
                        let mut arr = arr.borrow_mut();
                        if i >= arr.len() {
                            return Err(AsError::at(
                                format!("index {} out of bounds (len {})", i, arr.len()),
                                target.span,
                            ));
                        }
                        arr[i] = value.clone();
                        Ok(value)
                    }
                    _ => Err(AsError::at("cannot index-assign a non-array value", object.span)),
                }
            }
```

Add a helper (free function in `interp.rs`):

```rust
/// Validate that a value is a usable array index (a non-negative integer).
fn array_index(v: &Value, span: Span) -> Result<usize, AsError> {
    match v {
        Value::Number(n) if n.fract() == 0.0 && *n >= 0.0 => Ok(*n as usize),
        Value::Number(_) => Err(AsError::at("array index must be a non-negative integer", span)),
        _ => Err(AsError::at("array index must be a number", span)),
    }
}
```

Add interpreter tests:

```rust
    #[tokio::test]
    async fn array_literal_and_indexing() {
        let src = "let a = [10, 20, 30]\nprint(a[0])\nprint(a[2])";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "10\n30\n");
    }

    #[tokio::test]
    async fn index_assignment() {
        let src = "let a = [1, 2, 3]\na[1] = 99\nprint(a[1])\nprint(a)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "99\n[1, 99, 3]\n");
    }

    #[tokio::test]
    async fn out_of_bounds_index_errors() {
        let src = "let a = [1]\nprint(a[5])";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = interp.exec(&stmts, &env).await.unwrap_err();
        assert!(err.message.contains("out of bounds"));
    }
```

- [ ] **Step 7: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 8: Commit**

```bash
git add src/token.rs src/lexer.rs src/value.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add arrays with literals, indexing (read and write), and display"
```

---

## Task 3: Objects — literals, member access, computed access (read + write)

**Files:** `Cargo.toml`, `src/token.rs`, `src/lexer.rs`, `src/value.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `Cargo.toml`** — add the dependency: `indexmap = "2"`.

- [ ] **Step 2: `src/token.rs`** — add `Dot,` before `Eof`.

- [ ] **Step 3: `src/lexer.rs`** — change the lone-`.` arm so `.` emits `Dot` (and `..` still emits `DotDot`):

```rust
            '.' => {
                if i + 1 < chars.len() && chars[i + 1] == '.' {
                    tokens.push(Token { tok: Tok::DotDot, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { tok: Tok::Dot, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
```

- [ ] **Step 4: `src/value.rs`** — add the `Object` value kind (insertion-ordered).

Add import: `use indexmap::IndexMap;`

Variant:

```rust
    Object(Rc<RefCell<IndexMap<String, Value>>>),
```

`PartialEq`:

```rust
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
```

`Debug`:

```rust
            Value::Object(o) => write!(f, "Object(len {})", o.borrow().len()),
```

`write_display` — add an `Object` arm (cycle-safe; keys rendered bare, values via `write_element`):

```rust
            Value::Object(o) => {
                let ptr = Rc::as_ptr(o) as usize;
                if seen.contains(&ptr) {
                    return write!(f, "{{...}}");
                }
                seen.push(ptr);
                write!(f, "{{")?;
                for (i, (k, v)) in o.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: ", k)?;
                    v.write_element(f, seen)?;
                }
                write!(f, "}}")?;
                seen.pop();
                Ok(())
            }
```

Add a test:

```rust
    #[test]
    fn objects_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut m = IndexMap::new();
        m.insert("a".to_string(), Value::Number(1.0));
        m.insert("b".to_string(), Value::Str("x".into()));
        let o = Value::Object(Rc::new(RefCell::new(m)));
        assert_eq!(o.to_string(), "{a: 1, b: \"x\"}");
        assert_eq!(o.clone(), o);
        assert!(o.is_truthy());
    }
```

- [ ] **Step 5: `src/ast.rs`** — add object-literal and member expressions:

```rust
    Object(Vec<(String, Expr)>),
    Member { object: Box<Expr>, name: String },
```

`Display` arms:

```rust
            ExprKind::Object(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            }
            ExprKind::Member { object, name } => write!(f, "(. {} {})", object, name),
```

- [ ] **Step 6: `src/parser.rs`** — object literals in `primary`, member access in `postfix`, and mark `Member` assignable.

In `primary`'s `match self.advance()`, add a `{` arm (object literal — note: only reached in EXPRESSION position; statement-position `{` is handled by `statement()` as a block):

```rust
            Tok::LBrace => {
                let mut entries = Vec::new();
                if *self.peek() != Tok::RBrace {
                    loop {
                        let key = match self.advance() {
                            Tok::Ident(name) => name,
                            Tok::Str(s) => s,
                            other => {
                                return Err(AsError::at(
                                    format!("expected object key, found {:?}", other),
                                    self.tokens[self.pos - 1].span,
                                ))
                            }
                        };
                        self.eat(&Tok::Colon)?;
                        let value = self.expr()?;
                        entries.push((key, value));
                        if *self.peek() == Tok::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.eat(&Tok::RBrace)?;
                let span = Span::new(tok_span.start, self.prev_end());
                return Ok(Expr { kind: ExprKind::Object(entries), span });
            }
```

This needs a `Colon` token. Add `Colon,` to `src/token.rs` (before `Eof`) and a `':' => push(&mut tokens, Tok::Colon, start, &mut i),` arm to the lexer (do this as part of this task).

In `postfix`, add a `.`-member suffix to the loop:

```rust
                Tok::Dot => {
                    self.advance();
                    let name = match self.advance() {
                        Tok::Ident(name) => name,
                        other => {
                            return Err(AsError::at(
                                format!("expected a property name after '.', found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    };
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::Member { object: Box::new(expr), name }, span };
                }
```

Extend `is_assignable`:

```rust
fn is_assignable(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Ident(_) | ExprKind::Index { .. } | ExprKind::Member { .. }
    )
}
```

- [ ] **Step 7: `src/interp.rs`** — evaluate object literals, member reads, computed string indexing on objects, and member/computed writes.

`eval_expr` arms:

```rust
            ExprKind::Object(entries) => {
                let mut map = indexmap::IndexMap::with_capacity(entries.len());
                for (k, v) in entries {
                    let value = self.eval_expr(v, env).await?;
                    map.insert(k.clone(), value);
                }
                Ok(Value::Object(std::rc::Rc::new(std::cell::RefCell::new(map))))
            }
            ExprKind::Member { object, name } => {
                let obj = self.eval_expr(object, env).await?;
                self.read_member(&obj, name, object.span)
            }
```

Extend the existing `ExprKind::Index` READ arm so objects support string indexing (`obj["k"]`). Replace its `match obj` with:

```rust
                match obj {
                    Value::Array(arr) => {
                        let i = array_index(&idx, expr.span)?;
                        let arr = arr.borrow();
                        arr.get(i)
                            .cloned()
                            .ok_or_else(|| AsError::at(format!("index {} out of bounds (len {})", i, arr.len()), expr.span))
                    }
                    Value::Object(map) => match idx {
                        Value::Str(key) => Ok(map.borrow().get(key.as_ref()).cloned().unwrap_or(Value::Nil)),
                        _ => Err(AsError::at("object index must be a string", expr.span)),
                    },
                    _ => Err(AsError::at("cannot index this value", object.span)),
                }
```

Add a `read_member` helper:

```rust
    fn read_member(&self, obj: &Value, name: &str, span: Span) -> Result<Value, AsError> {
        match obj {
            Value::Object(map) => Ok(map.borrow().get(name).cloned().unwrap_or(Value::Nil)),
            Value::Nil => Err(AsError::at(format!("cannot read property '{}' of nil", name), span)),
            _ => Err(AsError::at(format!("cannot read property '{}' of this value", name), span)),
        }
    }
```

Extend `assign_to` to handle `Member` and object string-`Index` targets. Add a `Member` arm and extend the `Index` arm:

```rust
            ExprKind::Member { object, name } => {
                let obj = self.eval_expr(object, env).await?;
                match obj {
                    Value::Object(map) => {
                        map.borrow_mut().insert(name.clone(), value.clone());
                        Ok(value)
                    }
                    _ => Err(AsError::at(format!("cannot set property '{}' on this value", name), object.span)),
                }
            }
```

And in `assign_to`'s `Index` arm, add an `Object` case alongside the existing `Array` case:

```rust
                    Value::Object(map) => match idx {
                        Value::Str(key) => {
                            map.borrow_mut().insert(key.to_string(), value.clone());
                            Ok(value)
                        }
                        _ => Err(AsError::at("object index must be a string", target.span)),
                    },
```

Add interpreter tests:

```rust
    #[tokio::test]
    async fn object_literal_member_and_computed_access() {
        let src = "let o = { name: \"Ada\", age: 36 }\nprint(o.name)\nprint(o[\"age\"])\nprint(o.missing)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "Ada\n36\nnil\n");
    }

    #[tokio::test]
    async fn member_and_computed_assignment() {
        let src = "let o = { a: 1 }\no.b = 2\no[\"c\"] = 3\nprint(o.a + o.b + o.c)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "6\n");
    }

    #[tokio::test]
    async fn member_of_nil_errors() {
        let src = "let x = nil\nprint(x.foo)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = interp.exec(&stmts, &env).await.unwrap_err();
        assert!(err.message.contains("cannot read property 'foo' of nil"));
    }
```

Also add a parser test for object literals (in `parser.rs`):

```rust
    #[test]
    fn parses_object_and_member() {
        assert_eq!(sexpr("({a: 1}).a"), "(. {a: 1} a)");
    }
```

- [ ] **Step 8: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock src/token.rs src/lexer.rs src/value.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add objects with literals, member and computed access (read and write)"
```

---

## Task 4: Optional chaining `?.`

`obj?.name` evaluates to `nil` when `obj` is `nil` (short-circuiting), else reads the member.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `QuestionDot,` before `Eof`.

- [ ] **Step 2: `src/lexer.rs`** — extend the `'?'` arm so `?.` → `QuestionDot` (keep `??` → `QuestionQuestion`; lone `?` still errors with the M5 message):

```rust
            '?' => {
                if i + 1 < chars.len() && chars[i + 1] == '?' {
                    tokens.push(Token { tok: Tok::QuestionQuestion, span: Span::new(start, start + 2) });
                    i += 2;
                } else if i + 1 < chars.len() && chars[i + 1] == '.' {
                    tokens.push(Token { tok: Tok::QuestionDot, span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    return Err(AsError::at(
                        "unexpected character '?' (the ? operator arrives in Milestone 5)",
                        Span::new(start, start + 1),
                    ));
                }
            }
```

- [ ] **Step 3: `src/ast.rs`** — add an optional-member expression:

```rust
    OptMember { object: Box<Expr>, name: String },
```

`Display`: `ExprKind::OptMember { object, name } => write!(f, "(?. {} {})", object, name),`

- [ ] **Step 4: `src/parser.rs`** — add a `?.` suffix in `postfix` (mirrors the `.` arm):

```rust
                Tok::QuestionDot => {
                    self.advance();
                    let name = match self.advance() {
                        Tok::Ident(name) => name,
                        other => {
                            return Err(AsError::at(
                                format!("expected a property name after '?.', found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    };
                    let span = Span::new(expr.span.start, self.prev_end());
                    expr = Expr { kind: ExprKind::OptMember { object: Box::new(expr), name }, span };
                }
```

(`OptMember` is NOT assignable — leave `is_assignable` unchanged.)

- [ ] **Step 5: `src/interp.rs`** — add the `OptMember` eval arm (short-circuits on nil):

```rust
            ExprKind::OptMember { object, name } => {
                let obj = self.eval_expr(object, env).await?;
                if obj == Value::Nil {
                    Ok(Value::Nil)
                } else {
                    self.read_member(&obj, name, object.span)
                }
            }
```

Add tests:

```rust
    #[tokio::test]
    async fn optional_chaining_short_circuits_on_nil() {
        let src = "let o = { a: nil }\nprint(o?.a)\nprint(o.a?.deep)\nprint((o.a ?? 42))";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        // o?.a -> nil; o.a is nil so o.a?.deep -> nil; nil ?? 42 -> 42
        assert_eq!(interp.output, "nil\nnil\n42\n");
    }

    #[tokio::test]
    async fn optional_chaining_reads_when_present() {
        let src = "let o = { a: { b: 7 } }\nprint(o?.a?.b)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "7\n");
    }
```

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add optional chaining (?.)"
```

---

## Task 5: `for (x of iterable)` over arrays and strings

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add `Of,` before `Eof`.

- [ ] **Step 2: `src/lexer.rs`** — add `"of" => Tok::Of,` to the keyword match.

- [ ] **Step 3: `src/ast.rs`** — add a for-of statement:

```rust
    ForOf { var: String, iter: Expr, body: Vec<Stmt> },
```

- [ ] **Step 4: `src/parser.rs`** — make `for_stmt` branch on `in` (range) vs `of` (iterable) after reading the loop var. Replace `for_stmt`:

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
        match self.advance() {
            Tok::In => {
                let start = self.expr()?;
                self.eat(&Tok::DotDot)?;
                let end = self.expr()?;
                self.eat(&Tok::RParen)?;
                let body = self.block()?;
                Ok(Stmt::ForRange { var, start, end, body })
            }
            Tok::Of => {
                let iter = self.expr()?;
                self.eat(&Tok::RParen)?;
                let body = self.block()?;
                Ok(Stmt::ForOf { var, iter, body })
            }
            other => Err(AsError::at(
                format!("expected 'in' or 'of' in for-loop, found {:?}", other),
                self.tokens[self.pos - 1].span,
            )),
        }
    }
```

- [ ] **Step 5: `src/interp.rs`** — add the `ForOf` arm to `exec_stmt`. It iterates arrays (elements) and strings (characters); each iteration binds `var` in a fresh child scope and respects `Flow`:

```rust
            Stmt::ForOf { var, iter, body } => {
                let iterable = self.eval_expr(iter, env).await?;
                let items: Vec<Value> = match iterable {
                    Value::Array(arr) => arr.borrow().clone(),
                    Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string().into())).collect(),
                    other => {
                        return Err(AsError::at(
                            format!("value of type {} is not iterable", type_name(&other)),
                            iter.span,
                        ))
                    }
                };
                for item in items {
                    let child = env.child();
                    child.define(var, item, false).map_err(AsError::new)?;
                    match self.exec(body, &child).await? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Continue | Flow::Normal => {}
                    }
                }
                Ok(Flow::Normal)
            }
```

Add a small `type_name` helper (free fn in `interp.rs`) used by the error:

```rust
/// Human-readable type name for diagnostics.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Nil => "nil",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::Str(_) => "string",
        Value::Builtin(_) | Value::Function(_) => "function",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
```

NOTE: snapshotting the array into a `Vec` before iterating (`arr.borrow().clone()`) avoids holding a `RefCell` borrow across the `await` of the body (which could mutate the array). This is the correct, panic-safe choice.

Add tests:

```rust
    #[tokio::test]
    async fn for_of_iterates_array() {
        let src = "let total = 0\nfor (x of [10, 20, 30]) { total += x }\nprint(total)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "60\n");
    }

    #[tokio::test]
    async fn for_of_iterates_string_chars() {
        let src = "let out = \"\"\nfor (c of \"abc\") { out = out + c + \".\" }\nprint(out)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "a.b.c.\n");
    }

    #[tokio::test]
    async fn for_of_non_iterable_errors() {
        let src = "for (x of 42) { print(x) }";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        let err = interp.exec(&stmts, &env).await.unwrap_err();
        assert!(err.message.contains("not iterable"));
    }
```

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add for-of iteration over arrays and strings"
```

---

## Task 6: Template strings

Backtick strings with `${expr}` interpolation: `` `hi ${name}, you are ${age}` ``. Lexed as a sequence of literal chunks and embedded expressions; the parser assembles a string-concatenation expression.

**Files:** `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/token.rs`** — add tokens for template pieces:

```rust
    TemplateStr(String),  // a complete template with no interpolation: `...`
    TemplateStart(String), // `...${   — text before the first interpolation
    TemplateMiddle(String),// }...${    — text between interpolations
    TemplateEnd(String),   // }...`     — text after the last interpolation
```

- [ ] **Step 2: `src/lexer.rs`** — lex backtick templates. Add a `` '`' `` arm. The lexer scans the backtick string, emitting `TemplateStr` if there is no `${`, otherwise `TemplateStart` … (then normal tokens for the embedded expression) … `TemplateMiddle`/`TemplateEnd`. The embedded expressions are lexed by the normal loop; the tricky part is matching the `}` that closes an interpolation. Implement a helper that, on `` ` ``, reads literal text until `${`, `` ` ``, or EOF:

```rust
            '`' => {
                i += 1;
                let (text, kind) = lex_template_chunk(&chars, &mut i, start)?;
                match kind {
                    TemplateChunk::Full => tokens.push(Token { tok: Tok::TemplateStr(text), span: Span::new(start, i) }),
                    TemplateChunk::Start => tokens.push(Token { tok: Tok::TemplateStart(text), span: Span::new(start, i) }),
                }
            }
```

Because nested `}` matching across the normal lexer loop is complex, use this simpler, self-contained strategy: when the lexer encounters `` ` ``, it lexes the ENTIRE template into a single synthetic token stream is overkill — instead lex chunk-by-chunk with a small interpolation-depth approach. Implement the helpers below and a `}` continuation:

```rust
enum TemplateChunk {
    Full,  // `...`           (no interpolation)
    Start, // `...${          (more follows)
}

/// Read template text starting just after a backtick (or after `}` that closes
/// an interpolation). Advances `i` past the terminating `` ` `` or `${`.
fn lex_template_chunk(chars: &[char], i: &mut usize, start: usize) -> Result<(String, TemplateChunk), AsError> {
    let mut text = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        if c == '`' {
            *i += 1;
            return Ok((text, TemplateChunk::Full));
        }
        if c == '$' && *i + 1 < chars.len() && chars[*i + 1] == '{' {
            *i += 2;
            return Ok((text, TemplateChunk::Start));
        }
        if c == '\\' && *i + 1 < chars.len() {
            // simple escapes inside templates: \` \$ \\ \n \t
            *i += 1;
            let e = chars[*i];
            text.push(match e {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            *i += 1;
            continue;
        }
        text.push(c);
        *i += 1;
    }
    Err(AsError::at("unterminated template string", Span::new(start, *i)))
}
```

Handling the close of an interpolation (`}` followed by more template text or the closing `` ` ``) requires the lexer to know it is "inside a template interpolation". Track this with a stack: push when emitting `TemplateStart`/`TemplateMiddle`, and when the normal `}` arm runs, if the template stack is non-empty AND the brace depth for the current interpolation is back to zero, lex the next chunk as `TemplateMiddle`/`TemplateEnd` instead of emitting a plain `RBrace`.

Implement this with two lexer-local counters: a `Vec<usize>` `template_stack` storing the `{`-nesting depth captured when each interpolation started, and a running `brace_depth`. Concretely: add `let mut brace_depth = 0usize;` and `let mut template_stack: Vec<usize> = Vec::new();` before the loop. In the `'{'` arm, `brace_depth += 1` (in addition to pushing `LBrace`). In the `'}'` arm:

```rust
            '}' => {
                if let Some(&open_depth) = template_stack.last() {
                    if brace_depth == open_depth {
                        // This `}` closes a template interpolation.
                        template_stack.pop();
                        i += 1;
                        let (text, kind) = lex_template_chunk(&chars, &mut i, start)?;
                        match kind {
                            TemplateChunk::Full => tokens.push(Token { tok: Tok::TemplateEnd(text), span: Span::new(start, i) }),
                            TemplateChunk::Start => tokens.push(Token { tok: Tok::TemplateMiddle(text), span: Span::new(start, i) }),
                        }
                        continue;
                    }
                }
                brace_depth -= 1;
                push(&mut tokens, Tok::RBrace, start, &mut i);
            }
```

And update the `` '`' `` and template-continuation emission to push the current `brace_depth` onto `template_stack` whenever a `TemplateStart`/`TemplateMiddle` is emitted:

```rust
            '`' => {
                i += 1;
                let (text, kind) = lex_template_chunk(&chars, &mut i, start)?;
                match kind {
                    TemplateChunk::Full => tokens.push(Token { tok: Tok::TemplateStr(text), span: Span::new(start, i) }),
                    TemplateChunk::Start => {
                        tokens.push(Token { tok: Tok::TemplateStart(text), span: Span::new(start, i) });
                        template_stack.push(brace_depth);
                    }
                }
            }
```

(For the `TemplateMiddle` case in the `'}'` arm, also `template_stack.push(brace_depth);` after popping, since another interpolation follows.) Adjust the `'{'` arm:

```rust
            '{' => {
                brace_depth += 1;
                push(&mut tokens, Tok::LBrace, start, &mut i);
            }
```

Add lexer tests:

```rust
    #[test]
    fn lexes_plain_template() {
        assert_eq!(kinds("`hello`"), vec![Tok::TemplateStr("hello".into()), Tok::Eof]);
    }

    #[test]
    fn lexes_interpolated_template() {
        // `a${x}b`  ->  Start("a") Ident(x) End("b")
        assert_eq!(
            kinds("`a${x}b`"),
            vec![
                Tok::TemplateStart("a".into()),
                Tok::Ident("x".into()),
                Tok::TemplateEnd("b".into()),
                Tok::Eof,
            ]
        );
    }
```

- [ ] **Step 3: `src/ast.rs`** — add a template expression (a list of literal/expr parts):

```rust
    Template { parts: Vec<TemplatePart> },
```

and:

```rust
#[derive(Clone, Debug)]
pub enum TemplatePart {
    Lit(String),
    Expr(Box<Expr>),
}
```

`Display`: `ExprKind::Template { .. } => write!(f, "(template)"),`

- [ ] **Step 4: `src/parser.rs`** — parse templates in `primary`. Add arms for the four template tokens:

```rust
            Tok::TemplateStr(s) => ExprKind::Template { parts: vec![crate::ast::TemplatePart::Lit(s)] },
            Tok::TemplateStart(s) => {
                let mut parts = vec![crate::ast::TemplatePart::Lit(s)];
                loop {
                    let e = self.expr()?;
                    parts.push(crate::ast::TemplatePart::Expr(Box::new(e)));
                    match self.advance() {
                        Tok::TemplateMiddle(s) => parts.push(crate::ast::TemplatePart::Lit(s)),
                        Tok::TemplateEnd(s) => {
                            parts.push(crate::ast::TemplatePart::Lit(s));
                            break;
                        }
                        other => {
                            return Err(AsError::at(
                                format!("malformed template, found {:?}", other),
                                self.tokens[self.pos - 1].span,
                            ))
                        }
                    }
                }
                ExprKind::Template { parts }
            }
```

(These go in `primary`'s `match self.advance()`; they produce a `kind` assembled into `Expr { kind, span: tok_span }` by the existing tail — but because `TemplateStart` consumes more tokens, compute the span as `Span::new(tok_span.start, self.prev_end())`. Easiest: build and `return Ok(Expr { … })` directly inside each arm, like the array/object arms do.)

- [ ] **Step 5: `src/interp.rs`** — evaluate a template by concatenating parts (literals as-is, expressions via their `Display`):

```rust
            ExprKind::Template { parts } => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        crate::ast::TemplatePart::Lit(s) => out.push_str(s),
                        crate::ast::TemplatePart::Expr(e) => {
                            let v = self.eval_expr(e, env).await?;
                            out.push_str(&v.to_string());
                        }
                    }
                }
                Ok(Value::Str(out.into()))
            }
```

Add tests:

```rust
    #[tokio::test]
    async fn template_string_interpolates() {
        let src = "let name = \"Ada\"\nlet n = 3\nprint(`hi ${name}, ${n + 1} times`)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "hi Ada, 4 times\n");
    }

    #[tokio::test]
    async fn nested_template_and_plain() {
        let src = "print(`outer ${ `inner ${1 + 1}` } end`)";
        let stmts = parse(&lex(src).unwrap()).unwrap();
        let mut interp = Interp::new();
        let env = global_env();
        interp.exec(&stmts, &env).await.unwrap();
        assert_eq!(interp.output, "outer inner 2 end\n");
    }
```

- [ ] **Step 6: Run** `cargo test` + `cargo clippy --all-targets`.

- [ ] **Step 7: Commit**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/interp.rs
git commit -m "feat: add template strings with interpolation"
```

---

## Task 7: End-to-end demo + integration test

**Files:** `examples/data.as` (new), `tests/cli.rs` (modify).

- [ ] **Step 1: Create `examples/data.as`**

```
let people = [
  { name: "Ada", age: 36 },
  { name: "Alan", age: 41 },
  { name: "Grace", age: 45 },
]

let total = 0
for (p of people) {
  total += p.age
}

let oldest = people[0]
for (p of people) {
  if (p.age > oldest.age) { oldest = p }
}

print(`count: ${total / people.length ?? 0}`)
print(oldest.name)
```

WAIT: `.length` is a property/method not yet implemented (it belongs to `std/array` in M8). Do NOT use `.length` in the example. Use this corrected example instead:

```
let people = [
  { name: "Ada", age: 36 },
  { name: "Alan", age: 41 },
  { name: "Grace", age: 45 },
]

let total = 0
let count = 0
for (p of people) {
  total += p.age
  count += 1
}

let oldest = people[0]
for (p of people) {
  if (p.age > oldest.age) { oldest = p }
}

print(`sum of ages: ${total}`)
print(`average: ${total / count}`)
print(oldest.name)
```

- [ ] **Step 2: Add an integration test to `tests/cli.rs`**

```rust
#[test]
fn runs_data_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin)
        .arg("run")
        .arg("examples/data.as")
        .output()
        .unwrap();

    assert!(output.status.success(), "process failed: {:?}", output);
    // ages 36+41+45 = 122; average 122/3 ≈ 40.66...; oldest is Grace
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("sum of ages: 122"));
    assert!(out.contains("Grace"));
}
```

- [ ] **Step 3: Run** `cargo test` (incl. `runs_data_example`), then `cargo run --quiet -- run examples/data.as` (paste output). Run `cargo clippy --all-targets`.

- [ ] **Step 4: Commit**

```bash
git add examples/data.as tests/cli.rs
git commit -m "test: add data-structures end-to-end example"
```

---

## Definition of Done

- `cargo test` passes (all unit + integration); `cargo clippy --all-targets` clean.
- `cargo run -- run examples/data.as` prints the ages sum (122), average, and `Grace`.
- AScript supports: array literals + indexing (read/write), object literals + member/computed access (read/write), optional chaining `?.`, l-value assignment to members/indices, `for (x of …)` over arrays and strings, and template strings.
- Arrays/objects compare by identity; `Display` is cycle-safe.

## Hand-off to Milestone 5 ("Result & error model")

Adds `Ok`/`Err`, the `?` propagation operator, the Result tier vs panic tier distinction, and the `recover` boundary (spec §6). The lexer already reserves lone `?` with an M5-pointing message; out-of-bounds indexing (currently a plain error) will be reclassified into the panic tier. `array_index`/`read_member` errors are the natural first panic sites.
