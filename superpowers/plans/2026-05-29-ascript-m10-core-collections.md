# AScript Milestone 10 — Core Collections Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the first standard-library milestone (spec §11.1–§11.2 "Data & text" core): the always-global `core` functions (`len`, `type`, `range`), the `std/*` built-in-module resolution hook + native-dispatch infrastructure, and the six core collection modules `std/string`, `std/array`, `std/object`, `std/map`, `std/math`, `std/convert`. This milestone also introduces the **`Map` value kind** + the **`map<K,V>` contract type** (the one §4/§5 language item deferred to the stdlib), and closes a discovered Phase-1 spec gap: **array destructuring binding** `let [a, b] = expr` (spec §6), the idiom for consuming the `[value, err]` stdlib.

**Architecture:** Stdlib functions are exposed as ordinary `Value::Builtin(Rc<str>)` values whose name is **qualified** (`"math.abs"`, `"string.split"`). Importing `std/string` binds each export name (`split`) to its qualified builtin (`Value::Builtin("string.split")`) or, for constants, to a literal value (`pi → Value::Number(π)`). Calling lands in `call_builtin`, which routes any `module.func` name to `Interp::call_stdlib` (defined in a new `src/stdlib/` module tree). Native functions are therefore indistinguishable from user functions at the call site (spec §11.3). The `std/*` resolution hook lives in the `Stmt::Import` handler: a `"std/"`-prefixed source bypasses the filesystem and builds a `ModuleEntry` from a static export registry, cached under a synthetic `<std>/…` key.

**Tech Stack:** Rust 2021, tokio (current_thread), async-recursion, indexmap. **No new crates** (`math.random` uses a tiny self-seeded xorshift PRNG; all six modules are pure Rust over existing `Value`s).

**Starting state (end of M9, on `main`):** Full language + tooling, ~148 tests (128 lib + 14 cli + 4 module + 2 conformance), clippy clean. `global_env()` installs `print, Ok, Err, assert, recover, test`. `call_builtin(name, args, span)` dispatches those by name. `Value` has no `Map`. `Type` has no `Map`. `parse_type_atom` errors on `map`. `let_stmt` accepts a single identifier only. The Tree-sitter grammar **already** accepts `let [a,b] = …` (`array_pattern`) and `map<K,V>` (`map_type`) — so no grammar regeneration is needed; the interpreter parser must catch up and the conformance test will verify both agree.

**Conventions:** spans are char offsets; single-threaded `Rc`/`RefCell` (never `Arc`); `#[async_recursion(?Send)]`; the `Control` error channel (`Panic` = Tier-2 unrecoverable, `Propagate` = `?` Result short-circuit). Per spec §11.3: a fallible stdlib op returns a Tier-1 `[value, err]` Result; **misuse (wrong argument type) is a Tier-2 panic** via the contract system.

## Spec semantics decided (read before implementing)

