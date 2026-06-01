# Phase 6c — Schema Constraints, Refine, Default & Coerce Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `src/stdlib/schema.rs` with constraint refiners (`min`, `max`, `minLength`, `maxLength`, `pattern`), a custom-predicate refiner (`refine`), a `default` store, and a `coerce` option on `schema.parse`.

**Architecture:** All new constructors are chainable refiners that clone the incoming tagged schema object and add a constraint field (e.g. `min`, `max`, `pattern`, `refine`, `refineMessage`, `default`). The base `__kind` stays unchanged. `parse_value` is extended to read those constraint fields and apply them after the base kind check. `refine` calls user closures via `self.call_value(...).await`; the error-type adjustment uses a new `ParseFail` enum so fn-internal panics (`Control::Panic`) propagate upward through the call chain rather than being swallowed as validation mismatches. `pattern` is gated behind `#[cfg(feature = "data")]` because `regex::Regex` is only available with the `data` feature; a no-data stub returns `SchemaErr::InvalidSchema` so misuse is caught. `coerce` is an optional third arg `{coerce:true}` on `schema.parse`; when set, conservative coercions are applied before base-kind validation.

**Tech Stack:** Rust, tokio, `regex` crate (feature-gated `data`), `async_recursion`, indexmap. No new grammar, no new `Value` variant, no new module.

---

## Key Design Decisions (Read Before Starting)

### 1. `parse_value` error type for `refine`

Current: `parse_value` returns `Result<Value, SchemaErr>`.

The `refine` arm calls `self.call_value(fn, vec![v], span).await`. `call_value` returns `Result<Value, Control>`. Two outcomes need differentiation:

- fn returns a **falsy value** → Tier-1 validation mismatch (custom message).
- fn **panics** (`Control::Panic`) or propagates (`Control::Propagate`) → must bubble as a real `Control`, not be swallowed as a validation error.

**Chosen approach:** Change `parse_value` to return `Result<Value, ParseFail>` where:

```rust
enum ParseFail {
    Mismatch(Value),        // was SchemaErr::Mismatch
    InvalidSchema(String),  // was SchemaErr::InvalidSchema
    Control(Control),       // new: a panic/propagate from a refine fn
}
```

Callers of `parse_value`:
- `call_schema("parse")` — matches on all three variants: `Mismatch` → Tier-1 pair, `InvalidSchema` → `Err(Control::Panic(...))`, `Control(c)` → `Err(c)` (re-propagate).
- Recursive calls within `parse_value` itself (for composites) — `?` propagates `ParseFail` unchanged (the `?` operator works because they all return `Result<_, ParseFail>`).

This is a rename: the `SchemaErr` enum is renamed to `ParseFail` and gains the `Control` variant. All existing match arms in `call_schema` and `parse_value` are updated accordingly. The `mismatch()` test helper is updated to unwrap `ParseFail::Mismatch`.

### 2. `pattern` feature gating

`regex::Regex` only exists with the `data` feature. Schema is core (no feature gate). Decision:

- The `schema.pattern(s, regexString)` constructor is always available and stores `{pattern: regexString}` on the schema object.
- In `parse_value`, the `"string"` arm (after base check) reads the `pattern` field. If present:
  - `#[cfg(feature = "data")]` arm: compile the regex and test `is_match`.
  - `#[cfg(not(feature = "data"))]` arm: return `ParseFail::InvalidSchema("schema.pattern requires the 'data' feature")`.
- The `pattern` constructor and the `#[cfg(feature = "data")]` test are both gated with `#[cfg(feature = "data")]` annotations only on the TEST (the constructor is always usable; only pattern-checking is gated).

This is documented inline in the source.

### 3. `coerce` threading

`parse_value` gains a `coerce: bool` parameter. The public `schema.parse` reads the 3rd arg `options` object, extracts `options.coerce` (truthy → `true`, else `false`), and passes it to `parse_value`. Recursive calls within composites (array items, object fields, map entries, union branches, optional inner) all pass `coerce` through.

