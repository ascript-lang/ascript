//! `std/schema` — composable schema validators.
//!
//! Schemas are tagged AScript Objects `{__kind: "<t>", ...}`.
//! `schema.parse(schema, value)` dispatches on `__kind` and returns a
//! Tier-1 `[value, err]` pair; err is an Object `{path, message}` on
//! failure, or `nil` on success.
//!
//! The internal parse engine is the async method `Interp::parse_value(schema,
//! value, path)` which returns `Result<Value, SchemaErr>`.  Future sub-phases
//! (6b composites, 6c constraints/refine) extend it with new `__kind` arms
//! without touching the public API: it is `async fn` on `&self` already so 6b
//! recursion and 6c `refine` (which calls user fns via `self.call_value`) just
//! add match arms.

use super::{arg, bi};
use crate::error::AsError;
use crate::interp::{make_pair, type_name, Control, Interp};
use crate::span::Span;
use crate::value::Value;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

// ── public exports ────────────────────────────────────────────────────────────

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("string", bi("schema.string")),
        ("number", bi("schema.number")),
        ("bool", bi("schema.bool")),
        // `nil` is a reserved keyword (Tok::Nil), so the constructor is exposed
        // as `nilType` — `schema.nilType()` validates that a value IS nil. The
        // internal `__kind` tag stays "nil".
        ("nilType", bi("schema.nilType")),
        ("any", bi("schema.any")),
        ("literal", bi("schema.literal")),
        ("parse", bi("schema.parse")),
    ]
}

// ── internal error type ───────────────────────────────────────────────────────

/// The error channel of the parse engine.
///
/// - `Mismatch` is a Tier-1 validation failure carrying the `{path, message}`
///   error Object — it surfaces as the `err` slot of the `[value, err]` pair.
/// - `InvalidSchema` is a Tier-2 programmer error (a malformed schema node, e.g.
///   a nested value with no `__kind`) — `call_schema` escalates it to a
///   `Control::Panic` so it is never silently swallowed as a validation error.
#[derive(Debug)]
enum SchemaErr {
    Mismatch(Value),
    InvalidSchema(String),
}

// ── tagged-object helpers ─────────────────────────────────────────────────────

/// Build a schema tag object `{__kind: kind}`.
fn make_schema(kind: &str) -> Value {
    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert("__kind".to_string(), Value::Str(kind.into()));
    Value::Object(Rc::new(RefCell::new(m)))
}

/// Build a `{path, message}` error detail object for the Tier-1 err slot.
fn err_obj(path: &str, message: String) -> Value {
    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert("path".to_string(), Value::Str(path.into()));
    m.insert("message".to_string(), Value::Str(message.into()));
    Value::Object(Rc::new(RefCell::new(m)))
}