- **Functional, collection-first API.** Every stdlib function takes its subject as the first argument: `string.split(s, sep)`, `array.map(arr, fn)`, `map.get(m, k)`. There is **no** method-style dispatch on primitives — `read_member` is untouched.
- **Argument-type misuse panics** (Tier-2), via shared `want_*` helpers. Out-of-range or "not found" lookups return `nil` (the checked-accessor convention) — they do **not** panic.
- **Genuinely fallible ops return Tier-1 Results.** Only `std/convert.parseNumber` / `parseInt` are fallible (bad input is a runtime value condition, not a bug) → `[number, err]`.
- **`Map`** is identity-compared (like arrays/objects), truthy, displays as `map {key: value, …}`, `type` is `"map"`. Keys are `nil`/`bool`/`number`/`string` (hashable); a non-hashable key (array/object/function/…) is a Tier-2 panic. Insertion order is preserved (`IndexMap`).
- **No map literal syntax** — maps are built by `std/map.new()`.
- **Destructuring** `let [a, b] = expr` requires `expr` to evaluate to an array; missing elements bind `nil`, extra elements are ignored (JS-shaped). Only plain identifiers in the pattern (matches the grammar's `array_pattern`). No type annotation on a destructuring binding.

## File structure

| File | Responsibility | Change |
|---|---|---|
| `src/value.rs` | `Value::Map` + `MapKey` (+ its 6 trait/match arms) | modify |
| `src/ast.rs` | `Type::Map`, `Stmt::LetDestructure` | modify |
| `src/parser.rs` | `map<K,V>` type parse; `let [a,b]` parse | modify |
| `src/fmt.rs` | format `Stmt::LetDestructure` | modify |
| `src/interp.rs` | `Map`/`Type::Map` arms; `len`/`type`/`range` core builtins; `LetDestructure` exec; `std/` import hook; `call_stdlib` route; `pub(crate)` on `make_pair`/`make_error` | modify |
| `src/stdlib/mod.rs` | export registry, `call_stdlib` router, shared arg helpers | create |
| `src/stdlib/math.rs` | `std/math` | create |
| `src/stdlib/string.rs` | `std/string` | create |
| `src/stdlib/array.rs` | `std/array` (callback fns → `impl Interp`) | create |
| `src/stdlib/object.rs` | `std/object` | create |
| `src/stdlib/map.rs` | `std/map` | create |
| `src/stdlib/convert.rs` | `std/convert` | create |
| `src/lib.rs` | `pub mod stdlib;` | modify |
| `examples/stdlib.as` | end-to-end example | create |
| `tests/cli.rs` | example integration test | modify |

## Scope & Justified Deferrals

| Deferred | Why | Owning milestone |
|---|---|---|
| `std/regex`, `std/json`, `std/csv`, `std/toml`, `std/yaml`, `std/encoding`, `std/bytes`, `std/uuid` | Serialization/encoding group | **M11** |
| `std/time`, `std/date`, `std/intl` | Time & locale group | **M12** |
| Object destructuring `let {a, b} = obj` | Not in the spec (spec §6 shows only array destructuring); not dependency-blocked, simply out of spec scope | n/a (not a spec feature) |

Nothing in M10's own scope is deferred.

---

## Task 1: Array destructuring binding `let [a, b] = expr` (Phase-1 spec gap, §6)

**Files:** Modify `src/ast.rs`, `src/parser.rs`, `src/interp.rs`, `src/fmt.rs`.

- [ ] **Step 1: `src/ast.rs`** — add a statement variant next to `Stmt::Let`:
```rust
    LetDestructure { names: Vec<String>, value: Expr, mutable: bool },
```

- [ ] **Step 2: Write failing parser test.** In `src/parser.rs` `#[cfg(test)] mod tests`, add:
```rust
    #[test]
    fn parses_array_destructuring_let() {
        let toks = lex("let [a, b] = pair").unwrap();
        let prog = parse(&toks).unwrap();
        match &prog[0] {
            Stmt::LetDestructure { names, mutable, .. } => {
                assert_eq!(names, &["a".to_string(), "b".to_string()]);
                assert!(*mutable);
            }
            other => panic!("expected LetDestructure, got {other:?}"),
        }
    }
```
Run: `cargo test --lib parses_array_destructuring_let` → Expected: FAIL (no `LetDestructure`).

- [ ] **Step 3: `src/parser.rs`** — in `let_stmt`, branch on `[` after consuming `let`/`const`. Replace the body of `let_stmt` so that after `self.advance(); // consume let/const`, it reads:
```rust
        if *self.peek() == Tok::LBracket {
            self.advance(); // consume '['
            let mut names = Vec::new();
            if *self.peek() != Tok::RBracket {
                loop {
                    match self.advance() {
                        Tok::Ident(n) => names.push(n),
                        other => return Err(AsError::at(
                            format!("expected an identifier in destructuring pattern, found {:?}", other),
                            self.tokens[self.pos - 1].span,
                        )),
                    }
                    if *self.peek() == Tok::Comma {
                        self.advance();
                        if *self.peek() == Tok::RBracket { break; }
                    } else {
                        break;
                    }
                }
            }
            self.eat(&Tok::RBracket)?;
            self.eat(&Tok::Eq)?;
            let value = self.expr()?;
            return Ok(Stmt::LetDestructure { names, value, mutable });
        }
```
(Insert this block immediately after the `self.advance();` that consumes the `let`/`const` keyword and BEFORE the existing single-identifier parsing.)

- [ ] **Step 4: Run** `cargo test --lib parses_array_destructuring_let` → Expected: PASS.

- [ ] **Step 5: `src/fmt.rs`** — add a `Stmt::LetDestructure` arm next to `Stmt::Let`:
```rust
        Stmt::LetDestructure { names, value, mutable } => {
            indent(out, level);
            out.push_str(if *mutable { "let " } else { "const " });
            out.push('[');
            out.push_str(&names.join(", "));
            out.push_str("] = ");
            write_expr(out, value, 0);
            out.push('\n');
        }
```

- [ ] **Step 6: `src/interp.rs`** — add the exec arm in `exec_stmt`, after the `Stmt::Let` arm:
```rust
            Stmt::LetDestructure { names, value, mutable } => {
                let v = self.eval_expr(value, env).await?;
                let items = match v {
                    Value::Array(a) => a.borrow().clone(),
                    other => {
                        return Err(AsError::at(
                            format!("cannot destructure a non-array value of type {}", type_name(&other)),
                            value.span,
                        )
                        .into())
                    }
                };
                for (i, name) in names.iter().enumerate() {
                    let elem = items.get(i).cloned().unwrap_or(Value::Nil);
                    env.define(name, elem, *mutable).map_err(AsError::new)?;
                }
                Ok(Flow::Normal)
            }
```

- [ ] **Step 6.5: Add shared test helpers** to `src/interp.rs`'s `#[cfg(test)] mod tests` (the module currently has NO `run`/`run_err` helper — tests hand-roll `Interp::new()` + `exec`; these helpers DRY that up and every M10 test below uses them). Place near the top of the test module, after the existing `panic_of` helper:
```rust
    /// Lex+parse+exec a program string, returning its captured `print` output.
    /// Panics (test failure) on a lex/parse error or a runtime panic.
    async fn run(src: &str) -> String {
        let mut interp = Interp::new();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env();
        interp.exec(&stmts, &env).await.expect("program panicked");
        interp.output
    }

    /// Like `run`, but expects a runtime panic and returns its `AsError`.
    async fn run_err(src: &str) -> AsError {
        let mut interp = Interp::new();
        let tokens = lex(src).expect("lex");
        let stmts = parse(&tokens).expect("parse");
        let env = global_env();
        match interp.exec(&stmts, &env).await {
            Err(Control::Panic(e)) => e,
            Ok(_) => panic!("expected a runtime panic, but the program succeeded"),
            Err(Control::Propagate(_)) => panic!("expected a panic, got a `?` propagation"),
        }
    }
```
(`lex`, `parse`, `global_env`, `Interp`, `Control`, `AsError`, `Value` are already in scope via the module's `use super::*;` + `use crate::lexer::lex; use crate::parser::parse;`.)

- [ ] **Step 7: Write failing interp test.** In `src/interp.rs` tests add:
```rust
    #[tokio::test]
    async fn destructures_array_into_bindings() {
        let out = run("let [a, b] = [1, 2]\nprint(a)\nprint(b)\nlet [x, y] = [9]\nprint(x)\nprint(y)").await;
        assert_eq!(out, "1\n2\n9\nnil\n");
    }

    #[tokio::test]
    async fn destructuring_non_array_panics() {
        let err = run_err("let [a, b] = 5").await;
        assert!(err.message.contains("cannot destructure"));
    }
```
(If the existing helpers are named differently, mirror an adjacent test's harness exactly — do not invent new helpers if equivalents exist.)

- [ ] **Step 8: Run** `cargo test --lib destructur` → Expected: PASS. Run `cargo test` and `cargo clippy --all-targets` → all green/clean. **Commit:**
```bash
git add -A && git commit -m "feat: array destructuring binding 'let [a, b] = expr' (spec §6)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Core globals `len`, `type`, `range` (spec §11.1)

**Files:** Modify `src/interp.rs`.

- [ ] **Step 1: Write failing tests.** In `src/interp.rs` tests:
```rust
    #[tokio::test]
    async fn core_len_type_range() {
        assert_eq!(run("print(len([1,2,3]))").await, "3\n");
        assert_eq!(run("print(len(\"hello\"))").await, "5\n");
        assert_eq!(run("print(len({a:1, b:2}))").await, "2\n");
        assert_eq!(run("print(type(1))").await, "number\n");
        assert_eq!(run("print(type(\"x\"))").await, "string\n");
        assert_eq!(run("print(type([1]))").await, "array\n");
        assert_eq!(run("print(type(nil))").await, "nil\n");
        assert_eq!(run("print(range(3))").await, "[0, 1, 2]\n");
        assert_eq!(run("print(range(2, 5))").await, "[2, 3, 4]\n");
        assert_eq!(run("print(range(0, 10, 3))").await, "[0, 3, 6, 9]\n");
        assert_eq!(run("print(range(5, 0, -2))").await, "[5, 3, 1]\n");
    }

    #[tokio::test]
    async fn len_of_wrong_type_panics() {
        let err = run_err("len(5)").await;
        assert!(err.message.contains("len"));
    }
```
Run: `cargo test --lib core_len_type_range` → Expected: FAIL (`'len' is not a function`).

- [ ] **Step 2: `src/interp.rs`** — register the globals. In `global_env()` extend the name list:
```rust
    for name in ["print", "Ok", "Err", "assert", "recover", "test", "len", "type", "range"] {
```

- [ ] **Step 3: `src/interp.rs`** — add three arms to `call_builtin` (before the final `other =>`):
```rust
            "len" => {
                let v = args.first().cloned().unwrap_or(Value::Nil);
                let n = match &v {
                    Value::Str(s) => s.chars().count(),
                    Value::Array(a) => a.borrow().len(),
                    Value::Object(o) => o.borrow().len(),
                    Value::Map(m) => m.borrow().len(),
                    _ => {
                        return Err(AsError::at(
                            format!("len() expects a string, array, object, or map, got {}", type_name(&v)),
                            span,
                        )
                        .into())
                    }
                };
                Ok(Value::Number(n as f64))
            }
            "type" => {
                let v = args.first().cloned().unwrap_or(Value::Nil);
                Ok(Value::Str(type_name(&v).into()))
            }
            "range" => {
                let want_num = |i: usize| -> Result<f64, Control> {
                    match args.get(i) {
                        Some(Value::Number(n)) => Ok(*n),
                        Some(v) => Err(AsError::at(
                            format!("range() expects number arguments, got {}", type_name(v)),
                            span,
                        )
                        .into()),
                        None => Ok(0.0),
                    }
                };
                let (start, end, step) = match args.len() {
                    1 => (0.0, want_num(0)?, 1.0),
                    2 => (want_num(0)?, want_num(1)?, 1.0),
                    3 => (want_num(0)?, want_num(1)?, want_num(2)?),
                    n => {
                        return Err(AsError::at(
                            format!("range() expects 1 to 3 arguments, got {}", n),
                            span,
                        )
                        .into())
                    }
                };
                if step == 0.0 {
                    return Err(AsError::at("range() step must not be zero", span).into());
                }
                let mut out = Vec::new();
                let mut i = start;
                if step > 0.0 {
                    while i < end {
                        out.push(Value::Number(i));
                        i += step;
                    }
                } else {
                    while i > end {
                        out.push(Value::Number(i));
                        i += step;
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(out))))
            }
```
(Note: the `Value::Map(m)` arm in `len` will not compile until Task 5 adds `Value::Map`. To keep this task self-contained and green, **temporarily omit the `Value::Map` line** here and add it in Task 5's Step where `Value::Map` is introduced. The test above does not exercise `len` on a map.)

- [ ] **Step 4: Run** `cargo test --lib core_len_type_range len_of_wrong_type_panics` → Expected: PASS. Run `cargo test` + `cargo clippy --all-targets` → green/clean. **Commit:**
```bash
git add -A && git commit -m "feat: core globals len, type, range (spec §11.1)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: `std/*` resolution hook + native dispatch infra + `std/math`

**Files:** Create `src/stdlib/mod.rs`, `src/stdlib/math.rs`. Modify `src/lib.rs`, `src/interp.rs`.

- [ ] **Step 1: `src/lib.rs`** — add the module declaration alongside the others:
```rust
pub mod stdlib;
```

- [ ] **Step 2: `src/interp.rs`** — make the Result builders reusable by the stdlib. Change:
```rust
fn make_pair(value: Value, err: Value) -> Value {
```
to
```rust
pub(crate) fn make_pair(value: Value, err: Value) -> Value {
```
and likewise `fn make_error(` → `pub(crate) fn make_error(`. Also make `type_name` reusable: change `fn type_name(` → `pub(crate) fn type_name(`. And make the call dispatcher reusable by `std/array` callbacks: change `async fn call_value(` → `pub(crate) async fn call_value(` (it keeps its `#[async_recursion(?Send)]` attribute).

- [ ] **Step 3: `src/interp.rs`** — add the `std/` import hook. In the `Stmt::Import` arm, replace the first two lines:
```rust
                let resolved = self.resolve_import(source);
                let entry = self.load_module(&resolved).await?;
```
with:
```rust
                let entry = if source.starts_with("std/") {
                    self.load_std_module(source)?
                } else {
                    let resolved = self.resolve_import(source);
                    self.load_module(&resolved).await?
                };
```

- [ ] **Step 4: `src/interp.rs`** — add the std module loader method (next to `load_module`):
```rust
    /// Resolve a `std/*` built-in module to a cached `ModuleEntry`, building it
    /// from the static export registry. Bypasses the filesystem entirely.
    fn load_std_module(&mut self, source: &str) -> Result<ModuleEntry, Control> {
        let key = PathBuf::from(format!("<std>/{}", &source[4..]));
        if let Some(entry) = self.modules.get(&key) {
            return Ok(entry.clone());
        }
        let exports_list = crate::stdlib::std_module_exports(source).ok_or_else(|| {
            Control::Panic(AsError::new(format!("unknown standard library module '{}'", source)))
        })?;
        let env = global_env();
        let exports = Rc::new(RefCell::new(HashSet::new()));
        for (name, value) in exports_list {
            env.define(&name, value, false).map_err(AsError::new)?;
            exports.borrow_mut().insert(name);
        }
        let entry = ModuleEntry { env, exports };
        self.modules.insert(key, entry.clone());
        Ok(entry)
    }
```

- [ ] **Step 5: `src/interp.rs`** — route qualified builtins. In `call_builtin`, change the final arm:
```rust
            other => Err(AsError::at(format!("'{}' is not a function", other), span).into()),
```
to:
```rust
            other => {
                if let Some((module, func)) = other.split_once('.') {
                    self.call_stdlib(module, func, args, span).await
                } else {
                    Err(AsError::at(format!("'{}' is not a function", other), span).into())
                }
            }
```

- [ ] **Step 6: Create `src/stdlib/mod.rs`** — registry, router, shared helpers:
```rust
//! The AScript standard library: `std/*` modules implemented as native Rust
//! over the existing `Value` model. Each module exposes an `exports()` binding
//! list (imported names → `Value`) and a `call` entry the interpreter routes
//! qualified builtin names (`"math.abs"`) to. Per spec §11.3, native functions
//! are ordinary `function` values; argument-type misuse is a Tier-2 panic.

pub mod array;
pub mod convert;
pub mod map;
pub mod math;
pub mod object;
pub mod string;

use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use std::rc::Rc;

/// A native builtin value with a qualified name (`"math.abs"`).
pub(crate) fn bi(qualified: &str) -> Value {
    Value::Builtin(qualified.into())
}

/// The export list (binding name → value) for a `std/*` module path, or `None`
/// if `path` is not a known stdlib module.
pub fn std_module_exports(path: &str) -> Option<Vec<(String, Value)>> {
    let list: Vec<(&'static str, Value)> = match path {
        "std/math" => math::exports(),
        "std/string" => string::exports(),
        "std/array" => array::exports(),
        "std/object" => object::exports(),
        "std/map" => map::exports(),
        "std/convert" => convert::exports(),
        _ => return None,
    };
    Some(list.into_iter().map(|(n, v)| (n.to_string(), v)).collect())
}

impl Interp {
    /// Dispatch a qualified stdlib builtin (`module` = "math", `func` = "abs").
    pub(crate) async fn call_stdlib(
        &mut self,
        module: &str,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match module {
            "math" => math::call(func, args, span),
            "string" => string::call(func, args, span),
            "object" => object::call(func, args, span),
            "map" => map::call(func, args, span),
            "convert" => convert::call(func, args, span),
            // `array` callbacks invoke user functions, so it needs `&mut self`.
            "array" => self.call_array(func, args, span).await,
            _ => Err(AsError::at(format!("unknown stdlib module '{}'", module), span).into()),
        }
    }
}

// ---- shared argument helpers (Tier-2 panic on type misuse, spec §11.3) ----

pub(crate) fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Nil)
}

pub(crate) fn want_number(v: &Value, span: Span, ctx: &str) -> Result<f64, Control> {
    match v {
        Value::Number(n) => Ok(*n),
        _ => Err(AsError::at(format!("{} expects a number, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

pub(crate) fn want_string(v: &Value, span: Span, ctx: &str) -> Result<Rc<str>, Control> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        _ => Err(AsError::at(format!("{} expects a string, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

pub(crate) fn want_array(v: &Value, span: Span, ctx: &str) -> Result<Rc<std::cell::RefCell<Vec<Value>>>, Control> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        _ => Err(AsError::at(format!("{} expects an array, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

pub(crate) fn want_object(v: &Value, span: Span, ctx: &str) -> Result<Rc<std::cell::RefCell<indexmap::IndexMap<String, Value>>>, Control> {
    match v {
        Value::Object(o) => Ok(o.clone()),
        _ => Err(AsError::at(format!("{} expects an object, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

/// Resolve a possibly-negative index against a length, clamping into `0..=len`.
/// Used by `slice` (string/array). Negative counts from the end.
pub(crate) fn clamp_index(i: f64, len: usize) -> usize {
    if i < 0.0 {
        let from_end = len as f64 + i;
        if from_end < 0.0 { 0 } else { from_end as usize }
    } else if i as usize > len {
        len
    } else {
        i as usize
    }
}
```

- [ ] **Step 7: Create `src/stdlib/math.rs`** (spec §11.2: abs, floor, ceil, round, sqrt, pow, min, max, random, pi, e):
```rust
//! `std/math` — numeric functions and constants.

use super::{arg, bi, want_number};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use std::cell::Cell;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("abs", bi("math.abs")),
        ("floor", bi("math.floor")),
        ("ceil", bi("math.ceil")),
        ("round", bi("math.round")),
        ("sqrt", bi("math.sqrt")),
        ("pow", bi("math.pow")),
        ("min", bi("math.min")),
        ("max", bi("math.max")),
        ("random", bi("math.random")),
        ("pi", Value::Number(std::f64::consts::PI)),
        ("e", Value::Number(std::f64::consts::E)),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("math.{}", f);
    match func {
        "abs" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("abs"))?.abs())),
        "floor" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("floor"))?.floor())),
        "ceil" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("ceil"))?.ceil())),
        // round half away from zero (matches most scripting languages)
        "round" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("round"))?.round())),
        "sqrt" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("sqrt"))?.sqrt())),
        "pow" => {
            let b = want_number(&arg(args, 0), span, &ctx("pow"))?;
            let e = want_number(&arg(args, 1), span, &ctx("pow"))?;
            Ok(Value::Number(b.powf(e)))
        }
        "min" | "max" => {
            if args.is_empty() {
                return Err(AsError::at(format!("math.{} requires at least one argument", func), span).into());
            }
            let nums: Result<Vec<f64>, Control> =
                args.iter().map(|v| want_number(v, span, &ctx(func))).collect();
            let nums = nums?;
            let acc = if func == "min" {
                nums.iter().copied().fold(f64::INFINITY, f64::min)
            } else {
                nums.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            };
            Ok(Value::Number(acc))
        }
        "random" => Ok(Value::Number(next_random())),
        _ => Err(AsError::at(format!("std/math has no function '{}'", func), span).into()),
    }
}

// A tiny self-seeded xorshift64* PRNG (no external crate). Seeded once from the
// system clock + a per-process counter; advances thread-locally. Adequate for
// scripting `math.random()`; NOT cryptographic (see std/crypto, M13).
thread_local! {
    static RNG: Cell<u64> = Cell::new(seed());
}

fn seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    // Mix with the address of a stack local for a little extra entropy.
    let local = 0u8;
    let addr = &local as *const u8 as u64;
    (nanos ^ addr).max(1)
}

fn next_random() -> f64 {
    RNG.with(|cell| {
        let mut x = cell.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        cell.set(x);
        let r = x.wrapping_mul(0x2545F4914F6CDD1D);
        // Top 53 bits → a float in [0, 1).
        (r >> 11) as f64 / (1u64 << 53) as f64
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(call("abs", &[Value::Number(-3.0)], Span::new(0, 0)).unwrap(), Value::Number(3.0));
        assert_eq!(call("floor", &[Value::Number(2.9)], Span::new(0, 0)).unwrap(), Value::Number(2.0));
        assert_eq!(call("pow", &[Value::Number(2.0), Value::Number(10.0)], Span::new(0, 0)).unwrap(), Value::Number(1024.0));
        assert_eq!(call("max", &[Value::Number(1.0), Value::Number(9.0), Value::Number(4.0)], Span::new(0, 0)).unwrap(), Value::Number(9.0));
    }

    #[test]
    fn random_in_range() {
        for _ in 0..1000 {
            let r = next_random();
            assert!((0.0..1.0).contains(&r), "random out of range: {r}");
        }
    }

    #[test]
    fn type_misuse_panics() {
        let e = call("sqrt", &[Value::Str("x".into())], Span::new(0, 0));
        assert!(matches!(e, Err(Control::Panic(_))));
    }
}
```

- [ ] **Step 8: Write the end-to-end import test.** In `src/interp.rs` tests:
```rust
    #[tokio::test]
    async fn imports_std_math() {
        let out = run("import * as math from \"std/math\"\nprint(math.abs(-5))\nprint(math.pow(2, 8))\nprint(math.pi > 3.14)").await;
        assert_eq!(out, "5\n256\ntrue\n");
    }

    #[tokio::test]
    async fn named_import_from_std() {
        let out = run("import { sqrt, max } from \"std/math\"\nprint(sqrt(144))\nprint(max(3, 7, 2))").await;
        assert_eq!(out, "12\n7\n");
    }

    #[tokio::test]
    async fn unknown_std_module_errors() {
        let err = run_err("import { x } from \"std/nope\"").await;
        assert!(err.message.contains("unknown standard library module"));
    }
```

- [ ] **Step 9: Run** `cargo test` (incl. the new lib + `stdlib::math::tests`) and `cargo clippy --all-targets`. Green/clean. **Commit:**
```bash
git add -A && git commit -m "feat: std/* resolution hook, native dispatch infra, and std/math

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: `std/string` (spec §11.2)

**Files:** Create `src/stdlib/string.rs`. (Module already declared in `mod.rs`.)

API: `split(s, sep)`, `join(arr, sep)`, `slice(s, start, end?)`, `trim(s)`, `upper(s)`, `lower(s)`, `find(s, sub)` → index or `-1`, `replace(s, from, to)` → all occurrences, `format(template, ...args)` → `{}` positional / `{{`/`}}` escapes, `padStart(s, width, fill)`, `padEnd(s, width, fill)`, `repeat(s, n)`.

- [ ] **Step 1: Create `src/stdlib/string.rs`:**
```rust
//! `std/string` — string manipulation.

use super::{arg, bi, clamp_index, want_array, want_number, want_string};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("split", bi("string.split")),
        ("join", bi("string.join")),
        ("slice", bi("string.slice")),
        ("trim", bi("string.trim")),
        ("upper", bi("string.upper")),
        ("lower", bi("string.lower")),
        ("find", bi("string.find")),
        ("replace", bi("string.replace")),
        ("format", bi("string.format")),
        ("padStart", bi("string.padStart")),
        ("padEnd", bi("string.padEnd")),
        ("repeat", bi("string.repeat")),
    ]
}

fn str_val(s: String) -> Value {
    Value::Str(s.into())
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("string.{}", f);
    match func {
        "split" => {
            let s = want_string(&arg(args, 0), span, &ctx("split"))?;
            let sep = want_string(&arg(args, 1), span, &ctx("split"))?;
            let parts: Vec<Value> = if sep.is_empty() {
                s.chars().map(|c| str_val(c.to_string())).collect()
            } else {
                s.split(sep.as_ref()).map(|p| str_val(p.to_string())).collect()
            };
            Ok(Value::Array(Rc::new(RefCell::new(parts))))
        }
        "join" => {
            let arr = want_array(&arg(args, 0), span, &ctx("join"))?;
            let sep = want_string(&arg(args, 1), span, &ctx("join"))?;
            let pieces: Vec<String> = arr.borrow().iter().map(|v| v.to_string()).collect();
            Ok(str_val(pieces.join(sep.as_ref())))
        }
        "slice" => {
            let s = want_string(&arg(args, 0), span, &ctx("slice"))?;
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len();
            let start = clamp_index(want_number(&arg(args, 1), span, &ctx("slice"))?, len);
            let end = match args.get(2) {
                None | Some(Value::Nil) => len,
                Some(v) => clamp_index(want_number(v, span, &ctx("slice"))?, len),
            };
            let slice: String = if start < end { chars[start..end].iter().collect() } else { String::new() };
            Ok(str_val(slice))
        }
        "trim" => Ok(str_val(want_string(&arg(args, 0), span, &ctx("trim"))?.trim().to_string())),
        "upper" => Ok(str_val(want_string(&arg(args, 0), span, &ctx("upper"))?.to_uppercase())),
        "lower" => Ok(str_val(want_string(&arg(args, 0), span, &ctx("lower"))?.to_lowercase())),
        "find" => {
            let s = want_string(&arg(args, 0), span, &ctx("find"))?;
            let sub = want_string(&arg(args, 1), span, &ctx("find"))?;
            // Return the char index of the first match, or -1.
            match s.find(sub.as_ref()) {
                Some(byte_idx) => Ok(Value::Number(s[..byte_idx].chars().count() as f64)),
                None => Ok(Value::Number(-1.0)),
            }
        }
        "replace" => {
            let s = want_string(&arg(args, 0), span, &ctx("replace"))?;
            let from = want_string(&arg(args, 1), span, &ctx("replace"))?;
            let to = want_string(&arg(args, 2), span, &ctx("replace"))?;
            // Replace ALL occurrences (empty `from` returns the input unchanged).
            let result = if from.is_empty() { s.to_string() } else { s.replace(from.as_ref(), to.as_ref()) };
            Ok(str_val(result))
        }
        "format" => {
            let template = want_string(&arg(args, 0), span, &ctx("format"))?;
            Ok(str_val(format_template(&template, &args[1.min(args.len())..], span)?))
        }
        "padStart" | "padEnd" => {
            let s = want_string(&arg(args, 0), span, &ctx(func))?;
            let width = want_number(&arg(args, 1), span, &ctx(func))? as usize;
            let fill = match args.get(2) {
                None | Some(Value::Nil) => " ".to_string(),
                Some(v) => want_string(v, span, &ctx(func))?.to_string(),
            };
            let cur = s.chars().count();
            if cur >= width || fill.is_empty() {
                return Ok(str_val(s.to_string()));
            }
            let need = width - cur;
            let pad: String = fill.chars().cycle().take(need).collect();
            let result = if func == "padStart" { format!("{}{}", pad, s) } else { format!("{}{}", s, pad) };
            Ok(str_val(result))
        }
        "repeat" => {
            let s = want_string(&arg(args, 0), span, &ctx("repeat"))?;
            let n = want_number(&arg(args, 1), span, &ctx("repeat"))?;
            if n < 0.0 {
                return Err(AsError::at("string.repeat count must be non-negative", span).into());
            }
            Ok(str_val(s.repeat(n as usize)))
        }
        _ => Err(AsError::at(format!("std/string has no function '{}'", func), span).into()),
    }
}

/// `format("Hello {}, you are {}", name, age)`. `{}` consumes the next argument
/// in order; `{{` and `}}` are literal braces. Too few args → panic.
fn format_template(template: &str, args: &[Value], span: Span) -> Result<String, Control> {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    let mut next = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' if chars.peek() == Some(&'}') => {
                chars.next();
                match args.get(next) {
                    Some(v) => out.push_str(&v.to_string()),
                    None => return Err(AsError::at("string.format: not enough arguments for placeholders", span).into()),
                }
                next += 1;
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(x: &str) -> Value { Value::Str(x.into()) }
    fn sp() -> Span { Span::new(0, 0) }

    #[test]
    fn splits_and_joins() {
        let parts = call("split", &[s("a,b,c"), s(",")], sp()).unwrap();
        assert_eq!(parts.to_string(), "[\"a\", \"b\", \"c\"]");
        let arr = parts;
        let joined = call("join", &[arr, s("-")], sp()).unwrap();
        assert_eq!(joined, s("a-b-c"));
    }

    #[test]
    fn slice_trim_case() {
        assert_eq!(call("slice", &[s("hello"), Value::Number(1.0), Value::Number(4.0)], sp()).unwrap(), s("ell"));
        assert_eq!(call("slice", &[s("hello"), Value::Number(-2.0)], sp()).unwrap(), s("lo"));
        assert_eq!(call("trim", &[s("  hi  ")], sp()).unwrap(), s("hi"));
        assert_eq!(call("upper", &[s("aB")], sp()).unwrap(), s("AB"));
        assert_eq!(call("lower", &[s("aB")], sp()).unwrap(), s("ab"));
    }

    #[test]
    fn find_replace_format_pad_repeat() {
        assert_eq!(call("find", &[s("hello"), s("ll")], sp()).unwrap(), Value::Number(2.0));
        assert_eq!(call("find", &[s("hello"), s("z")], sp()).unwrap(), Value::Number(-1.0));
        assert_eq!(call("replace", &[s("a.b.c"), s("."), s("-")], sp()).unwrap(), s("a-b-c"));
        assert_eq!(call("format", &[s("{} + {} = {}"), Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)], sp()).unwrap(), s("1 + 2 = 3"));
        assert_eq!(call("format", &[s("{{literal}}")], sp()).unwrap(), s("{literal}"));
        assert_eq!(call("padStart", &[s("7"), Value::Number(3.0), s("0")], sp()).unwrap(), s("007"));
        assert_eq!(call("padEnd", &[s("7"), Value::Number(3.0)], sp()).unwrap(), s("7  "));
        assert_eq!(call("repeat", &[s("ab"), Value::Number(3.0)], sp()).unwrap(), s("ababab"));
    }

    #[test]
    fn misuse_panics() {
        assert!(matches!(call("split", &[Value::Number(1.0), s(",")], sp()), Err(Control::Panic(_))));
        assert!(matches!(call("format", &[s("{}")], sp()), Err(Control::Panic(_))));
    }
}
```

- [ ] **Step 2: Run** `cargo test --lib stdlib::string` → PASS. Then `cargo test` + `cargo clippy --all-targets`. **Commit:**
```bash
git add -A && git commit -m "feat: std/string module

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: `std/array` (spec §11.2, with async callbacks)

**Files:** Create `src/stdlib/array.rs`.

API: `map(arr, fn)`, `filter(arr, fn)`, `reduce(arr, fn, init)`, `push(arr, item)` → new length (mutates), `pop(arr)` → element or `nil` (mutates), `slice(arr, start, end?)` → new array, `sort(arr, cmp?)` → new sorted array (non-destructive), `contains(arr, val)` → bool, `get(arr, i)` → element or `nil` (the checked accessor, §6). Callbacks: `map`/`filter` call `fn(elem, index)`; `reduce` calls `fn(acc, elem, index)`; `sort`'s comparator is `cmp(a, b)` → number (`<0`/`0`/`>0`).

- [ ] **Step 1: Create `src/stdlib/array.rs`:**
```rust
//! `std/array` — array operations. Callback-taking functions (`map`, `filter`,
//! `reduce`, `sort`) live on `impl Interp` because they invoke user functions.

use super::{arg, bi, clamp_index, want_array, want_number};
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("map", bi("array.map")),
        ("filter", bi("array.filter")),
        ("reduce", bi("array.reduce")),
        ("push", bi("array.push")),
        ("pop", bi("array.pop")),
        ("slice", bi("array.slice")),
        ("sort", bi("array.sort")),
        ("contains", bi("array.contains")),
        ("get", bi("array.get")),
    ]
}

impl Interp {
    pub(crate) async fn call_array(&mut self, func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        let ctx = |f: &str| format!("array.{}", f);
        match func {
            "map" => {
                let arr = want_array(&arg(args, 0), span, &ctx("map"))?;
                let f = arg(args, 1);
                let items = arr.borrow().clone();
                let mut out = Vec::with_capacity(items.len());
                for (i, item) in items.into_iter().enumerate() {
                    let v = self.call_value(f.clone(), vec![item, Value::Number(i as f64)], span).await?;
                    out.push(v);
                }
                Ok(Value::Array(Rc::new(RefCell::new(out))))
            }
            "filter" => {
                let arr = want_array(&arg(args, 0), span, &ctx("filter"))?;
                let f = arg(args, 1);
                let items = arr.borrow().clone();
                let mut out = Vec::new();
                for (i, item) in items.into_iter().enumerate() {
                    let keep = self.call_value(f.clone(), vec![item.clone(), Value::Number(i as f64)], span).await?;
                    if keep.is_truthy() {
                        out.push(item);
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(out))))
            }
            "reduce" => {
                let arr = want_array(&arg(args, 0), span, &ctx("reduce"))?;
                let f = arg(args, 1);
                let mut acc = arg(args, 2);
                let items = arr.borrow().clone();
                for (i, item) in items.into_iter().enumerate() {
                    acc = self.call_value(f.clone(), vec![acc, item, Value::Number(i as f64)], span).await?;
                }
                Ok(acc)
            }
            "push" => {
                let arr = want_array(&arg(args, 0), span, &ctx("push"))?;
                let item = arg(args, 1);
                let mut b = arr.borrow_mut();
                b.push(item);
                Ok(Value::Number(b.len() as f64))
            }
            "pop" => {
                let arr = want_array(&arg(args, 0), span, &ctx("pop"))?;
                Ok(arr.borrow_mut().pop().unwrap_or(Value::Nil))
            }
            "slice" => {
                let arr = want_array(&arg(args, 0), span, &ctx("slice"))?;
                let b = arr.borrow();
                let len = b.len();
                let start = clamp_index(want_number(&arg(args, 1), span, &ctx("slice"))?, len);
                let end = match args.get(2) {
                    None | Some(Value::Nil) => len,
                    Some(v) => clamp_index(want_number(v, span, &ctx("slice"))?, len),
                };
                let out: Vec<Value> = if start < end { b[start..end].to_vec() } else { Vec::new() };
                Ok(Value::Array(Rc::new(RefCell::new(out))))
            }
            "contains" => {
                let arr = want_array(&arg(args, 0), span, &ctx("contains"))?;
                let needle = arg(args, 1);
                let found = arr.borrow().iter().any(|v| *v == needle);
                Ok(Value::Bool(found))
            }
            "get" => {
                let arr = want_array(&arg(args, 0), span, &ctx("get"))?;
                let i = want_number(&arg(args, 1), span, &ctx("get"))?;
                if i < 0.0 || i.fract() != 0.0 {
                    return Ok(Value::Nil);
                }
                Ok(arr.borrow().get(i as usize).cloned().unwrap_or(Value::Nil))
            }
            "sort" => {
                let arr = want_array(&arg(args, 0), span, &ctx("sort"))?;
                let mut items = arr.borrow().clone();
                let cmp = args.get(1).cloned();
                match cmp {
                    Some(Value::Nil) | None => {
                        // Default ordering: numbers ascending, strings lexicographic.
                        // Mixed/other kinds → panic (no defined ordering).
                        sort_default(&mut items, span)?;
                    }
                    Some(f) => {
                        // Insertion sort driven by the async comparator (n is small in
                        // practice; keeps the await-in-comparator simple and stable).
                        let mut sorted: Vec<Value> = Vec::with_capacity(items.len());
                        for item in items.into_iter() {
                            let mut lo = 0usize;
                            while lo < sorted.len() {
                                let r = self.call_value(f.clone(), vec![item.clone(), sorted[lo].clone()], span).await?;
                                let n = match r {
                                    Value::Number(n) => n,
                                    other => return Err(AsError::at(
                                        format!("array.sort comparator must return a number, got {}", crate::interp::type_name(&other)),
                                        span,
                                    ).into()),
                                };
                                if n < 0.0 { break; }
                                lo += 1;
                            }
                            sorted.insert(lo, item);
                        }
                        items = sorted;
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(items))))
            }
            _ => Err(AsError::at(format!("std/array has no function '{}'", func), span).into()),
        }
    }
}

/// Sort by the natural order of a homogeneous number or string array.
fn sort_default(items: &mut [Value], span: Span) -> Result<(), Control> {
    let all_numbers = items.iter().all(|v| matches!(v, Value::Number(_)));
    let all_strings = items.iter().all(|v| matches!(v, Value::Str(_)));
    if all_numbers {
        items.sort_by(|a, b| match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
            _ => std::cmp::Ordering::Equal,
        });
        Ok(())
    } else if all_strings {
        items.sort_by(|a, b| match (a, b) {
            (Value::Str(x), Value::Str(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        });
        Ok(())
    } else {
        Err(AsError::at("array.sort without a comparator requires a homogeneous array of numbers or strings", span).into())
    }
}
```

- [ ] **Step 2: Write tests.** Because `call_array` needs `&mut Interp` + user functions, test through the interpreter in `src/interp.rs` tests:
```rust
    #[tokio::test]
    async fn std_array_map_filter_reduce() {
        let src = "import * as array from \"std/array\"\n\
                   let xs = [1, 2, 3, 4]\n\
                   print(array.map(xs, (x) => x * 2))\n\
                   print(array.filter(xs, (x) => x % 2 == 0))\n\
                   print(array.reduce(xs, (a, x) => a + x, 0))";
        assert_eq!(run(src).await, "[2, 4, 6, 8]\n[2, 4]\n10\n");
    }

    #[tokio::test]
    async fn std_array_mutation_and_access() {
        let src = "import * as array from \"std/array\"\n\
                   let xs = [1, 2]\n\
                   print(array.push(xs, 3))\n\
                   print(xs)\n\
                   print(array.pop(xs))\n\
                   print(array.get(xs, 0))\n\
                   print(array.get(xs, 9))\n\
                   print(array.contains(xs, 2))\n\
                   print(array.slice([10,20,30,40], 1, 3))";
        assert_eq!(run(src).await, "3\n[1, 2, 3]\n3\n1\nnil\ntrue\n[20, 30]\n");
    }

    #[tokio::test]
    async fn std_array_sort_default_and_comparator() {
        let src = "import * as array from \"std/array\"\n\
                   print(array.sort([3, 1, 2]))\n\
                   print(array.sort([\"b\", \"a\", \"c\"]))\n\
                   print(array.sort([3, 1, 2], (a, b) => b - a))";
        assert_eq!(run(src).await, "[1, 2, 3]\n[\"a\", \"b\", \"c\"]\n[3, 2, 1]\n");
    }
```

- [ ] **Step 3: Run** `cargo test --lib std_array` → PASS. `cargo test` + `cargo clippy --all-targets`. **Commit:**
```bash
git add -A && git commit -m "feat: std/array module (map/filter/reduce/sort with callbacks)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: `std/object` (spec §11.2)

**Files:** Create `src/stdlib/object.rs`.

API: `keys(obj)` → array of string keys (insertion order), `values(obj)` → array, `entries(obj)` → array of `[key, value]`, `has(obj, key)` → bool, `delete(obj, key)` → bool (whether it existed; mutates), `merge(a, b, …)` → new object (later keys win; non-mutating).

- [ ] **Step 1: Create `src/stdlib/object.rs`:**
```rust
//! `std/object` — object (string-keyed map) operations.

use super::{arg, bi, want_object, want_string};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("keys", bi("object.keys")),
        ("values", bi("object.values")),
        ("entries", bi("object.entries")),
        ("has", bi("object.has")),
        ("delete", bi("object.delete")),
        ("merge", bi("object.merge")),
    ]
}

fn arr(v: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(v)))
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("object.{}", f);
    match func {
        "keys" => {
            let o = want_object(&arg(args, 0), span, &ctx("keys"))?;
            Ok(arr(o.borrow().keys().map(|k| Value::Str(k.as_str().into())).collect()))
        }
        "values" => {
            let o = want_object(&arg(args, 0), span, &ctx("values"))?;
            Ok(arr(o.borrow().values().cloned().collect()))
        }
        "entries" => {
            let o = want_object(&arg(args, 0), span, &ctx("entries"))?;
            Ok(arr(o
                .borrow()
                .iter()
                .map(|(k, v)| arr(vec![Value::Str(k.as_str().into()), v.clone()]))
                .collect()))
        }
        "has" => {
            let o = want_object(&arg(args, 0), span, &ctx("has"))?;
            let key = want_string(&arg(args, 1), span, &ctx("has"))?;
            Ok(Value::Bool(o.borrow().contains_key(key.as_ref())))
        }
        "delete" => {
            let o = want_object(&arg(args, 0), span, &ctx("delete"))?;
            let key = want_string(&arg(args, 1), span, &ctx("delete"))?;
            // shift_remove preserves the order of the remaining keys.
            Ok(Value::Bool(o.borrow_mut().shift_remove(key.as_ref()).is_some()))
        }
        "merge" => {
            let mut out: IndexMap<String, Value> = IndexMap::new();
            for (i, v) in args.iter().enumerate() {
                let o = want_object(v, span, &format!("object.merge (argument {})", i + 1))?;
                for (k, val) in o.borrow().iter() {
                    out.insert(k.clone(), val.clone());
                }
            }
            Ok(Value::Object(Rc::new(RefCell::new(out))))
        }
        _ => Err(AsError::at(format!("std/object has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn obj(pairs: &[(&str, Value)]) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in pairs { m.insert(k.to_string(), v.clone()); }
        Value::Object(Rc::new(RefCell::new(m)))
    }

    #[test]
    fn keys_values_entries() {
        let o = obj(&[("a", Value::Number(1.0)), ("b", Value::Number(2.0))]);
        assert_eq!(call("keys", &[o.clone()], sp()).unwrap().to_string(), "[\"a\", \"b\"]");
        assert_eq!(call("values", &[o.clone()], sp()).unwrap().to_string(), "[1, 2]");
        assert_eq!(call("entries", &[o], sp()).unwrap().to_string(), "[[\"a\", 1], [\"b\", 2]]");
    }

    #[test]
    fn has_delete_merge() {
        let o = obj(&[("a", Value::Number(1.0))]);
        assert_eq!(call("has", &[o.clone(), Value::Str("a".into())], sp()).unwrap(), Value::Bool(true));
        assert_eq!(call("has", &[o.clone(), Value::Str("z".into())], sp()).unwrap(), Value::Bool(false));
        assert_eq!(call("delete", &[o.clone(), Value::Str("a".into())], sp()).unwrap(), Value::Bool(true));
        assert_eq!(call("has", &[o, Value::Str("a".into())], sp()).unwrap(), Value::Bool(false));
        let merged = call("merge", &[
            obj(&[("a", Value::Number(1.0)), ("b", Value::Number(2.0))]),
            obj(&[("b", Value::Number(9.0)), ("c", Value::Number(3.0))]),
        ], sp()).unwrap();
        assert_eq!(merged.to_string(), "{a: 1, b: 9, c: 3}");
    }
}
```

- [ ] **Step 2: Run** `cargo test --lib stdlib::object` → PASS. `cargo test` + clippy. **Commit:**
```bash
git add -A && git commit -m "feat: std/object module

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: `Map` value kind + `map<K,V>` type + `std/map` (spec §4/§5/§11.2)

**Files:** Modify `src/value.rs`, `src/ast.rs`, `src/parser.rs`, `src/interp.rs`. Create `src/stdlib/map.rs`.

- [ ] **Step 1: `src/value.rs`** — add `MapKey` and the `Map` variant. After the imports, add:
```rust
/// A hashable map key. Maps key on `nil`/`bool`/`number`/`string` (spec §11.2);
/// other value kinds are not hashable and panic at insertion time.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Nil,
    Bool(bool),
    Num(u64), // canonicalized f64 bits (−0.0→+0.0, all NaNs→one)
    Str(Rc<str>),
}

impl MapKey {
    /// Convert a value to a key, or `None` if its kind is not hashable.
    pub fn from_value(v: &Value) -> Option<MapKey> {
        match v {
            Value::Nil => Some(MapKey::Nil),
            Value::Bool(b) => Some(MapKey::Bool(*b)),
            Value::Number(n) => {
                let canon = if *n == 0.0 {
                    0.0f64.to_bits()
                } else if n.is_nan() {
                    f64::NAN.to_bits()
                } else {
                    n.to_bits()
                };
                Some(MapKey::Num(canon))
            }
            Value::Str(s) => Some(MapKey::Str(s.clone())),
            _ => None,
        }
    }

    /// Recover the value form of a key (for `keys`/`entries`/display/contracts).
    pub fn to_value(&self) -> Value {
        match self {
            MapKey::Nil => Value::Nil,
            MapKey::Bool(b) => Value::Bool(*b),
            MapKey::Num(bits) => Value::Number(f64::from_bits(*bits)),
            MapKey::Str(s) => Value::Str(s.clone()),
        }
    }
}
```
Add the variant to `enum Value` (after `Object`):
```rust
    Map(Rc<RefCell<IndexMap<MapKey, Value>>>),
```

- [ ] **Step 2: `src/value.rs`** — add the five match arms (compiler will flag each non-exhaustive match; add to all):
  - `PartialEq::eq`: `(Value::Map(a), Value::Map(b)) => Rc::ptr_eq(a, b),`
  - `Debug::fmt`: `Value::Map(m) => write!(f, "Map(len {})", m.borrow().len()),`
  - `write_display` (Display): add a cycle-guarded arm modeled on `Object`:
```rust
            Value::Map(m) => {
                let ptr = Rc::as_ptr(m) as usize;
                if seen.contains(&ptr) {
                    return write!(f, "map {{...}}");
                }
                seen.push(ptr);
                write!(f, "map {{")?;
                for (i, (k, v)) in m.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    k.to_value().write_element(f, seen)?;
                    write!(f, ": ")?;
                    v.write_element(f, seen)?;
                }
                write!(f, "}}")?;
                seen.pop();
                Ok(())
            }
```
  - `is_truthy`: **no change** — `Map` is neither `Nil` nor `Bool(false)`, so the existing `!matches!(...)` makes it truthy automatically. (Add a one-line `// Map is truthy via the catch-all` comment if helpful; do not special-case.)

- [ ] **Step 3: Write a `value.rs` test:**
```rust
    #[test]
    fn maps_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert(MapKey::Str("a".into()), Value::Number(1.0));
        m.insert(MapKey::Num(0.0f64.to_bits()), Value::Str("zero".into()));
        let map = Value::Map(Rc::new(RefCell::new(m)));
        assert_eq!(map.to_string(), "map {\"a\": 1, 0: \"zero\"}");
        assert_eq!(map.clone(), map);
        assert!(map.is_truthy());
        assert_ne!(MapKey::from_value(&Value::Number(0.0)), None);
        assert_eq!(MapKey::from_value(&Value::Array(Rc::new(RefCell::new(vec![])))), None);
    }
```
Run: `cargo test --lib maps_display_and_compare_by_identity` → after Steps 1–2 compile, PASS.

- [ ] **Step 4: `src/ast.rs`** — add the type variant (after `Named`):
```rust
    Map(Box<Type>, Box<Type>),
```
And in `Type`'s `Display`, add:
```rust
            Type::Map(k, v) => write!(f, "map<{}, {}>", k, v),
```

- [ ] **Step 5: `src/parser.rs`** — replace the `"map"` error arm in `parse_type_atom`:
```rust
                "map" => {
                    self.eat(&Tok::Lt)?;
                    let k = self.parse_type()?;
                    self.eat(&Tok::Comma)?;
                    let v = self.parse_type()?;
                    self.eat(&Tok::Gt)?;
                    Ok(Type::Map(Box::new(k), Box::new(v)))
                }
```
Add a parser test:
```rust
    #[test]
    fn parses_map_type_annotation() {
        let toks = lex("let m: map<string, number> = empty").unwrap();
        let prog = parse(&toks).unwrap();
        match &prog[0] {
            Stmt::Let { ty: Some(t), .. } => assert_eq!(t.to_string(), "map<string, number>"),
            other => panic!("expected typed let, got {other:?}"),
        }
    }
```

- [ ] **Step 6: `src/interp.rs`** — add the `check_type` arm (in `check_type`, after `Named`):
```rust
        Type::Map(k, v) => match value {
            Value::Map(m) => m
                .borrow()
                .iter()
                .all(|(mk, val)| check_type(&mk.to_value(), k) && check_type(val, v)),
            _ => false,
        },
```
Add the `type_name` arm:
```rust
        Value::Map(_) => "map",
```
And now add the `Value::Map(m) => m.borrow().len(),` line back into the `len` builtin (deferred from Task 2).

- [ ] **Step 7: Create `src/stdlib/map.rs`** (spec §11.2: new, get, set, has, delete, keys, values, entries):
```rust
//! `std/map` — the `Map` collection (insertion-ordered, hashable keys).

use super::{arg, bi};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::{MapKey, Value};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("new", bi("map.new")),
        ("get", bi("map.get")),
        ("set", bi("map.set")),
        ("has", bi("map.has")),
        ("delete", bi("map.delete")),
        ("keys", bi("map.keys")),
        ("values", bi("map.values")),
        ("entries", bi("map.entries")),
    ]
}

fn want_map(v: &Value, span: Span, ctx: &str) -> Result<Rc<RefCell<IndexMap<MapKey, Value>>>, Control> {
    match v {
        Value::Map(m) => Ok(m.clone()),
        _ => Err(AsError::at(format!("{} expects a map, got {}", ctx, crate::interp::type_name(v)), span).into()),
    }
}

fn want_key(v: &Value, span: Span, ctx: &str) -> Result<MapKey, Control> {
    MapKey::from_value(v).ok_or_else(|| {
        AsError::at(
            format!("{}: map keys must be nil, bool, number, or string, got {}", ctx, crate::interp::type_name(v)),
            span,
        )
        .into()
    })
}

fn arr(v: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(v)))
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("map.{}", f);
    match func {
        "new" => {
            let m = Rc::new(RefCell::new(IndexMap::new()));
            // Optional: seed from an array of [k, v] entry pairs.
            if let Some(seed) = args.first() {
                if !matches!(seed, Value::Nil) {
                    let entries = match seed {
                        Value::Array(a) => a.borrow().clone(),
                        _ => return Err(AsError::at("map.new optional argument must be an array of [key, value] pairs", span).into()),
                    };
                    for e in entries {
                        match e {
                            Value::Array(pair) if pair.borrow().len() == 2 => {
                                let p = pair.borrow();
                                m.borrow_mut().insert(want_key(&p[0], span, "map.new")?, p[1].clone());
                            }
                            _ => return Err(AsError::at("map.new entries must be [key, value] pairs", span).into()),
                        }
                    }
                }
            }
            Ok(Value::Map(m))
        }
        "get" => {
            let m = want_map(&arg(args, 0), span, &ctx("get"))?;
            let k = want_key(&arg(args, 1), span, &ctx("get"))?;
            Ok(m.borrow().get(&k).cloned().unwrap_or(Value::Nil))
        }
        "set" => {
            let m = want_map(&arg(args, 0), span, &ctx("set"))?;
            let k = want_key(&arg(args, 1), span, &ctx("set"))?;
            let v = arg(args, 2);
            m.borrow_mut().insert(k, v);
            Ok(arg(args, 0)) // return the map (chainable)
        }
        "has" => {
            let m = want_map(&arg(args, 0), span, &ctx("has"))?;
            let k = want_key(&arg(args, 1), span, &ctx("has"))?;
            Ok(Value::Bool(m.borrow().contains_key(&k)))
        }
        "delete" => {
            let m = want_map(&arg(args, 0), span, &ctx("delete"))?;
            let k = want_key(&arg(args, 1), span, &ctx("delete"))?;
            Ok(Value::Bool(m.borrow_mut().shift_remove(&k).is_some()))
        }
        "keys" => {
            let m = want_map(&arg(args, 0), span, &ctx("keys"))?;
            Ok(arr(m.borrow().keys().map(|k| k.to_value()).collect()))
        }
        "values" => {
            let m = want_map(&arg(args, 0), span, &ctx("values"))?;
            Ok(arr(m.borrow().values().cloned().collect()))
        }
        "entries" => {
            let m = want_map(&arg(args, 0), span, &ctx("entries"))?;
            Ok(arr(m.borrow().iter().map(|(k, v)| arr(vec![k.to_value(), v.clone()])).collect()))
        }
        _ => Err(AsError::at(format!("std/map has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }

    #[test]
    fn new_set_get_has_delete() {
        let m = call("new", &[], sp()).unwrap();
        call("set", &[m.clone(), Value::Str("a".into()), Value::Number(1.0)], sp()).unwrap();
        call("set", &[m.clone(), Value::Number(2.0), Value::Str("two".into())], sp()).unwrap();
        assert_eq!(call("get", &[m.clone(), Value::Str("a".into())], sp()).unwrap(), Value::Number(1.0));
        assert_eq!(call("get", &[m.clone(), Value::Number(2.0)], sp()).unwrap(), Value::Str("two".into()));
        assert_eq!(call("get", &[m.clone(), Value::Str("z".into())], sp()).unwrap(), Value::Nil);
        assert_eq!(call("has", &[m.clone(), Value::Str("a".into())], sp()).unwrap(), Value::Bool(true));
        assert_eq!(call("delete", &[m.clone(), Value::Str("a".into())], sp()).unwrap(), Value::Bool(true));
        assert_eq!(call("has", &[m.clone(), Value::Str("a".into())], sp()).unwrap(), Value::Bool(false));
        assert_eq!(call("keys", &[m], sp()).unwrap().to_string(), "[2]");
    }

    #[test]
    fn non_hashable_key_panics() {
        let m = call("new", &[], sp()).unwrap();
        let bad = Value::Array(Rc::new(RefCell::new(vec![])));
        assert!(matches!(call("set", &[m, bad, Value::Number(1.0)], sp()), Err(Control::Panic(_))));
    }
}
```

- [ ] **Step 8: Write the interp-level map test** in `src/interp.rs` tests:
```rust
    #[tokio::test]
    async fn std_map_end_to_end() {
        let src = "import * as map from \"std/map\"\n\
                   let m = map.new()\n\
                   map.set(m, \"x\", 10)\n\
                   map.set(m, \"y\", 20)\n\
                   print(map.get(m, \"x\"))\n\
                   print(len(m))\n\
                   print(map.keys(m))\n\
                   print(map.values(m))";
        assert_eq!(run(src).await, "10\n2\n[\"x\", \"y\"]\n[10, 20]\n");
    }

    #[tokio::test]
    async fn map_type_contract_enforced() {
        // a map<string, number> value passes; a wrong value type panics
        let ok = run("import * as map from \"std/map\"\nlet m: map<string, number> = map.new()\nmap.set(m, \"a\", 1)\nprint(len(m))").await;
        assert_eq!(ok, "1\n");
        let err = run_err("let m: map<string, number> = 5").await;
        assert!(err.message.contains("type contract violated"));
    }
```

- [ ] **Step 9: Run** `cargo test` + `cargo clippy --all-targets`. Green/clean. **Commit:**
```bash
git add -A && git commit -m "feat: Map value kind, map<K,V> type, and std/map module

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: `std/convert` (spec §11.2)

**Files:** Create `src/stdlib/convert.rs`.

API: `parseNumber(s)` → `Result<number>` (`[number, err]`), `parseInt(s, radix?)` → `Result<number>`, `toString(x)` → string, `toNumber(x)` → number (coerces: number→itself, bool→1/0, nil→0, string→parsed-or-panic), `toBool(x)` → bool (truthiness). `parseNumber`/`parseInt` are the only fallible ones (bad text is a value condition → Tier-1 Result, not a panic).

- [ ] **Step 1: Create `src/stdlib/convert.rs`:**
```rust
//! `std/convert` — parsing and coercions.

use super::{arg, bi, want_number, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("parseNumber", bi("convert.parseNumber")),
        ("parseInt", bi("convert.parseInt")),
        ("toString", bi("convert.toString")),
        ("toNumber", bi("convert.toNumber")),
        ("toBool", bi("convert.toBool")),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("convert.{}", f);
    match func {
        "parseNumber" => {
            let s = want_string(&arg(args, 0), span, &ctx("parseNumber"))?;
            match s.trim().parse::<f64>() {
                Ok(n) => Ok(make_pair(Value::Number(n), Value::Nil)),
                Err(_) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("cannot parse '{}' as a number", s).into())),
                )),
            }
        }
        "parseInt" => {
            let s = want_string(&arg(args, 0), span, &ctx("parseInt"))?;
            let radix = match args.get(1) {
                None | Some(Value::Nil) => 10u32,
                Some(v) => want_number(v, span, &ctx("parseInt"))? as u32,
            };
            if !(2..=36).contains(&radix) {
                return Err(AsError::at("convert.parseInt radix must be between 2 and 36", span).into());
            }
            match i64::from_str_radix(s.trim(), radix) {
                Ok(n) => Ok(make_pair(Value::Number(n as f64), Value::Nil)),
                Err(_) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("cannot parse '{}' as an integer (radix {})", s, radix).into())),
                )),
            }
        }
        "toString" => Ok(Value::Str(arg(args, 0).to_string().into())),
        "toNumber" => {
            let v = arg(args, 0);
            let n = match &v {
                Value::Number(n) => *n,
                Value::Bool(b) => if *b { 1.0 } else { 0.0 },
                Value::Nil => 0.0,
                Value::Str(s) => match s.trim().parse::<f64>() {
                    Ok(n) => n,
                    Err(_) => return Err(AsError::at(format!("convert.toNumber: cannot coerce '{}' to a number", s), span).into()),
                },
                _ => return Err(AsError::at(format!("convert.toNumber: cannot coerce {} to a number", crate::interp::type_name(&v)), span).into()),
            };
            Ok(Value::Number(n))
        }
        "toBool" => Ok(Value::Bool(arg(args, 0).is_truthy())),
        _ => Err(AsError::at(format!("std/convert has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }

    #[test]
    fn parse_number_ok_and_err() {
        let ok = call("parseNumber", &[s("3.5")], sp()).unwrap();
        assert_eq!(ok.to_string(), "[3.5, nil]");
        let err = call("parseNumber", &[s("abc")], sp()).unwrap();
        assert!(err.to_string().starts_with("[nil, {message:"));
    }

    #[test]
    fn parse_int_radix() {
        assert_eq!(call("parseInt", &[s("ff"), Value::Number(16.0)], sp()).unwrap().to_string(), "[255, nil]");
        assert_eq!(call("parseInt", &[s("101"), Value::Number(2.0)], sp()).unwrap().to_string(), "[5, nil]");
    }

    #[test]
    fn coercions() {
        assert_eq!(call("toString", &[Value::Number(7.0)], sp()).unwrap(), s("7"));
        assert_eq!(call("toNumber", &[Value::Bool(true)], sp()).unwrap(), Value::Number(1.0));
        assert_eq!(call("toNumber", &[s(" 42 ")], sp()).unwrap(), Value::Number(42.0));
        assert_eq!(call("toBool", &[Value::Number(0.0)], sp()).unwrap(), Value::Bool(true));
        assert_eq!(call("toBool", &[Value::Nil], sp()).unwrap(), Value::Bool(false));
    }
}
```

- [ ] **Step 2: Run** `cargo test --lib stdlib::convert` → PASS. `cargo test` + clippy. **Commit:**
```bash
git add -A && git commit -m "feat: std/convert module

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: End-to-end example + integration test + holistic verification

**Files:** Create `examples/stdlib.as`. Modify `tests/cli.rs`.

- [ ] **Step 1: Create `examples/stdlib.as`** — exercises all six modules, destructuring, the Map kind, the map type, and the `?` operator over a stdlib Result:
```
import * as string from "std/string"
import * as array from "std/array"
import * as object from "std/object"
import * as map from "std/map"
import * as math from "std/math"
import * as convert from "std/convert"

// string + array + core
let words = string.split("the quick brown fox", " ")
print(len(words))
let lengths = array.map(words, (w) => len(w))
print(lengths)
print(array.reduce(lengths, (a, n) => a + n, 0))
print(string.join(array.sort(words), ", "))

// math + range
let squares = array.map(range(1, 5), (n) => math.pow(n, 2))
print(squares)
print(math.max(3, 9, 2))

// object
let person = { name: "Ada", age: 36 }
print(object.keys(person))
print(object.has(person, "age"))

// map<K,V> type + std/map
let scores: map<string, number> = map.new()
map.set(scores, "ada", 100)
map.set(scores, "alan", 95)
print(map.get(scores, "ada"))
print(len(scores))

// convert + destructuring of a Tier-1 Result
let [n, err] = convert.parseNumber("42")
if (err == nil) {
  print(n + 8)
}
let [bad, e2] = convert.parseNumber("xyz")
print(e2.message)
```

- [ ] **Step 2: Run it and capture output.** `cargo run --quiet -- run examples/stdlib.as`. Confirm it produces (verify exact numbers before writing the assertion):
```
4
[3, 5, 5, 3]
16
brown, fox, quick, the
[1, 4, 9, 16]
9
["name", "age"]
true
100
2
50
cannot parse 'xyz' as a number
```

- [ ] **Step 3: Add the integration test** to `tests/cli.rs` (mirror the existing `runs_*_example` tests' structure):
```rust
#[test]
fn runs_stdlib_example() {
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg("examples/stdlib.as").output().unwrap();
    assert!(output.status.success(), "process failed: {:?}", output);
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("brown, fox, quick, the"));
    assert!(out.contains("[1, 4, 9, 16]"));
    assert!(out.contains("\"name\", \"age\"")); // object.keys
    assert!(out.contains("cannot parse 'xyz' as a number")); // Result destructuring
    assert!(out.contains("\n50\n")); // 42 + 8 after parseNumber + destructure
}
```

- [ ] **Step 4: Run the full suite** `cargo test` (lib + cli + modules + **treesitter conformance** — the new `examples/stdlib.as` must parse cleanly under BOTH the tree-sitter grammar and the interpreter parser, which validates the destructuring + `map<K,V>` surface). Then `cargo clippy --all-targets`. Both green/clean.

- [ ] **Step 5: fmt support check for the new surface.** Do NOT run `fmt` in-place on the committed `examples/stdlib.as` (fmt drops comments — a known M9 limitation — and would mutate the example). Instead, verify fmt handles the two new syntaxes by copying a snippet to a temp file and formatting it twice:
```bash
printf 'let [n, err] = parseNumber("42")\nlet m: map<string, number> = empty\n' > /tmp/m10_fmt.as
cargo run --quiet -- fmt /tmp/m10_fmt.as && cat /tmp/m10_fmt.as
cargo run --quiet -- fmt /tmp/m10_fmt.as && cat /tmp/m10_fmt.as
```
Expected: the destructuring `let [n, err] = …` and the `map<string, number>` annotation are preserved and stable across both runs (this exercises the `Stmt::LetDestructure` and `Type::Map` formatting added in Tasks 1 and 7).

- [ ] **Step 6: Commit:**
```bash
git add -A && git commit -m "test: end-to-end std collections example + integration test

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Definition of Done

- `cargo test` passes (all lib unit tests incl. `stdlib::*`, cli integration, modules, and the two tree-sitter conformance tests); `cargo clippy --all-targets` clean.
- `cargo run -- run examples/stdlib.as` imports and uses all six modules, destructures a `[value, err]` Result, constructs and queries a `Map`, and enforces a `map<string, number>` contract.
- Implemented per spec §11.1/§11.2: core globals `len`/`type`/`range`; `std/string`, `std/array`, `std/object`, `std/map`, `std/math`, `std/convert`; the `Map` value kind; the `map<K,V>` contract type; array destructuring `let [a, b] = expr`.
- Native functions are ordinary `function` values; argument-type misuse panics (Tier-2); only `parseNumber`/`parseInt` are Tier-1 Results.
- Nothing in M10 scope is deferred.

## Hand-off to Milestone 11 ("Serialization & encoding")

M11 adds `std/json`, `std/regex`, `std/encoding`, `std/bytes`, `std/uuid`, `std/csv`, `std/toml`, `std/yaml`. The seams are now in place: add submodules under `src/stdlib/`, register them in `std_module_exports` + `call_stdlib`, and follow the export-list + `call` pattern. `std/json` round-trips AScript `Value`s (object/array/number/string/bool/nil) — note `Map` has no JSON form, so JSON objects parse to `Value::Object`. `std/bytes` likely introduces a bytes representation (decide: `Value::Array` of byte-numbers vs a dedicated kind — a dedicated `Value::Bytes(Rc<RefCell<Vec<u8>>>)` is cleaner and is also needed by `std/process`/`std/net/http` in M13/M14; consider introducing it in M11). New crates land here (`serde_json`, `regex`, `base64`, `hex`, `uuid`, `csv`, `toml`, `serde_yaml`), gated under a `data` Cargo feature per spec §12.4.
```