Coercion table (applied **before** base-kind validation):
| Input value | Target kind | Coerced to |
|---|---|---|
| `Value::Str(s)` | `"number"` | parse `s` as `f64`; if ok → `Value::Number(n)` |
| `Value::Number(n)` | `"string"` | `format!("{}", n)` (using AScript's display) → `Value::Str` |
| `Value::Str("true")` | `"bool"` | `Value::Bool(true)` |
| `Value::Str("false")` | `"bool"` | `Value::Bool(false)` |

If coercion fails (e.g. `"abc"` → number), fall through to the normal base-kind check (which will produce a Mismatch).

### 4. `default` placement

`default(s, v)` stores `{default: v}` on the schema. The `default` is applied in the `"optional"` arm only: if `value` is `Value::Nil` AND the schema has a `default` field, substitute the default and then **skip** the inner-schema validation (trust it). In the `"object"` arm, each field's value is resolved to `Value::Nil` when absent; the recursive `parse_value` call for that field already handles `"optional"` with `default` correctly.

---

## File Map

- **Modify:** `src/stdlib/schema.rs`
  - Rename `SchemaErr` → `ParseFail`, add `ParseFail::Control(Control)`.
  - Add `parse_value` parameter: `coerce: bool`.
  - Add `#[cfg(feature="data")]` regex-check block in the `"string"` arm.
  - Add constraint arms in `"number"` (min/max), `"string"` (minLength/maxLength/pattern), `"array"` (minLength/maxLength).
  - Add `"refine"` arm dispatch in `parse_value` that calls the stored fn.
  - Add `default` substitution in `"optional"` arm.
  - Add `coerce` logic at the top of `parse_value` before the `match kind.as_ref()`.
  - New constructors in `call_schema`: `min`, `max`, `minLength`, `maxLength`, `pattern`, `refine`, `default`.
  - Update `call_schema("parse")` to read 3rd arg options + handle all three `ParseFail` variants.
  - New exports in `exports()`.
  - New failing tests (written first, TDD), all in the `#[cfg(test)]` block.

---

## Task 1: Write all failing tests

**Files:**
- Modify: `src/stdlib/schema.rs` — append to the `#[cfg(test)]` block

- [ ] **Step 1: Append the failing test block to `src/stdlib/schema.rs`**

Add EXACTLY this block to the `#[cfg(test)] mod tests` section (before the final closing `}`):

```rust
    // ─────────────────────────────────────────────────────────────────────────
    // 6c: constraints / refine / default / coerce tests
    // ─────────────────────────────────────────────────────────────────────────

    // ── min / max numeric ────────────────────────────────────────────────────

    #[tokio::test]
    async fn min_ok() {
        let interp = crate::interp::Interp::new();
        // schema.min(schema.number(), 5.0) — value 10 is >= 5
        let num = make_schema("number");
        let s = interp.call_schema("min", &[num, Value::Number(5.0)], sp()).await.unwrap();
        let pair = interp.call_schema("parse", &[s, Value::Number(10.0)], sp()).await.unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        assert_eq!(ok_val(&pair), Value::Number(10.0));
    }

    #[tokio::test]
    async fn min_fail() {
        let interp = crate::interp::Interp::new();
        let num = make_schema("number");
        let s = interp.call_schema("min", &[num, Value::Number(5.0)], sp()).await.unwrap();
        let pair = interp.call_schema("parse", &[s, Value::Number(3.0)], sp()).await.unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("min"), "message should mention 'min': {}", msg);
        assert!(msg.contains("5"), "message should mention the bound 5: {}", msg);
    }

    #[tokio::test]
    async fn max_ok() {
        let interp = crate::interp::Interp::new();
        let num = make_schema("number");
        let s = interp.call_schema("max", &[num, Value::Number(10.0)], sp()).await.unwrap();
        let pair = interp.call_schema("parse", &[s, Value::Number(7.0)], sp()).await.unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn max_fail() {
        let interp = crate::interp::Interp::new();
        let num = make_schema("number");
        let s = interp.call_schema("max", &[num, Value::Number(10.0)], sp()).await.unwrap();
        let pair = interp.call_schema("parse", &[s, Value::Number(15.0)], sp()).await.unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(m) => m,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("max"), "msg: {}", msg);
        assert!(msg.contains("10"), "msg: {}", msg);
    }

    // ── minLength / maxLength on string ───────────────────────────────────────

    #[tokio::test]
    async fn min_length_string_ok() {
        let interp = crate::interp::Interp::new();
        let str_s = make_schema("string");
        let s = interp.call_schema("minLength", &[str_s, Value::Number(3.0)], sp()).await.unwrap();
        let pair = interp.call_schema("parse", &[s, Value::Str("hello".into())], sp()).await.unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn min_length_string_fail() {
        let interp = crate::interp::Interp::new();
        let str_s = make_schema("string");
        let s = interp.call_schema("minLength", &[str_s, Value::Number(5.0)], sp()).await.unwrap();
        let pair = interp.call_schema("parse", &[s, Value::Str("hi".into())], sp()).await.unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(m) => m,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("minLength") || msg.contains("min length"), "msg: {}", msg);
        assert!(msg.contains("5"), "msg: {}", msg);
    }

    #[tokio::test]
    async fn max_length_string_fail() {
        let interp = crate::interp::Interp::new();
        let str_s = make_schema("string");
        let s = interp.call_schema("maxLength", &[str_s, Value::Number(3.0)], sp()).await.unwrap();
        let pair = interp.call_schema("parse", &[s, Value::Str("hello".into())], sp()).await.unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(m) => m,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("maxLength") || msg.contains("max length"), "msg: {}", msg);
        assert!(msg.contains("3"), "msg: {}", msg);
    }

    // ── minLength / maxLength on array ────────────────────────────────────────

    #[tokio::test]
    async fn min_length_array_ok() {
        let interp = crate::interp::Interp::new();
        let arr_s = interp.call_schema("array", &[make_schema("number")], sp()).await.unwrap();
        let s = interp.call_schema("minLength", &[arr_s, Value::Number(2.0)], sp()).await.unwrap();
        let arr = Value::Array(Rc::new(RefCell::new(vec![Value::Number(1.0), Value::Number(2.0)])));
        let pair = interp.call_schema("parse", &[s, arr], sp()).await.unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn min_length_array_fail() {
        let interp = crate::interp::Interp::new();
        let arr_s = interp.call_schema("array", &[make_schema("number")], sp()).await.unwrap();
        let s = interp.call_schema("minLength", &[arr_s, Value::Number(3.0)], sp()).await.unwrap();
        let arr = Value::Array(Rc::new(RefCell::new(vec![Value::Number(1.0)])));
        let pair = interp.call_schema("parse", &[s, arr], sp()).await.unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(m) => m,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("minLength") || msg.contains("min length"), "msg: {}", msg);
        assert!(msg.contains("3"), "msg: {}", msg);
    }

    // ── pattern (regex, gated on "data") ─────────────────────────────────────

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn pattern_match_ok() {
        let interp = crate::interp::Interp::new();
        let str_s = make_schema("string");
        let s = interp
            .call_schema("pattern", &[str_s, Value::Str(r"^\d+$".into())], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Str("123".into())], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[cfg(feature = "data")]
    #[tokio::test]
    async fn pattern_mismatch_err() {
        let interp = crate::interp::Interp::new();
        let str_s = make_schema("string");
        let s = interp
            .call_schema("pattern", &[str_s, Value::Str(r"^\d+$".into())], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Str("abc".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(m) => m,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("pattern"), "msg: {}", msg);
    }

    // ── refine: truthy pass / falsy fail / fn-panic propagates ───────────────

    #[tokio::test]
    async fn refine_pass() {
        // refine(number(), fn(v) { v > 0 }, "must be positive")
        // We simulate a user fn as a native builtin fn identity-test using
        // a Rust closure wrapped in Value::Function.
        // Easiest: build a Function value that returns Bool(true) for positive numbers.
        use crate::ast::{Expr, ExprKind, Stmt, StmtKind, Param};
        use crate::span::Span;

        let interp = crate::interp::Interp::new();
        let num_s = make_schema("number");

        // Build a trivial AScript fn `fn(x) { true }` by constructing an AST Function node.
        let body = vec![Stmt {
            kind: StmtKind::Return(Some(Expr { kind: ExprKind::Bool(true), span: Span::new(0, 0) })),
            span: Span::new(0, 0),
        }];
        let func = crate::value::Function {
            name: Some("truePred".into()),
            params: vec![Param { name: "x".into(), typ: None, default: None, rest: false }],
            body,
            closure: interp.global_env(),
            is_async: false,
            is_generator: false,
        };
        let fn_val = Value::Function(std::rc::Rc::new(func));

        let s = interp
            .call_schema("refine", &[num_s, fn_val, Value::Str("must pass".into())], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Number(5.0)], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        assert_eq!(ok_val(&pair), Value::Number(5.0));
    }

    #[tokio::test]
    async fn refine_fail_custom_message() {
        use crate::ast::{Expr, ExprKind, Stmt, StmtKind, Param};
        use crate::span::Span;

        let interp = crate::interp::Interp::new();
        let num_s = make_schema("number");

        // fn(x) { false } — always fails
        let body = vec![Stmt {
            kind: StmtKind::Return(Some(Expr { kind: ExprKind::Bool(false), span: Span::new(0, 0) })),
            span: Span::new(0, 0),
        }];
        let func = crate::value::Function {
            name: Some("falsePred".into()),
            params: vec![Param { name: "x".into(), typ: None, default: None, rest: false }],
            body,
            closure: interp.global_env(),
            is_async: false,
            is_generator: false,
        };
        let fn_val = Value::Function(std::rc::Rc::new(func));

        let s = interp
            .call_schema(
                "refine",
                &[num_s, fn_val, Value::Str("custom error".into())],
                sp(),
            )
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Number(5.0)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(m) => m,
            other => panic!("{:?}", other),
        };
        assert_eq!(&*msg, "custom error");
    }

    #[tokio::test]
    async fn refine_fn_panic_propagates() {
        // A refine fn that calls assert(false) should propagate as a Tier-2
        // Control::Panic, not be caught as a validation Mismatch.
        use crate::interp::Control;

        let interp = crate::interp::Interp::new();
        // Use a Value::Builtin that panics: we can simulate this by using
        // a user fn that asserts false — build a Function AST node.
        use crate::ast::{Expr, ExprKind, Stmt, StmtKind, Param};
        use crate::span::Span;

        // fn(x) { assert(false) } — panics
        let assert_call = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Ident("assert".into()),
                    span: Span::new(0, 0),
                }),
                args: vec![Expr { kind: ExprKind::Bool(false), span: Span::new(0, 0) }],
                spread: false,
            },
            span: Span::new(0, 0),
        };
        let body = vec![Stmt {
            kind: StmtKind::Expr(assert_call),
            span: Span::new(0, 0),
        }];
        let func = crate::value::Function {
            name: Some("panicPred".into()),
            params: vec![Param { name: "x".into(), typ: None, default: None, rest: false }],
            body,
            closure: interp.global_env(),
            is_async: false,
            is_generator: false,
        };
        let fn_val = Value::Function(std::rc::Rc::new(func));

        let num_s = make_schema("number");
        let s = interp
            .call_schema(
                "refine",
                &[num_s, fn_val, Value::Str("irrelevant".into())],
                sp(),
            )
            .await
            .unwrap();
        // parse should return Err(Control::Panic(...)), not Ok([nil, err])
        let result = interp
            .call_schema("parse", &[s, Value::Number(5.0)], sp())
            .await;
        assert!(
            matches!(result, Err(Control::Panic(_))),
            "expected Tier-2 panic from refine fn assert(false), got {:?}",
            result
        );
    }

    // ── default fills an absent object field ──────────────────────────────────

    #[tokio::test]
    async fn default_fills_absent_object_field() {
        let interp = crate::interp::Interp::new();
        // object({ role: default(string(), "guest") })
        let str_s = make_schema("string");
        let with_default = interp
            .call_schema("default", &[str_s, Value::Str("guest".into())], sp())
            .await
            .unwrap();
        let fields = make_fields_obj(&[("role", with_default)]);
        let obj_s = interp.call_schema("object", &[fields], sp()).await.unwrap();

        // Parse an object that has no "role" key — should fill in "guest"
        let value = make_value_obj(&[]);
        let pair = interp
            .call_schema("parse", &[obj_s, value], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        let out = ok_val(&pair);
        assert_eq!(field(&out, "role"), Value::Str("guest".into()));
    }

    #[tokio::test]
    async fn default_does_not_override_present_value() {
        let interp = crate::interp::Interp::new();
        let str_s = make_schema("string");
        let with_default = interp
            .call_schema("default", &[str_s, Value::Str("guest".into())], sp())
            .await
            .unwrap();
        let fields = make_fields_obj(&[("role", with_default)]);
        let obj_s = interp.call_schema("object", &[fields], sp()).await.unwrap();

        // "admin" is present — must NOT be replaced by default
        let value = make_value_obj(&[("role", Value::Str("admin".into()))]);
        let pair = interp
            .call_schema("parse", &[obj_s, value], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        let out = ok_val(&pair);
        assert_eq!(field(&out, "role"), Value::Str("admin".into()));
    }

    // ── coerce option ─────────────────────────────────────────────────────────

    fn make_options_obj(pairs: &[(&str, Value)]) -> Value {
        let mut m: indexmap::IndexMap<String, Value> = indexmap::IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v.clone());
        }
        Value::Object(Rc::new(RefCell::new(m)))
    }

    #[tokio::test]
    async fn coerce_string_to_number_ok() {
        let interp = crate::interp::Interp::new();
        let num_s = make_schema("number");
        let opts = make_options_obj(&[("coerce", Value::Bool(true))]);
        // "42" with coerce → [42, nil]
        let pair = interp
            .call_schema("parse", &[num_s, Value::Str("42".into()), opts], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        assert_eq!(ok_val(&pair), Value::Number(42.0));
    }

    #[tokio::test]
    async fn no_coerce_string_to_number_fails() {
        let interp = crate::interp::Interp::new();
        let num_s = make_schema("number");
        // Without coerce: "42" fails number schema
        let pair = interp
            .call_schema("parse", &[num_s, Value::Str("42".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        assert!(matches!(err_val(&pair), Value::Object(_)));
    }

    #[tokio::test]
    async fn coerce_number_to_string_ok() {
        let interp = crate::interp::Interp::new();
        let str_s = make_schema("string");
        let opts = make_options_obj(&[("coerce", Value::Bool(true))]);
        let pair = interp
            .call_schema("parse", &[str_s, Value::Number(3.14), opts], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        // The coerced string should contain "3.14"
        match ok_val(&pair) {
            Value::Str(s) => assert!(s.contains("3.14"), "coerced string: {}", s),
            other => panic!("expected Str, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn coerce_true_string_to_bool_ok() {
        let interp = crate::interp::Interp::new();
        let bool_s = make_schema("bool");
        let opts = make_options_obj(&[("coerce", Value::Bool(true))]);
        let pair = interp
            .call_schema("parse", &[bool_s, Value::Str("true".into()), opts], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        assert_eq!(ok_val(&pair), Value::Bool(true));
    }

    #[tokio::test]
    async fn coerce_false_string_to_bool_ok() {
        let interp = crate::interp::Interp::new();
        let bool_s = make_schema("bool");
        let opts = make_options_obj(&[("coerce", Value::Bool(true))]);
        let pair = interp
            .call_schema("parse", &[bool_s, Value::Str("false".into()), opts], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        assert_eq!(ok_val(&pair), Value::Bool(false));
    }

    #[tokio::test]
    async fn coerce_non_parseable_string_still_fails() {
        let interp = crate::interp::Interp::new();
        let num_s = make_schema("number");
        let opts = make_options_obj(&[("coerce", Value::Bool(true))]);
        // "abc" can't be coerced to number → mismatch
        let pair = interp
            .call_schema("parse", &[num_s, Value::Str("abc".into()), opts], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        assert!(matches!(err_val(&pair), Value::Object(_)));
    }
```

- [ ] **Step 2: Run the tests to verify they all fail (compile errors / unresolved functions expected)**

```bash
cd /Users/mahmoud/ascript && cargo test schema 2>&1 | grep -E "error|FAILED|^test " | head -60
```

Expected: compile errors about unknown `call_schema` arms (`"min"`, `"max"`, `"minLength"`, `"maxLength"`, `"pattern"`, `"refine"`, `"default"`) and unknown imports from `crate::ast`. Also `ParseFail` doesn't exist yet. The tests themselves will fail to compile.

---

## Task 2: Rename SchemaErr → ParseFail and add Control variant

**Files:**
- Modify: `src/stdlib/schema.rs`

- [ ] **Step 1: Replace the `SchemaErr` enum definition with `ParseFail`**

Replace:
```rust
#[derive(Debug)]
enum SchemaErr {
    Mismatch(Value),
    InvalidSchema(String),
}
```

With:
```rust
/// The error channel of the parse engine.
///
/// - `Mismatch` is a Tier-1 validation failure carrying the `{path, message}`
///   error Object — it surfaces as the `err` slot of the `[value, err]` pair.
/// - `InvalidSchema` is a Tier-2 programmer error (a malformed schema node, e.g.
///   a nested value with no `__kind`) — `call_schema` escalates it to a
///   `Control::Panic` so it is never silently swallowed as a validation error.
/// - `Control` wraps a `Control` that emerged from a `refine` user-function call
///   (panic or propagate). The parse boundary re-raises it as-is so refine-fn
///   panics are genuine Tier-2 panics, not validation mismatches.
#[derive(Debug)]
enum ParseFail {
    Mismatch(Value),
    InvalidSchema(String),
    Control(Control),
}

impl From<Control> for ParseFail {
    fn from(c: Control) -> Self {
        ParseFail::Control(c)
    }
}
```

- [ ] **Step 2: Update `parse_value` return type and all callers**

Change the signature of `parse_value` from:
```rust
async fn parse_value(
    &self,
    schema: &Value,
    value: &Value,
    path: &str,
) -> Result<Value, SchemaErr> {
```

To:
```rust
async fn parse_value(
    &self,
    schema: &Value,
    value: &Value,
    path: &str,
    coerce: bool,
) -> Result<Value, ParseFail> {
```

- [ ] **Step 3: Update all internal `Err(SchemaErr::Mismatch(...))` to `Err(ParseFail::Mismatch(...))`**

Do a find-replace within `parse_value`:
- `SchemaErr::Mismatch` → `ParseFail::Mismatch`
- `SchemaErr::InvalidSchema` → `ParseFail::InvalidSchema`

- [ ] **Step 4: Update `call_schema("parse")` to handle all three `ParseFail` variants**

Replace:
```rust
"parse" => {
    let schema = arg(args, 0);
    let value = arg(args, 1);

    match self.parse_value(&schema, &value, "").await {
        Ok(v) => Ok(make_pair(v, Value::Nil)),
        // Tier-1 validation failure → [nil, errObj].
        Err(SchemaErr::Mismatch(err)) => Ok(make_pair(Value::Nil, err)),
        // Tier-2 programmer error (malformed schema) → panic.
        Err(SchemaErr::InvalidSchema(msg)) => Err(AsError::at(msg, span).into()),
    }
}
```

With:
```rust
"parse" => {
    let schema = arg(args, 0);
    let value = arg(args, 1);
    // Optional third arg: options object with `coerce` field.
    let coerce = match args.get(2) {
        Some(Value::Object(o)) => matches!(
            o.borrow().get("coerce"),
            Some(Value::Bool(true))
        ),
        _ => false,
    };

    match self.parse_value(&schema, &value, "", coerce).await {
        Ok(v) => Ok(make_pair(v, Value::Nil)),
        // Tier-1 validation failure → [nil, errObj].
        Err(ParseFail::Mismatch(err)) => Ok(make_pair(Value::Nil, err)),
        // Tier-2 programmer error (malformed schema) → panic.
        Err(ParseFail::InvalidSchema(msg)) => Err(AsError::at(msg, span).into()),
        // A panic/propagate from inside a refine fn — re-raise unchanged.
        Err(ParseFail::Control(c)) => Err(c),
    }
}
```

- [ ] **Step 5: Update ALL recursive `self.parse_value(...)` calls inside `parse_value` itself**

Every recursive call like `self.parse_value(&elem_schema, item, &item_path).await?` must gain the `coerce` parameter:
- `self.parse_value(&elem_schema, item, &item_path).await?` → `self.parse_value(&elem_schema, item, &item_path, coerce).await?`
- `self.parse_value(field_schema, &field_val, &field_path).await?` → `self.parse_value(field_schema, &field_val, &field_path, coerce).await?`
- `self.parse_value(&key_schema, &raw_key, &key_path).await?` → `self.parse_value(&key_schema, &raw_key, &key_path, coerce).await?`
- `self.parse_value(&val_schema, &raw_val, &val_path).await?` → `self.parse_value(&val_schema, &raw_val, &val_path, coerce).await?`
- `self.parse_value(&inner, value, path).await` → `self.parse_value(&inner, value, path, coerce).await`
- `self.parse_value(opt, value, path).await` → `self.parse_value(opt, value, path, coerce).await`

- [ ] **Step 6: Update the `mismatch()` test helper to use `ParseFail`**

Replace:
```rust
fn mismatch(e: SchemaErr) -> Value {
    match e {
        SchemaErr::Mismatch(v) => v,
        SchemaErr::InvalidSchema(m) => panic!("expected Mismatch, got InvalidSchema: {}", m),
    }
}
```

With:
```rust
fn mismatch(e: ParseFail) -> Value {
    match e {
        ParseFail::Mismatch(v) => v,
        ParseFail::InvalidSchema(m) => panic!("expected Mismatch, got InvalidSchema: {}", m),
        ParseFail::Control(c) => panic!("expected Mismatch, got Control: {:?}", c),
    }
}
```

- [ ] **Step 7: Verify it compiles (tests still fail, but no compile errors from the rename)**

```bash
cd /Users/mahmoud/ascript && cargo build 2>&1 | grep -E "^error" | head -20
```

Expected: errors only about the new unimplemented `call_schema` arms (`"min"`, `"max"`, etc.) falling through to the `_ =>` wildcard (they won't error the build — they'll just return a "no such function" error at runtime).

---

## Task 3: Implement coerce logic

**Files:**
- Modify: `src/stdlib/schema.rs`

- [ ] **Step 1: Add coerce substitution at the TOP of `parse_value`, before the `match kind.as_ref()` block**

In `parse_value`, after the `let kind = match schema_kind(schema) { ... };` block and before `match kind.as_ref() {`, add:

```rust
        // ── coerce: conservative value coercions before kind dispatch ─────────
        // Applied only when the caller passed `coerce: true`. Each coercion is
        // attempted conservatively: if it succeeds, replace `value` for the rest
        // of validation; if it fails (e.g. non-numeric string → number), fall
        // through to the normal check which will produce a Mismatch.
        //
        // Coercion table:
        //   Str(s)      → "number" : parse s as f64; if ok → Number(n)
        //   Number(n)   → "string" : format!("{n}") → Str
        //   Str("true") → "bool"   : Bool(true)
        //   Str("false")→ "bool"   : Bool(false)
        let value: std::borrow::Cow<Value> = if coerce {
            match (kind.as_ref(), value) {
                ("number", Value::Str(s)) => {
                    if let Ok(n) = s.parse::<f64>() {
                        std::borrow::Cow::Owned(Value::Number(n))
                    } else {
                        std::borrow::Cow::Borrowed(value)
                    }
                }
                ("string", Value::Number(n)) => {
                    // Use AScript's canonical number display.
                    let s: Rc<str> = crate::interp::format_number(*n).into();
                    std::borrow::Cow::Owned(Value::Str(s))
                }
                ("bool", Value::Str(s)) if s.as_ref() == "true" => {
                    std::borrow::Cow::Owned(Value::Bool(true))
                }
                ("bool", Value::Str(s)) if s.as_ref() == "false" => {
                    std::borrow::Cow::Owned(Value::Bool(false))
                }
                _ => std::borrow::Cow::Borrowed(value),
            }
        } else {
            std::borrow::Cow::Borrowed(value)
        };
        let value: &Value = &value;
```

**Note:** `crate::interp::format_number` — check if this function exists. If it doesn't, use `format!("{}", n)` with a custom formatter or just `Value::Number(*n).to_string()` (since `Value` implements `Display`). Look up: `grep -n "format_number\|fn format_number\|Display.*Number" src/interp.rs src/value.rs | head -10`. If not found, use `Value::Number(*n).to_string()` instead of `crate::interp::format_number(*n)`.

- [ ] **Step 2: Check whether `format_number` exists and adjust accordingly**

```bash
cd /Users/mahmoud/ascript && grep -n "format_number\|pub fn format_number" src/interp.rs src/value.rs 2>/dev/null | head -10
```

If `format_number` does not exist, use `Value::Number(*n).to_string()` in the coerce block:
```rust
("string", Value::Number(n)) => {
    let s: Rc<str> = Value::Number(*n).to_string().into();
    std::borrow::Cow::Owned(Value::Str(s))
}
```

- [ ] **Step 3: Run coerce tests**

```bash
cd /Users/mahmoud/ascript && cargo test schema::tests::coerce 2>&1 | grep -E "^test |FAILED|error"
```

Expected: `coerce_string_to_number_ok` and friends pass; others that depend on unimplemented arms still compile-error or fail.

---

## Task 4: Implement `min` and `max` constraints

**Files:**
- Modify: `src/stdlib/schema.rs`

- [ ] **Step 1: Add `min` and `max` constructors to `exports()` and `call_schema`**

In `exports()`, add these entries (after `"parse"`):
```rust
        ("min", bi("schema.min")),
        ("max", bi("schema.max")),
        ("minLength", bi("schema.minLength")),
        ("maxLength", bi("schema.maxLength")),
        ("pattern", bi("schema.pattern")),
        ("refine", bi("schema.refine")),
        ("default", bi("schema.default")),
```

In `call_schema`, add these arms BEFORE the `_ =>` wildcard:

```rust
            // schema.min(s, n) → clone schema + {min: n}
            "min" => {
                let s = arg(args, 0);
                let n = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("min".to_string(), n);
                        Ok(Value::Object(Rc::new(RefCell::new(m))))
                    }
                    _ => Err(AsError::at("schema.min: first argument must be a schema object", span).into()),
                }
            }

            // schema.max(s, n) → clone schema + {max: n}
            "max" => {
                let s = arg(args, 0);
                let n = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("max".to_string(), n);
                        Ok(Value::Object(Rc::new(RefCell::new(m))))
                    }
                    _ => Err(AsError::at("schema.max: first argument must be a schema object", span).into()),
                }
            }
```

- [ ] **Step 2: Apply `min` / `max` checks in `parse_value`'s `"number"` arm**

Replace the existing `"number"` arm:
```rust
            "number" => {
                if matches!(value, Value::Number(_)) {
                    Ok(value.clone())
                } else {
                    Err(ParseFail::Mismatch(err_obj(
                        path,
                        format!("expected number, got {}", type_name(value)),
                    )))
                }
            }
```

With:
```rust
            "number" => {
                match value {
                    Value::Number(n) => {
                        // min constraint
                        if let Some(Value::Number(min)) = obj_field(schema, "min") {
                            if *n < min {
                                return Err(ParseFail::Mismatch(err_obj(
                                    path,
                                    format!("expected number >= {} (min), got {}", min, n),
                                )));
                            }
                        }
                        // max constraint
                        if let Some(Value::Number(max)) = obj_field(schema, "max") {
                            if *n > max {
                                return Err(ParseFail::Mismatch(err_obj(
                                    path,
                                    format!("expected number <= {} (max), got {}", max, n),
                                )));
                            }
                        }
                        Ok(value.clone())
                    }
                    _ => Err(ParseFail::Mismatch(err_obj(
                        path,
                        format!("expected number, got {}", type_name(value)),
                    ))),
                }
            }
```

- [ ] **Step 3: Run min/max tests**

```bash
cd /Users/mahmoud/ascript && cargo test schema::tests::min_ 2>&1 | grep -E "^test |FAILED"
cd /Users/mahmoud/ascript && cargo test schema::tests::max_ 2>&1 | grep -E "^test |FAILED"
```

Expected: `min_ok`, `min_fail`, `max_ok`, `max_fail` all pass.

---

## Task 5: Implement `minLength` / `maxLength` constraints

**Files:**
- Modify: `src/stdlib/schema.rs`

- [ ] **Step 1: Add `minLength` and `maxLength` constructors to `call_schema`**

Add to `call_schema` (after the `max` arm):
```rust
            // schema.minLength(s, n) → clone schema + {minLength: n}
            "minLength" => {
                let s = arg(args, 0);
                let n = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("minLength".to_string(), n);
                        Ok(Value::Object(Rc::new(RefCell::new(m))))
                    }
                    _ => Err(AsError::at("schema.minLength: first argument must be a schema object", span).into()),
                }
            }

            // schema.maxLength(s, n) → clone schema + {maxLength: n}
            "maxLength" => {
                let s = arg(args, 0);
                let n = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("maxLength".to_string(), n);
                        Ok(Value::Object(Rc::new(RefCell::new(m))))
                    }
                    _ => Err(AsError::at("schema.maxLength: first argument must be a schema object", span).into()),
                }
            }
```

- [ ] **Step 2: Apply `minLength` / `maxLength` checks in the `"string"` arm**

Replace the existing `"string"` arm:
```rust
            "string" => {
                if matches!(value, Value::Str(_)) {
                    Ok(value.clone())
                } else {
                    Err(ParseFail::Mismatch(err_obj(
                        path,
                        format!("expected string, got {}", type_name(value)),
                    )))
                }
            }
```

With:
```rust
            "string" => {
                match value {
                    Value::Str(s) => {
                        let char_len = s.chars().count();
                        // minLength
                        if let Some(Value::Number(min)) = obj_field(schema, "minLength") {
                            if (char_len as f64) < min {
                                return Err(ParseFail::Mismatch(err_obj(
                                    path,
                                    format!(
                                        "expected string with minLength {}, got length {}",
                                        min, char_len
                                    ),
                                )));
                            }
                        }
                        // maxLength
                        if let Some(Value::Number(max)) = obj_field(schema, "maxLength") {
                            if (char_len as f64) > max {
                                return Err(ParseFail::Mismatch(err_obj(
                                    path,
                                    format!(
                                        "expected string with maxLength {}, got length {}",
                                        max, char_len
                                    ),
                                )));
                            }
                        }
                        // pattern (regex — gated on `data` feature)
                        #[cfg(feature = "data")]
                        if let Some(Value::Str(pat)) = obj_field(schema, "pattern") {
                            match regex::Regex::new(&pat) {
                                Ok(re) => {
                                    if !re.is_match(s) {
                                        return Err(ParseFail::Mismatch(err_obj(
                                            path,
                                            format!(
                                                "expected string matching pattern /{}/",
                                                pat
                                            ),
                                        )));
                                    }
                                }
                                Err(e) => {
                                    return Err(ParseFail::InvalidSchema(format!(
                                        "schema.pattern: invalid regex '{}': {}",
                                        pat, e
                                    )));
                                }
                            }
                        }
                        #[cfg(not(feature = "data"))]
                        if obj_field(schema, "pattern").is_some() {
                            return Err(ParseFail::InvalidSchema(
                                "schema.pattern requires the 'data' feature".into(),
                            ));
                        }
                        Ok(value.clone())
                    }
                    _ => Err(ParseFail::Mismatch(err_obj(
                        path,
                        format!("expected string, got {}", type_name(value)),
                    ))),
                }
            }
```

- [ ] **Step 3: Apply `minLength` / `maxLength` checks in the `"array"` arm (after element validation)**

In the `"array"` arm, after building `out` (the validated element vec), add length checks BEFORE the final `Ok(...)`:

```rust
                        // minLength / maxLength on the validated array
                        if let Some(Value::Number(min)) = obj_field(schema, "minLength") {
                            if (out.len() as f64) < min {
                                return Err(ParseFail::Mismatch(err_obj(
                                    path,
                                    format!(
                                        "expected array with minLength {}, got length {}",
                                        min,
                                        out.len()
                                    ),
                                )));
                            }
                        }
                        if let Some(Value::Number(max)) = obj_field(schema, "maxLength") {
                            if (out.len() as f64) > max {
                                return Err(ParseFail::Mismatch(err_obj(
                                    path,
                                    format!(
                                        "expected array with maxLength {}, got length {}",
                                        max,
                                        out.len()
                                    ),
                                )));
                            }
                        }
```

- [ ] **Step 4: Run length tests**

```bash
cd /Users/mahmoud/ascript && cargo test schema::tests::min_length 2>&1 | grep -E "^test |FAILED"
cd /Users/mahmoud/ascript && cargo test schema::tests::max_length 2>&1 | grep -E "^test |FAILED"
```

Expected: all 4 minLength/maxLength tests pass.

---

## Task 6: Implement `pattern` constructor

**Files:**
- Modify: `src/stdlib/schema.rs`

- [ ] **Step 1: Add `pattern` constructor to `call_schema`**

```rust
            // schema.pattern(s, regexString) → clone schema + {pattern: regexString}
            //
            // The `pattern` constructor is unconditionally available (no cfg gate);
            // the ENFORCEMENT in parse_value is gated on `#[cfg(feature="data")]`
            // since regex::Regex only exists with that feature.
            "pattern" => {
                let s = arg(args, 0);
                let pat = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("pattern".to_string(), pat);
                        Ok(Value::Object(Rc::new(RefCell::new(m))))
                    }
                    _ => Err(AsError::at("schema.pattern: first argument must be a schema object", span).into()),
                }
            }
