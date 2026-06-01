//! `std/assert` — rich assertion helpers for test code.
//!
//! Each assertion passes silently on success and raises a Tier-2 panic
//! (`Control::Panic`) with a descriptive, value-showing message on failure.
//!
//! All container comparisons (`eq`, `ne`, `contains`) use structural
//! deep equality via `object::deep_equal`, so `assert.eq([1,2],[1,2])` passes
//! even though the two arrays are distinct heap objects.
//!
//! `assert.throws(fn) -> errValue` is async: it calls `fn`, drives any
//! returned `Value::Future` to completion, then checks whether the call
//! raised a `Control::Panic`.  On success it returns the caught error value
//! (the `{ message }` object that `recover` would have returned).  If `fn`
//! completes without panicking, `assert.throws` itself panics.

use super::arg;
use crate::error::AsError;
use crate::interp::{make_error, Control, Interp};
use crate::span::Span;
use crate::stdlib::object::deep_equal;
use crate::value::Value;
use rust_decimal::prelude::ToPrimitive;

pub fn exports() -> Vec<(&'static str, Value)> {
    use super::bi;
    vec![
        ("eq", bi("assert.eq")),
        ("ne", bi("assert.ne")),
        ("isTrue", bi("assert.isTrue")),
        ("isFalse", bi("assert.isFalse")),
        ("isNil", bi("assert.isNil")),
        ("notNil", bi("assert.notNil")),
        ("gt", bi("assert.gt")),
        ("gte", bi("assert.gte")),
        ("lt", bi("assert.lt")),
        ("lte", bi("assert.lte")),
        ("contains", bi("assert.contains")),
        ("approxEq", bi("assert.approxEq")),
        ("throws", bi("assert.throws")),
    ]
}

/// Helper: format a panic error with an optional user message prefix.
fn fail(base: impl Into<String>, user_msg: Option<&str>, span: Span) -> Control {
    let msg = match user_msg {
        Some(m) if !m.is_empty() => format!("{}: {}", m, base.into()),
        _ => base.into(),
    };
    AsError::at(msg, span).into()
}