/// Extract the `__kind` field from a schema Object, or return `None` if the
/// value is not an Object or has no `__kind` string.
fn schema_kind(schema: &Value) -> Option<Rc<str>> {
    match schema {
        Value::Object(o) => match o.borrow().get("__kind") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Get a field from a `Value::Object`.
fn obj_field(obj: &Value, key: &str) -> Option<Value> {
    match obj {
        Value::Object(o) => o.borrow().get(key).cloned(),
        _ => None,
    }
}

// ── Interp dispatch + parse engine (async, on &self for 6b/6c) ──────────────────

impl Interp {
    /// The recursive parse engine.  Accepts the schema node, the candidate
    /// value, and the current dot-path string (empty at top level). Returns
    /// `Ok(coerced_value)` on success or `Err(SchemaErr)` on mismatch / malformed
    /// schema.
    ///
    /// It is `async fn` on `&self` even though 6a primitives never `.await` —
    /// 6b composites recurse into this method and 6c's `refine` calls user
    /// functions via `self.call_value(...).await`, so the async/`&self` shape is
    /// in place now and those sub-phases only add match arms.
    ///
    /// Extension points for 6b:
    ///   "object"  → recursively parse each declared field, building a new Object
    ///   "array"   → map over items, recursively calling parse_value
    ///   "union"   → try each branch, return first success
    ///   "tuple"   → positional parse_value per element
    /// Extension point for 6c:
    ///   "refine"  → run a user predicate via `self.call_value(...).await`.
    ///
    /// Invariant: never hold a `RefCell` borrow across an `.await` (the future
    /// awaits in 6b/6c). The primitive arms below take no borrow across a yield.
    #[async_recursion::async_recursion(?Send)]
    async fn parse_value(
        &self,
        schema: &Value,
        value: &Value,
        path: &str,
    ) -> Result<Value, SchemaErr> {
        let kind = match schema_kind(schema) {
            Some(k) => k,
            // Not a schema object (no __kind) → Tier-2 (escalated to a panic by
            // the caller), never a silent validation failure.
            None => {
                return Err(SchemaErr::InvalidSchema(format!(
                    "schema.parse: not a valid schema object (missing __kind){}",
                    if path.is_empty() {
                        String::new()
                    } else {
                        format!(" at '{}'", path)
                    }
                )))
            }
        };

        match kind.as_ref() {
            "string" => {
                if matches!(value, Value::Str(_)) {
                    Ok(value.clone())
                } else {
                    Err(SchemaErr::Mismatch(err_obj(
                        path,
                        format!("expected string, got {}", type_name(value)),
                    )))
                }
            }
            "number" => {
                if matches!(value, Value::Number(_)) {
                    Ok(value.clone())
                } else {
                    Err(SchemaErr::Mismatch(err_obj(
                        path,
                        format!("expected number, got {}", type_name(value)),
                    )))
                }
            }
            "bool" => {
                if matches!(value, Value::Bool(_)) {
                    Ok(value.clone())
                } else {
                    Err(SchemaErr::Mismatch(err_obj(
                        path,
                        format!("expected bool, got {}", type_name(value)),
                    )))
                }
            }
            "nil" => {
                if matches!(value, Value::Nil) {
                    Ok(value.clone())
                } else {
                    Err(SchemaErr::Mismatch(err_obj(
                        path,
                        format!("expected nil, got {}", type_name(value)),
                    )))
                }
            }
            "any" => Ok(value.clone()),
            "literal" => {
                let expected = obj_field(schema, "value").unwrap_or(Value::Nil);
                if value == &expected {
                    Ok(value.clone())
                } else {
                    Err(SchemaErr::Mismatch(err_obj(
                        path,
                        format!("expected literal {}, got {}", expected, value),
                    )))
                }
            }
            other => Err(SchemaErr::InvalidSchema(format!(
                "schema.parse: unknown schema kind '{}'",
                other
            ))),
        }
    }

    pub(crate) async fn call_schema(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            // ── constructors (pure, build tagged Objects) ─────────────────────
            "string" => Ok(make_schema("string")),
            "number" => Ok(make_schema("number")),
            "bool" => Ok(make_schema("bool")),
            // `nil` is a keyword; the constructor is `nilType` (kind stays "nil").
            "nilType" => Ok(make_schema("nil")),
            "any" => Ok(make_schema("any")),
            "literal" => {
                let v = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("literal".into()));
                m.insert("value".to_string(), v);
                Ok(Value::Object(Rc::new(RefCell::new(m))))
            }

            // ── schema.parse(schema, value) -> [value, err] ───────────────────
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

            _ => Err(AsError::at(
                format!("std/schema has no function '{}'", func),
                span,
            )
            .into()),
        }
    }
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    /// Extract index 0 (ok slot) from a `[val, err]` pair.
    fn ok_val(pair: &Value) -> Value {
        match pair {
            Value::Array(a) => a.borrow()[0].clone(),
            _ => panic!("not a pair: {:?}", pair),
        }
    }
    /// Extract index 1 (err slot) from a `[val, err]` pair.
    fn err_val(pair: &Value) -> Value {
        match pair {
            Value::Array(a) => a.borrow()[1].clone(),
            _ => panic!("not a pair: {:?}", pair),
        }
    }
    /// Get a named field from an Object Value.
    fn field(obj: &Value, key: &str) -> Value {
        match obj {
            Value::Object(o) => o.borrow().get(key).cloned().unwrap_or(Value::Nil),
            _ => panic!("not an object: {:?}", obj),
        }
    }

    // The parse engine is `Interp::parse_value` (async on &self); tests drive it
    // through a fresh `Interp::new()` on the tokio runtime.

    // ── constructor smoke tests ───────────────────────────────────────────────

    #[test]
    fn constructor_string_kind() {
        let s = make_schema("string");
        assert_eq!(
            schema_kind(&s).as_deref(),
            Some("string")
        );
    }

    #[test]
    fn constructor_literal_value() {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("__kind".to_string(), Value::Str("literal".into()));
        m.insert("value".to_string(), Value::Number(5.0));
        let lit = Value::Object(Rc::new(RefCell::new(m)));
        assert_eq!(schema_kind(&lit).as_deref(), Some("literal"));
        assert_eq!(obj_field(&lit, "value"), Some(Value::Number(5.0)));
    }

    /// Unwrap a `SchemaErr::Mismatch` err Object; panic on `InvalidSchema`.
    fn mismatch(e: SchemaErr) -> Value {
        match e {
            SchemaErr::Mismatch(v) => v,
            SchemaErr::InvalidSchema(m) => panic!("expected Mismatch, got InvalidSchema: {}", m),
        }
    }

    // ── parse_value: success cases ────────────────────────────────────────────

    #[tokio::test]
    async fn parse_string_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("string");
        let v = Value::Str("hi".into());
        let result = interp.parse_value(&schema, &v, "").await;
        assert_eq!(result.unwrap(), Value::Str("hi".into()));
    }

    #[tokio::test]
    async fn parse_number_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("number");
        let v = Value::Number(42.0);
        let result = interp.parse_value(&schema, &v, "").await;
        assert_eq!(result.unwrap(), Value::Number(42.0));
    }

    #[tokio::test]
    async fn parse_bool_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("bool");
        let v = Value::Bool(true);
        let result = interp.parse_value(&schema, &v, "").await;
        assert_eq!(result.unwrap(), Value::Bool(true));
    }

    #[tokio::test]
    async fn parse_nil_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("nil");
        let result = interp.parse_value(&schema, &Value::Nil, "").await;
        assert_eq!(result.unwrap(), Value::Nil);
    }

    #[tokio::test]
    async fn parse_any_passes_everything() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("any");
        assert_eq!(
            interp
                .parse_value(&schema, &Value::Number(1.0), "")
                .await
                .unwrap(),
            Value::Number(1.0)
        );
        assert_eq!(
            interp
                .parse_value(&schema, &Value::Str("x".into()), "")
                .await
                .unwrap(),
            Value::Str("x".into())
        );
        assert_eq!(
            interp.parse_value(&schema, &Value::Nil, "").await.unwrap(),
            Value::Nil
        );
    }

    #[tokio::test]
    async fn parse_literal_ok() {
        let interp = crate::interp::Interp::new();
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("__kind".to_string(), Value::Str("literal".into()));
        m.insert("value".to_string(), Value::Number(5.0));
        let lit = Value::Object(Rc::new(RefCell::new(m)));
        let result = interp.parse_value(&lit, &Value::Number(5.0), "").await;
        assert_eq!(result.unwrap(), Value::Number(5.0));
    }

    // ── parse_value: failure cases ────────────────────────────────────────────

    #[tokio::test]
    async fn parse_string_fail_with_err_obj() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("string");
        let v = Value::Str("x".into()); // correct type for string — won't fail
        // Test mismatch: pass a number to a string schema
        let err = mismatch(
            interp
                .parse_value(&schema, &Value::Number(1.0), "")
                .await
                .unwrap_err(),
        );
        assert_eq!(field(&err, "path"), Value::Str("".into()));
        let msg = field(&err, "message");
        match &msg {
            Value::Str(s) => {
                assert!(s.contains("expected string"), "message was: {}", s);
                assert!(s.contains("number"), "message was: {}", s);
            }
            _ => panic!("message was not a string: {:?}", msg),
        }
        // The success case
        let ok = interp.parse_value(&schema, &v, "").await.unwrap();
        assert_eq!(ok, Value::Str("x".into()));
    }

    #[tokio::test]
    async fn parse_number_fail_string_input() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("number");
        let err = mismatch(
            interp
                .parse_value(&schema, &Value::Str("x".into()), "")
                .await
                .unwrap_err(),
        );
        assert_eq!(field(&err, "path"), Value::Str("".into()));
        let msg = match field(&err, "message") {
            Value::Str(s) => s,
            other => panic!("expected string message, got {:?}", other),
        };
        assert!(msg.contains("expected number"), "message: {}", msg);
        assert!(msg.contains("string"), "message: {}", msg);
    }

    #[tokio::test]
    async fn parse_literal_fail_wrong_value() {
        let interp = crate::interp::Interp::new();
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("__kind".to_string(), Value::Str("literal".into()));
        m.insert("value".to_string(), Value::Number(5.0));
        let lit = Value::Object(Rc::new(RefCell::new(m)));
        let err = mismatch(
            interp
                .parse_value(&lit, &Value::Number(6.0), "")
                .await
                .unwrap_err(),
        );
        let msg = match field(&err, "message") {
            Value::Str(s) => s,
            other => panic!("expected string message, got {:?}", other),
        };
        assert!(msg.contains("expected literal"), "message: {}", msg);
    }

    #[tokio::test]
    async fn no_kind_returns_invalid_schema() {
        // An object without __kind yields SchemaErr::InvalidSchema (Tier-2),
        // NOT a Mismatch — so 6b recursion can't swallow it as validation error.
        let interp = crate::interp::Interp::new();
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("a".to_string(), Value::Number(1.0));
        let bad = Value::Object(Rc::new(RefCell::new(m)));
        let err = interp.parse_value(&bad, &Value::Nil, "").await.unwrap_err();
        assert!(
            matches!(err, SchemaErr::InvalidSchema(_)),
            "expected InvalidSchema, got Mismatch"
        );
    }

    // ── Interp.call_schema tests (async, uses tokio runtime) ─────────────────

    #[tokio::test]
    async fn call_schema_string_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("string");
        let pair = interp
            .call_schema("parse", &[schema, Value::Str("hi".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Str("hi".into()));
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn call_schema_number_mismatch() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("number");
        let pair = interp
            .call_schema(
                "parse",
                &[schema, Value::Str("x".into())],
                sp(),
            )
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let err = err_val(&pair);
        assert_eq!(field(&err, "path"), Value::Str("".into()));
        let msg = match field(&err, "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("expected number"), "msg: {}", msg);
    }

    #[tokio::test]
    async fn call_schema_bool_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("bool");
        let pair = interp
            .call_schema("parse", &[schema, Value::Bool(true)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Bool(true));
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn call_schema_nil_ok() {
        let interp = crate::interp::Interp::new();
        // Build the schema via the `nilType` constructor (the script-facing name,
        // since `nil` is a keyword) — confirms it produces a `{__kind:"nil"}` tag.
        let schema = interp.call_schema("nilType", &[], sp()).await.unwrap();
        assert_eq!(schema_kind(&schema).as_deref(), Some("nil"));
        let pair = interp
            .call_schema("parse", &[schema, Value::Nil], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn call_schema_nil_type_rejects_non_nil() {
        let interp = crate::interp::Interp::new();
        let schema = interp.call_schema("nilType", &[], sp()).await.unwrap();
        let pair = interp
            .call_schema("parse", &[schema, Value::Number(1.0)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("expected nil"), "msg: {}", msg);
    }

    #[tokio::test]
    async fn call_schema_literal_ok_and_fail() {
        let interp = crate::interp::Interp::new();
        let lit = {
            let mut m: IndexMap<String, Value> = IndexMap::new();
            m.insert("__kind".to_string(), Value::Str("literal".into()));
            m.insert("value".to_string(), Value::Number(5.0));
            Value::Object(Rc::new(RefCell::new(m)))
        };
        // 5 == 5 → ok
        let pair = interp
            .call_schema("parse", &[lit.clone(), Value::Number(5.0)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Number(5.0));
        assert_eq!(err_val(&pair), Value::Nil);
        // 6 != 5 → err
        let pair2 = interp
            .call_schema("parse", &[lit, Value::Number(6.0)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair2), Value::Nil);
        assert!(matches!(err_val(&pair2), Value::Object(_)));
    }

    #[tokio::test]
    async fn call_schema_any_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("any");
        // number
        let p1 = interp
            .call_schema("parse", &[schema.clone(), Value::Number(1.0)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&p1), Value::Number(1.0));
        // string
        let p2 = interp
            .call_schema(
                "parse",
                &[schema, Value::Str("x".into())],
                sp(),
            )
            .await
            .unwrap();
        assert_eq!(ok_val(&p2), Value::Str("x".into()));
    }

    #[tokio::test]
    async fn call_schema_no_kind_panics() {
        let interp = crate::interp::Interp::new();
        // An Object without __kind → Tier-2 panic.
        let bad_obj = {
            let mut m: IndexMap<String, Value> = IndexMap::new();
            m.insert("a".to_string(), Value::Number(1.0));
            Value::Object(Rc::new(RefCell::new(m)))
        };
        // Non-Object schema args must ALSO panic (string / number / nil).
        let candidates = [
            bad_obj,
            Value::Str("not a schema".into()),
            Value::Number(3.0),
            Value::Nil,
        ];
        for schema in candidates {
            let result = interp
                .call_schema("parse", &[schema.clone(), Value::Number(1.0)], sp())
                .await;
            assert!(
                matches!(result, Err(Control::Panic(_))),
                "expected Tier-2 panic for schema {:?}, got {:?}",
                schema,
                result
            );
        }
    }
}