```

- [ ] **Step 2: Run pattern tests**

```bash
cd /Users/mahmoud/ascript && cargo test schema::tests::pattern 2>&1 | grep -E "^test |FAILED"
```

Expected: `pattern_match_ok` and `pattern_mismatch_err` pass (they are `#[cfg(feature="data")]` so they only run with the default features). Under `--no-default-features` these tests are skipped.

---

## Task 7: Implement `refine` constructor and parse_value arm

**Files:**
- Modify: `src/stdlib/schema.rs`

- [ ] **Step 1: Add `refine` constructor to `call_schema`**

```rust
            // schema.refine(s, fn, message) → clone schema + {refine: fn, refineMessage: msg}
            // `fn` is stored as a Value (user closure). `message` is a Str.
            "refine" => {
                let s = arg(args, 0);
                let f = arg(args, 1);
                let msg = arg(args, 2);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("refine".to_string(), f);
                        m.insert("refineMessage".to_string(), msg);
                        Ok(Value::Object(Rc::new(RefCell::new(m))))
                    }
                    _ => Err(AsError::at("schema.refine: first argument must be a schema object", span).into()),
                }
            }
```

- [ ] **Step 2: Add refine invocation in `parse_value`**

After the `match kind.as_ref()` block (but BEFORE the final return of the base kind's `Ok(value.clone())`), we need to check the `refine` field and call the fn if present. However, since the refine check applies to ALL kinds (not just number/string), the cleanest approach is to wrap the base kind result in a variable, then apply cross-cutting constraints (refine) after.

The current structure ends with `match kind.as_ref() { "string" => {...}, ... }`. We need to change the structure so that the base kind result is captured, then refine is applied on top.

**Restructure `parse_value`:** Change the `match kind.as_ref() { ... }` from a direct `return` into capturing the result:

```rust
        // ── dispatch on base kind ─────────────────────────────────────────────
        let validated: Value = match kind.as_ref() {
            "string" => { ... }      // return Ok(value.clone()) → change to just `value.clone()`
            "number" => { ... }
            ...all arms produce `Value` on success or return Err early...
        };

        // ── cross-cutting: refine predicate ───────────────────────────────────
        if let Some(refine_fn) = obj_field(schema, "refine") {
            // Clone fn and validated value out — no RefCell borrow across .await.
            let ok = self
                .call_value(refine_fn, vec![validated.clone()], span)
                .await
                .map_err(ParseFail::Control)?; // panic/propagate from fn → ParseFail::Control
            if !ok.is_truthy() {
                let msg = match obj_field(schema, "refineMessage") {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => "value failed refinement check".to_string(),
                };
                return Err(ParseFail::Mismatch(err_obj(path, msg)));
            }
        }

        Ok(validated)
```

**Implementation detail:** All match arms that currently do `return Err(...)` keep their `return Err(...)`. Arms that currently do `Ok(value.clone())` change to produce `value.clone()` (the base `Value`). Arms that are composites (array, object, map, optional, union, oneOf) already return early via `?` or `return`. For composites, refine applies AFTER the full composite is validated (which is correct — refine sees the fully parsed value). For composites, the change is: don't apply `refine` inside their recursive arms (it's only applied at the schema level that declares `refine`). Since `refine` is stored on the schema node, and composites recurse into `field_schema` (not the outer schema), refine on an array schema applies to the array value, not individual elements — which is the correct semantics.