impl Interp {
    /// Dispatch for `assert.*` builtin calls.
    pub(crate) async fn call_assert(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            // ── assert.eq(a, b, msg?) ─────────────────────────────────────────
            "eq" => {
                let a = arg(args, 0);
                let b = arg(args, 1);
                let msg = opt_str(args, 2);
                if !deep_equal(&a, &b) {
                    return Err(fail(
                        format!("assert.eq failed: {} != {}", a, b),
                        msg.as_deref(),
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.ne(a, b, msg?) ─────────────────────────────────────────
            "ne" => {
                let a = arg(args, 0);
                let b = arg(args, 1);
                let msg = opt_str(args, 2);
                if deep_equal(&a, &b) {
                    return Err(fail(
                        format!("assert.ne failed: both equal {}", a),
                        msg.as_deref(),
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.isTrue(x) ──────────────────────────────────────────────
            "isTrue" => {
                let x = arg(args, 0);
                if !x.is_truthy() {
                    return Err(fail(
                        format!("assert.isTrue failed: {} is falsy", x),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.isFalse(x) ─────────────────────────────────────────────
            "isFalse" => {
                let x = arg(args, 0);
                if x.is_truthy() {
                    return Err(fail(
                        format!("assert.isFalse failed: {} is truthy", x),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.isNil(x) ───────────────────────────────────────────────
            "isNil" => {
                let x = arg(args, 0);
                if x != Value::Nil {
                    return Err(fail(
                        format!("assert.isNil failed: expected nil, got {}", x),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.notNil(x) ──────────────────────────────────────────────
            "notNil" => {
                let x = arg(args, 0);
                if x == Value::Nil {
                    return Err(fail("assert.notNil failed: got nil", None, span));
                }
                Ok(Value::Nil)
            }
            // ── assert.gt / gte / lt / lte ────────────────────────────────────
            "gt" | "gte" | "lt" | "lte" => {
                let a = arg(args, 0);
                let b = arg(args, 1);
                let (an, bn) = numeric_pair(&a, &b, func, span)?;
                let ok = match func {
                    "gt" => an > bn,
                    "gte" => an >= bn,
                    "lt" => an < bn,
                    "lte" => an <= bn,
                    _ => unreachable!(),
                };
                if !ok {
                    return Err(fail(
                        format!(
                            "assert.{} failed: {} {} {}",
                            func,
                            a,
                            cmp_op(func),
                            b
                        ),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.contains(haystack, needle) ─────────────────────────────
            "contains" => {
                let haystack = arg(args, 0);
                let needle = arg(args, 1);
                let found = match &haystack {
                    Value::Str(s) => {
                        // needle must be a string for substring search
                        match &needle {
                            Value::Str(n) => s.contains(n.as_ref()),
                            _ => {
                                return Err(AsError::at(
                                    format!(
                                        "assert.contains: string haystack needs a string needle, got {}",
                                        crate::interp::type_name(&needle)
                                    ),
                                    span,
                                )
                                .into())
                            }
                        }
                    }
                    Value::Array(a) => {
                        // membership by == (Value PartialEq)
                        a.borrow().iter().any(|elem| deep_equal(elem, &needle))
                    }
                    Value::Object(o) => {
                        // key presence — needle must be a string
                        match &needle {
                            Value::Str(k) => o.borrow().contains_key(k.as_ref()),
                            _ => {
                                return Err(AsError::at(
                                    format!(
                                        "assert.contains: object haystack needs a string key, got {}",
                                        crate::interp::type_name(&needle)
                                    ),
                                    span,
                                )
                                .into())
                            }
                        }
                    }
                    Value::Map(m) => {
                        // key presence — needle must be a hashable map key
                        match crate::value::MapKey::from_value(&needle) {
                            Some(k) => m.borrow().contains_key(&k),
                            None => {
                                return Err(AsError::at(
                                    format!(
                                        "assert.contains: map haystack needs a hashable key, got {}",
                                        crate::interp::type_name(&needle)
                                    ),
                                    span,
                                )
                                .into())
                            }
                        }
                    }
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "assert.contains expects a string, array, object, or map as haystack, got {}",
                                crate::interp::type_name(&haystack)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                if !found {
                    return Err(fail(
                        format!("assert.contains failed: {} not in {}", needle, haystack),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.approxEq(a, b, epsilon?) ──────────────────────────────
            "approxEq" => {
                let a = arg(args, 0);
                let b = arg(args, 1);
                // Both Number and Decimal are accepted (consistent with gt/gte/lt/lte).
                let (an, bn) = numeric_pair(&a, &b, "approxEq", span)?;
                let epsilon = match arg(args, 2) {
                    Value::Nil => 1e-9_f64,
                    Value::Number(n) => n,
                    Value::Decimal(d) => d.to_f64().unwrap_or(f64::NAN),
                    v => {
                        return Err(AsError::at(
                            format!(
                                "assert.approxEq epsilon expects a number, got {}",
                                crate::interp::type_name(&v)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                if (an - bn).abs() > epsilon {
                    return Err(fail(
                        format!(
                            "assert.approxEq failed: |{} - {}| = {} > epsilon {}",
                            a,
                            b,
                            (an - bn).abs(),
                            epsilon
                        ),
                        None,
                        span,
                    ));
                }
                Ok(Value::Nil)
            }
            // ── assert.throws(fn) -> errValue ─────────────────────────────────
            //
            // Calls fn with no arguments.  If fn returns a Value::Future (i.e.
            // it is an async fn), that future is driven to completion before
            // checking whether a panic occurred.  Pattern mirrors `recover` +
            // the `task.retry` future-drive idiom.
            "throws" => {
                let callee = arg(args, 0);
                let call_result = self.call_value(callee, vec![], span).await;
                // Drive any returned future to completion (async fn path).
                let result: Result<Value, Control> = match call_result {
                    Ok(Value::Future(f)) => f.get().await,
                    other => other,
                };
                match result {
                    Err(Control::Panic(e)) => {
                        // Return the error value (same shape recover returns).
                        Ok(make_error(Value::Str(e.message.into())))
                    }
                    Err(other) => {
                        // Propagate / Exit pass through unchanged.
                        Err(other)
                    }
                    Ok(_) => {
                        // fn returned normally — assert.throws should panic.
                        Err(AsError::at(
                            "assert.throws failed: expected fn to throw, but it returned normally",
                            span,
                        )
                        .into())
                    }
                }
            }
            _ => Err(AsError::at(
                format!("assert has no function '{}'", func),
                span,
            )
            .into()),
        }
    }
}

// ── private helpers ──────────────────────────────────────────────────────────

/// Extract an optional user-provided message string (3rd/2nd arg etc.).
fn opt_str(args: &[Value], i: usize) -> Option<String> {
    match args.get(i) {
        Some(Value::Str(s)) => Some(s.to_string()),
        Some(Value::Nil) | None => None,
        Some(v) => Some(v.to_string()),
    }
}

/// Unwrap both values as numbers; panics with a clear message if either is not.
fn numeric_pair(
    a: &Value,
    b: &Value,
    func: &str,
    span: Span,
) -> Result<(f64, f64), Control> {
    let an = match a {
        Value::Number(n) => *n,
        Value::Decimal(d) => d.to_f64().unwrap_or(f64::NAN),
        _ => {
            return Err(AsError::at(
                format!(
                    "assert.{} expects numbers, got {} for first argument",
                    func,
                    crate::interp::type_name(a)
                ),
                span,
            )
            .into())
        }
    };
    let bn = match b {
        Value::Number(n) => *n,
        Value::Decimal(d) => d.to_f64().unwrap_or(f64::NAN),
        _ => {
            return Err(AsError::at(
                format!(
                    "assert.{} expects numbers, got {} for second argument",
                    func,
                    crate::interp::type_name(b)
                ),
                span,
            )
            .into())
        }
    };
    Ok((an, bn))
}

fn cmp_op(func: &str) -> &'static str {
    match func {
        "gt" => ">",
        "gte" => ">=",
        "lt" => "<",
        "lte" => "<=",
        _ => "?",
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should succeed")
    }

    async fn run_err(src: &str) -> String {
        crate::run_source(src)
            .await
            .expect_err("program should fail")
            .message
    }

    // ── assert.eq ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn eq_primitives_pass() {
        run(r#"
import * as assert from "std/assert"
assert.eq(1, 1)
assert.eq("hello", "hello")
assert.eq(true, true)
assert.eq(nil, nil)
"#)
        .await;
    }

    #[tokio::test]
    async fn eq_deep_arrays_pass() {
        run(r#"
import * as assert from "std/assert"
assert.eq([1, 2, 3], [1, 2, 3])
assert.eq([[1], [2]], [[1], [2]])
"#)
        .await;
    }

    #[tokio::test]
    async fn eq_deep_objects_pass() {
        run(r#"
import * as assert from "std/assert"
assert.eq({a: 1, b: 2}, {a: 1, b: 2})
"#)
        .await;
    }

    #[tokio::test]
    async fn eq_fails_with_panic() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.eq([1], [2]))
print(r[1] != nil)
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.starts_with("true\n"), "expected 'true' first line, got: {out}");
        assert!(out.contains("assert.eq failed"), "expected 'assert.eq failed' in: {out}");
    }

    #[tokio::test]
    async fn eq_with_user_msg() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.eq(1, 2, "my message"))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.contains("my message"), "expected user msg in: {out}");
    }

    // ── assert.ne ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ne_pass() {
        run(r#"
import * as assert from "std/assert"
assert.ne(1, 2)
assert.ne([1], [2])
"#)
        .await;
    }

    #[tokio::test]
    async fn ne_fails_when_equal() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.ne([1, 2], [1, 2]))
print(r[1] != nil)
"#;
        let out = run(src).await;
        assert_eq!(out, "true\n");
    }

    // ── assert.isTrue / assert.isFalse ──────────────────────────────────────

    #[tokio::test]
    async fn is_true_pass() {
        run(r#"
import * as assert from "std/assert"
assert.isTrue(1)
assert.isTrue("x")
assert.isTrue([])
"#)
        .await;
    }

    #[tokio::test]
    async fn is_true_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.isTrue(false))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn is_false_pass() {
        run(r#"
import * as assert from "std/assert"
assert.isFalse(false)
assert.isFalse(nil)
"#)
        .await;
    }

    #[tokio::test]
    async fn is_false_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.isFalse(1))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.isNil / assert.notNil ────────────────────────────────────────

    #[tokio::test]
    async fn is_nil_pass() {
        run(r#"
import * as assert from "std/assert"
assert.isNil(nil)
"#)
        .await;
    }

    #[tokio::test]
    async fn is_nil_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.isNil(5))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn not_nil_pass() {
        run(r#"
import * as assert from "std/assert"
assert.notNil(5)
assert.notNil(false)
assert.notNil(0)
"#)
        .await;
    }

    #[tokio::test]
    async fn not_nil_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.notNil(nil))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.gt / gte / lt / lte ─────────────────────────────────────────

    #[tokio::test]
    async fn cmp_pass() {
        run(r#"
import * as assert from "std/assert"
assert.gt(3, 2)
assert.gte(3, 3)
assert.lt(1, 2)
assert.lte(2, 2)
"#)
        .await;
    }

    #[tokio::test]
    async fn gt_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.gt(1, 2))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn gt_type_misuse_panics() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.gt("a", "b"))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.contains ────────────────────────────────────────────────────

    #[tokio::test]
    async fn contains_string_pass() {
        run(r#"
import * as assert from "std/assert"
assert.contains("hello world", "ell")
"#)
        .await;
    }

    #[tokio::test]
    async fn contains_array_pass() {
        run(r#"
import * as assert from "std/assert"
assert.contains([1, 2, 3], 2)
assert.contains([[1], [2]], [1])
"#)
        .await;
    }

    #[tokio::test]
    async fn contains_object_key_pass() {
        run(r#"
import * as assert from "std/assert"
assert.contains({a: 1}, "a")
"#)
        .await;
    }

    #[tokio::test]
    async fn contains_string_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.contains("hello", "xyz"))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn contains_array_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.contains([1, 2, 3], 99))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn contains_object_missing_key_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.contains({a: 1}, "b"))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn contains_map_key_pass() {
        run(r#"
import * as assert from "std/assert"
import * as map from "std/map"
let m = map.new()
map.set(m, "a", 1)
map.set(m, 2, "two")
assert.contains(m, "a")
assert.contains(m, 2)
"#)
        .await;
    }

    #[tokio::test]
    async fn contains_map_missing_key_fail() {
        let src = r#"
import * as assert from "std/assert"
import * as map from "std/map"
let m = map.new()
map.set(m, "a", 1)
let r = recover(() => assert.contains(m, "b"))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.approxEq ────────────────────────────────────────────────────

    #[tokio::test]
    async fn approx_eq_pass() {
        run(r#"
import * as assert from "std/assert"
assert.approxEq(0.1 + 0.2, 0.3)
assert.approxEq(1.0, 1.0)
"#)
        .await;
    }

    #[tokio::test]
    async fn approx_eq_custom_epsilon_pass() {
        run(r#"
import * as assert from "std/assert"
assert.approxEq(1.0, 1.05, 0.1)
"#)
        .await;
    }

    #[tokio::test]
    async fn approx_eq_accepts_decimal() {
        // approxEq must accept Decimal args (consistent with gt/gte/lt/lte).
        run(r#"
import * as assert from "std/assert"
import * as decimal from "std/decimal"
assert.approxEq(decimal.from("0.1"), decimal.from("0.1"))
"#)
        .await;
    }

    #[tokio::test]
    async fn approx_eq_fail() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.approxEq(1, 2))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── assert.throws ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn throws_catches_panic_and_returns_error() {
        // Use assert(false, "boom") with a different local alias to avoid shadowing.
        let out = run(r#"
import * as A from "std/assert"
let e = A.throws(() => assert(false, "boom"))
print(e.message)
"#)
        .await;
        assert!(out.contains("boom"), "expected 'boom' in: {out}");
    }

    #[tokio::test]
    async fn throws_works_with_assert_eq_failure() {
        run(r#"
import * as assert from "std/assert"
let e = assert.throws(() => assert.eq(1, 2))
assert.contains(e.message, "assert.eq failed")
"#)
        .await;
    }

    #[tokio::test]
    async fn throws_fails_when_no_panic() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.throws(() => 1))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    #[tokio::test]
    async fn throws_message_contains_expected_phrase() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.throws(() => 42))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(
            out.contains("expected fn to throw"),
            "expected diagnostic, got: {out}"
        );
    }

    // ── assert.throws with async fn ────────────────────────────────────────

    #[tokio::test]
    async fn throws_drives_async_fn() {
        // assert.throws drives the future returned by an async fn.
        // Use [][0] (out-of-bounds) to trigger a panic inside the async fn.
        let out = run(r#"
import * as assert from "std/assert"
async fn boom() {
    let _ = [][0]
}
let e = await assert.throws(boom)
print(e.message)
"#)
        .await;
        assert!(!out.trim().is_empty(), "expected a non-empty error message, got: {out}");
    }

    #[tokio::test]
    async fn throws_fails_for_non_panicking_async_fn() {
        let src = r#"
import * as assert from "std/assert"
async fn ok() { return 42 }
let r = recover(() => assert.throws(ok))
print(r[1] != nil)
"#;
        assert_eq!(run(src).await, "true\n");
    }

    // ── error message format ────────────────────────────────────────────────

    #[tokio::test]
    async fn eq_message_shows_both_values() {
        let src = r#"
import * as assert from "std/assert"
let r = recover(() => assert.eq(42, 99))
print(r[1].message)
"#;
        let out = run(src).await;
        assert!(out.contains("42"), "expected '42' in: {out}");
        assert!(out.contains("99"), "expected '99' in: {out}");
    }

    #[tokio::test]
    async fn global_assert_still_works_without_import() {
        // Without importing std/assert, the global assert(cond) builtin still works.
        run(r#"assert(true)"#).await;
    }

    #[tokio::test]
    async fn global_assert_not_shadowed_with_different_alias() {
        // Importing std/assert under a different name leaves the global assert intact.
        run(r#"
import * as A from "std/assert"
assert(true)
A.eq(1, 1)
"#)
        .await;
    }

    #[tokio::test]
    async fn run_err_helper_works() {
        // Verify run_err captures panic messages (using out-of-bounds index).
        let msg = run_err("let _ = [][0]").await;
        assert!(!msg.is_empty(), "expected a non-empty error message");
    }
}