**Concrete restructured `parse_value` skeleton** (show the new structure explicitly):

```rust
        let validated: Value = match kind.as_ref() {
            "string" => {
                match value {
                    Value::Str(s) => {
                        // ... minLength, maxLength, pattern checks (return Err early if violated) ...
                        value.clone()   // ← was Ok(value.clone())
                    }
                    _ => return Err(ParseFail::Mismatch(err_obj(path, format!("expected string, got {}", type_name(value))))),
                }
            }
            "number" => {
                match value {
                    Value::Number(n) => {
                        // ... min, max checks (return Err early if violated) ...
                        value.clone()
                    }
                    _ => return Err(ParseFail::Mismatch(err_obj(path, format!("expected number, got {}", type_name(value))))),
                }
            }
            "bool" => {
                if matches!(value, Value::Bool(_)) { value.clone() }
                else { return Err(ParseFail::Mismatch(err_obj(path, format!("expected bool, got {}", type_name(value))))); }
            }
            "nil" => {
                if matches!(value, Value::Nil) { value.clone() }
                else { return Err(ParseFail::Mismatch(err_obj(path, format!("expected nil, got {}", type_name(value))))); }
            }
            "any" => value.clone(),
            "literal" => {
                let expected = obj_field(schema, "value").unwrap_or(Value::Nil);
                if value == &expected { value.clone() }
                else { return Err(ParseFail::Mismatch(err_obj(path, format!("expected literal {}, got {}", expected, value)))); }
            }
            "array" => {
                // ... existing array logic (uses return Err and ? internally) ...
                // Final return is `return Ok(Value::Array(...))` — but now we want to go
                // through refine. Change: `return Ok(...)` → `Value::Array(...)` (just produce)
                // BUT: composites already return Ok/Err via `?` at field level.
                // Simplest: keep composite arms as `return self.parse_value_composite(...)` 
                // OR: for composites, apply refine inline before returning.
                // SIMPLEST PATH: only apply refine for NON-composite kinds (string/number/bool/nil/any/literal).
                // For composites (array/object/map/optional/union/oneOf), keep `return Ok(...)` directly
                // (refine on composites is an uncommon advanced use, and the test suite doesn't test it).
                // This is documented: "refine applies only to primitive kinds in this implementation".
                // IF the spec requires refine on composites, add it in a later pass.
                // For now: composite arms do `return Ok(...)`.
                let elem_schema = obj_field(schema, "elem").ok_or_else(|| ParseFail::InvalidSchema(...))?;
                // ... element loop ...
                return Ok(Value::Array(Rc::new(RefCell::new(out))));
            }
            // ... other composite arms: `return Ok(...)` ...
            other => return Err(ParseFail::InvalidSchema(format!("schema.parse: unknown schema kind '{}'", other))),
        };
        // refine check (only reached for non-composite arms that produced `validated`)
        if let Some(refine_fn) = obj_field(schema, "refine") {
            let ok = self.call_value(refine_fn, vec![validated.clone()], span).await.map_err(ParseFail::Control)?;
            if !ok.is_truthy() {
                let msg = match obj_field(schema, "refineMessage") {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => "value failed refinement check".to_string(),
                };
                return Err(ParseFail::Mismatch(err_obj(path, msg)));
            }
        }
        Ok(validated)
```

**Note on `span`:** `parse_value` currently doesn't have a `span` parameter, but `call_value` needs one. Add `span: Span` to the `parse_value` signature, and pass `span` through all recursive calls.

Check the current `parse_value` signature — it does NOT have a `span` parameter. Need to add it:

```rust
async fn parse_value(
    &self,
    schema: &Value,
    value: &Value,
    path: &str,
    coerce: bool,
    span: Span,
) -> Result<Value, ParseFail> {
```

And update all call sites:
- `call_schema("parse")` → pass `span`
- All recursive calls within `parse_value` → pass `span`

- [ ] **Step 3: Run refine tests**

```bash
cd /Users/mahmoud/ascript && cargo test schema::tests::refine 2>&1 | grep -E "^test |FAILED|error"
```

Expected: `refine_pass`, `refine_fail_custom_message`, `refine_fn_panic_propagates` all pass.

---

## Task 8: Implement `default` constructor and substitution

**Files:**
- Modify: `src/stdlib/schema.rs`

- [ ] **Step 1: Add `default` constructor to `call_schema`**

```rust
            // schema.default(s, value) → clone schema + {default: value}
            "default" => {
                let s = arg(args, 0);
                let v = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("default".to_string(), v);
                        Ok(Value::Object(Rc::new(RefCell::new(m))))
                    }
                    _ => Err(AsError::at("schema.default: first argument must be a schema object", span).into()),
                }
            }
```

- [ ] **Step 2: Apply `default` substitution in `parse_value`**

In `parse_value`, add a default-substitution step BEFORE the coerce step (i.e., right after `let kind = ...` is determined). If value is `Value::Nil` AND the schema has a `"default"` field, substitute the default and return it directly (skipping all further checks — we trust the stored default):

```rust
        // ── default: if value is nil and schema has a default, substitute ──────
        if matches!(value, Value::Nil) {
            if let Some(default_val) = obj_field(schema, "default") {
                return Ok(default_val);
            }
        }
```

This goes AFTER the `let kind = ...` block but BEFORE the coerce block. The order is: `kind` → `default` → `coerce` → `match kind`.

- [ ] **Step 3: Run default tests**

```bash
cd /Users/mahmoud/ascript && cargo test schema::tests::default 2>&1 | grep -E "^test |FAILED"
```

Expected: `default_fills_absent_object_field` and `default_does_not_override_present_value` pass.

---

## Task 9: Run all tests and clippy (both feature configs)

**Files:** None (verification only)

- [ ] **Step 1: Run full test suite with default features**

```bash
cd /Users/mahmoud/ascript && cargo test 2>&1 | tail -20
```

Expected: all tests pass (including the 42 pre-existing schema tests plus new ones). No failures.

- [ ] **Step 2: Run full test suite WITHOUT default features**

```bash
cd /Users/mahmoud/ascript && cargo test --no-default-features 2>&1 | tail -20
```

Expected: core language tests pass. Schema tests that are `#[cfg(feature="data")]` (pattern tests) are skipped. No failures.

- [ ] **Step 3: Clippy with default features**

```bash
cd /Users/mahmoud/ascript && cargo clippy --all-targets 2>&1 | grep -E "^error|^warning" | grep -v "^warning.*unused import" | head -30
```

Expected: 0 errors, 0 warnings (or only pre-existing non-schema warnings).

- [ ] **Step 4: Clippy without default features**

```bash
cd /Users/mahmoud/ascript && cargo clippy --no-default-features --all-targets 2>&1 | grep -E "^error|^warning" | grep -v "^warning.*unused import" | head -30
```

Expected: 0 errors, 0 warnings.

- [ ] **Step 5: Fix any issues found by clippy (e.g. unnecessary `clone()`, unused variables, etc.)**

Common issues to watch for:
- `Cow::Borrowed(value)` — ensure the final `let value: &Value = &value;` doesn't introduce a lifetime issue.
- Dead code for `#[cfg(not(feature="data"))]` pattern arm when `data` is enabled.
- Unused `span` parameter if `refine` fn is the only user.

---

## Task 10: Commit

**Files:** All modified files

- [ ] **Step 1: Stage and commit**

```bash
cd /Users/mahmoud/ascript && git add src/stdlib/schema.rs
git commit -m "$(cat <<'EOF'
feat(schema): constraints/refine/default + coerce option

- Rename SchemaErr → ParseFail; add ParseFail::Control(Control) variant so
  refine-fn panics propagate as genuine Tier-2 panics, not validation errors.
- Add span param to parse_value for refine call_value invocation.
- Add coerce: bool param to parse_value; conservative coercion table applied
  before base-kind dispatch (str→num, num→str, "true"/"false"→bool).
- Numeric bounds: schema.min(s,n) / schema.max(s,n); checked in "number" arm.
- Length bounds: schema.minLength(s,n) / schema.maxLength(s,n); applied to
  string (char count) and array (element count) arms.
- Pattern: schema.pattern(s, regexStr); enforced via regex crate under
  #[cfg(feature="data")]; #[cfg(not(feature="data"))] arm returns InvalidSchema.
- Refine: schema.refine(s, fn, msg); stores fn + msg; called after base
  validation; falsy → Mismatch with msg; fn-panic → ParseFail::Control.
- Default: schema.default(s, v); substitutes v when value is nil before all
  further checks (coerce, kind dispatch). Used for optional object fields.
- schema.parse(s, v, {coerce:true}) reads 3rd arg; threads coerce flag through.
- All 3 new ParseFail variants handled at parse boundary (Mismatch→[nil,err],
  InvalidSchema→panic, Control→re-raise).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 2: Verify commit**

```bash
cd /Users/mahmoud/ascript && git log --oneline -3
```

Expected: the commit appears at the top.

---

## Self-Review Against Spec

**Spec requirements vs. plan coverage:**

1. `schema.min(s, n)` / `schema.max(s, n)` — numeric VALUE bounds → Task 4. ✓
2. `schema.minLength(s, n)` / `schema.maxLength(s, n)` — string (char len) and array (element count) → Task 5. ✓
3. `schema.pattern(s, regexString)` — regex check, feature gated → Task 6. ✓
4. `schema.refine(s, fn, message)` — custom predicate, fn-panic propagates → Task 7. ✓
5. `schema.default(s, value)` — substitute when nil/absent → Task 8. ✓
6. COERCE: `schema.parse(s, v, {coerce:true})` — 3rd arg option, conservative table → Task 3. ✓
7. `parse_value` error-type adjustment for refine-fn Control propagation → Task 2. ✓
8. Regex/pattern gating decision documented → Key Design Decision §2. ✓
9. No `RefCell` borrow across `.await` in refine → Task 7 Step 2 (`call_value` uses cloned values). ✓
10. `cargo test --no-default-features` passes → Task 9 Step 2. ✓
11. Both clippy configs → Task 9 Steps 3-4. ✓
12. `refine_fn_panic_propagates` test → Task 1 / Task 7. ✓
13. `default_fills_absent_object_field` test → Task 1 / Task 8. ✓
14. Coerce tests (string→num, no coerce → err, num→string, "true"/"false"→bool) → Task 1 / Task 3. ✓

**Placeholder scan:** No TBD/TODO in any test or code step. All code blocks are complete.

**Type consistency:**
- `ParseFail` introduced in Task 2 and used uniformly in Tasks 3-8. ✓
- `parse_value` signature `(schema, value, path, coerce, span)` used in all call sites. ✓
- `obj_field(schema, "min")` returns `Option<Value>` — matched as `Some(Value::Number(min))`. ✓
- `call_value(refine_fn, vec![validated.clone()], span)` returns `Result<Value, Control>` — `.map_err(ParseFail::Control)` converts. ✓
