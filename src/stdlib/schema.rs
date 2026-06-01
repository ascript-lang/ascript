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
        // ── 6b composites ────────────────────────────────────────────────────
        ("array", bi("schema.array")),
        ("object", bi("schema.object")),
        ("strict", bi("schema.strict")),
        ("map", bi("schema.map")),
        ("optional", bi("schema.optional")),
        ("union", bi("schema.union")),
        // `enum` is a reserved keyword (Tok::Enum) → use `oneOf`.
        // `schema.enum` is a parse error; the internal `__kind` tag is "oneOf".
        ("oneOf", bi("schema.oneOf")),
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
            // ── 6b: array ─────────────────────────────────────────────────────
            "array" => {
                let elem_schema = obj_field(schema, "elem").ok_or_else(|| {
                    SchemaErr::InvalidSchema("schema.parse: 'array' schema missing 'elem'".into())
                })?;
                match value {
                    Value::Array(arr) => {
                        let items: Vec<Value> = arr.borrow().clone();
                        let mut out = Vec::with_capacity(items.len());
                        for (i, item) in items.iter().enumerate() {
                            let item_path = format!("{}[{}]", path, i);
                            let v = self
                                .parse_value(&elem_schema, item, &item_path)
                                .await?;
                            out.push(v);
                        }
                        Ok(Value::Array(Rc::new(RefCell::new(out))))
                    }
                    _ => Err(SchemaErr::Mismatch(err_obj(
                        path,
                        format!("expected array, got {}", type_name(value)),
                    ))),
                }
            }

            // ── 6b: object ────────────────────────────────────────────────────
            "object" => {
                let fields_schema = obj_field(schema, "fields").ok_or_else(|| {
                    SchemaErr::InvalidSchema(
                        "schema.parse: 'object' schema missing 'fields'".into(),
                    )
                })?;
                let is_strict = matches!(obj_field(schema, "strict"), Some(Value::Bool(true)));

                match value {
                    Value::Object(val_obj) => {
                        // Collect declared field names and schemas
                        let field_pairs: Vec<(String, Value)> =
                            match &fields_schema {
                                Value::Object(fs) => fs
                                    .borrow()
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect(),
                                _ => {
                                    return Err(SchemaErr::InvalidSchema(
                                        "schema.parse: 'fields' must be an Object".into(),
                                    ))
                                }
                            };

                        // strict: reject extra keys
                        if is_strict {
                            let declared: std::collections::HashSet<&str> =
                                field_pairs.iter().map(|(k, _)| k.as_str()).collect();
                            let val_borrow = val_obj.borrow();
                            for k in val_borrow.keys() {
                                if !declared.contains(k.as_str()) {
                                    let key_path = if path.is_empty() {
                                        k.clone()
                                    } else {
                                        format!("{}.{}", path, k)
                                    };
                                    return Err(SchemaErr::Mismatch(err_obj(
                                        &key_path,
                                        format!(
                                            "unknown key '{}' not allowed in strict object",
                                            k
                                        ),
                                    )));
                                }
                            }
                        }

                        // Validate declared fields
                        let mut out: IndexMap<String, Value> = IndexMap::new();
                        for (field_name, field_schema) in &field_pairs {
                            let field_val = val_obj
                                .borrow()
                                .get(field_name)
                                .cloned()
                                .unwrap_or(Value::Nil);
                            let field_path = if path.is_empty() {
                                field_name.clone()
                            } else {
                                format!("{}.{}", path, field_name)
                            };
                            let parsed = self
                                .parse_value(field_schema, &field_val, &field_path)
                                .await?;
                            out.insert(field_name.clone(), parsed);
                        }
                        Ok(Value::Object(Rc::new(RefCell::new(out))))
                    }
                    _ => Err(SchemaErr::Mismatch(err_obj(
                        path,
                        format!("expected object, got {}", type_name(value)),
                    ))),
                }
            }

            // ── 6b: map ───────────────────────────────────────────────────────
            "map" => {
                use crate::value::MapKey;
                let key_schema = obj_field(schema, "key").ok_or_else(|| {
                    SchemaErr::InvalidSchema("schema.parse: 'map' schema missing 'key'".into())
                })?;
                let val_schema = obj_field(schema, "val").ok_or_else(|| {
                    SchemaErr::InvalidSchema("schema.parse: 'map' schema missing 'val'".into())
                })?;

                // Collect entries from either Map or Object (coerce Object→Map).
                let entries: Vec<(Value, Value)> = match value {
                    Value::Map(m) => m
                        .borrow()
                        .iter()
                        .map(|(k, v)| (k.to_value(), v.clone()))
                        .collect(),
                    Value::Object(o) => o
                        .borrow()
                        .iter()
                        .map(|(k, v)| (Value::Str(k.as_str().into()), v.clone()))
                        .collect(),
                    _ => {
                        return Err(SchemaErr::Mismatch(err_obj(
                            path,
                            format!("expected map or object, got {}", type_name(value)),
                        )))
                    }
                };

                let mut out: IndexMap<MapKey, Value> = IndexMap::new();
                for (raw_key, raw_val) in entries {
                    // Value path: "<path>[<key>]" (e.g. `cfg[port]`).
                    // Key path appends a "(key)" marker so a key-validation error
                    // is distinguishable from a value-validation error at the same
                    // entry (e.g. `cfg[port] (key)`).
                    let val_path = format!("{}[{}]", path, raw_key);
                    let key_path = format!("{} (key)", val_path);
                    let parsed_key =
                        self.parse_value(&key_schema, &raw_key, &key_path).await?;
                    let parsed_val =
                        self.parse_value(&val_schema, &raw_val, &val_path).await?;
                    let map_key = MapKey::from_value(&parsed_key).ok_or_else(|| {
                        SchemaErr::Mismatch(err_obj(
                            &key_path,
                            format!("map key type {} is not hashable", type_name(&parsed_key)),
                        ))
                    })?;
                    out.insert(map_key, parsed_val);
                }
                Ok(Value::Map(Rc::new(RefCell::new(out))))
            }

            // ── 6b: optional ──────────────────────────────────────────────────
            "optional" => {
                if matches!(value, Value::Nil) {
                    return Ok(Value::Nil);
                }
                let inner = obj_field(schema, "inner").ok_or_else(|| {
                    SchemaErr::InvalidSchema(
                        "schema.parse: 'optional' schema missing 'inner'".into(),
                    )
                })?;
                self.parse_value(&inner, value, path).await
            }

            // ── 6b: union ─────────────────────────────────────────────────────
            "union" => {
                let options = obj_field(schema, "options").ok_or_else(|| {
                    SchemaErr::InvalidSchema(
                        "schema.parse: 'union' schema missing 'options'".into(),
                    )
                })?;
                let opts: Vec<Value> = match &options {
                    Value::Array(a) => a.borrow().clone(),
                    _ => {
                        return Err(SchemaErr::InvalidSchema(
                            "schema.parse: 'union' options must be an Array".into(),
                        ))
                    }
                };
                let mut kinds: Vec<String> = Vec::new();
                for opt in &opts {
                    match self.parse_value(opt, value, path).await {
                        Ok(v) => return Ok(v),
                        Err(SchemaErr::Mismatch(_)) => {
                            kinds.push(
                                schema_kind(opt)
                                    .map(|k| k.to_string())
                                    .unwrap_or_else(|| "?".into()),
                            );
                        }
                        Err(e @ SchemaErr::InvalidSchema(_)) => return Err(e),
                    }
                }
                Err(SchemaErr::Mismatch(err_obj(
                    path,
                    format!("expected one of [{}], got {}", kinds.join(", "), type_name(value)),
                )))
            }

            // ── 6b: oneOf (enum-like, `enum` is a keyword) ───────────────────
            "oneOf" => {
                let values_field = obj_field(schema, "values").ok_or_else(|| {
                    SchemaErr::InvalidSchema(
                        "schema.parse: 'oneOf' schema missing 'values'".into(),
                    )
                })?;
                let allowed: Vec<Value> = match &values_field {
                    Value::Array(a) => a.borrow().clone(),
                    _ => {
                        return Err(SchemaErr::InvalidSchema(
                            "schema.parse: 'oneOf' values must be an Array".into(),
                        ))
                    }
                };
                for v in &allowed {
                    if value == v {
                        return Ok(value.clone());
                    }
                }
                let listed: Vec<String> = allowed.iter().map(|v| format!("{}", v)).collect();
                Err(SchemaErr::Mismatch(err_obj(
                    path,
                    format!(
                        "expected one of [{}], got {}",
                        listed.join(", "),
                        value
                    ),
                )))
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

            // ── 6b composite constructors ─────────────────────────────────────

            // schema.array(elemSchema) → {__kind:"array", elem}
            "array" => {
                let elem = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("array".into()));
                m.insert("elem".to_string(), elem);
                Ok(Value::Object(Rc::new(RefCell::new(m))))
            }

            // schema.object(fieldsObj) → {__kind:"object", fields, strict:false}
            "object" => {
                let fields = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("object".into()));
                m.insert("fields".to_string(), fields);
                m.insert("strict".to_string(), Value::Bool(false));
                Ok(Value::Object(Rc::new(RefCell::new(m))))
            }

            // schema.strict(objSchema) → clone with strict:true
            "strict" => {
                let obj_schema = arg(args, 0);
                // Verify it's an object schema
                match schema_kind(&obj_schema).as_deref() {
                    Some("object") => {
                        // Clone the fields, set strict:true
                        match &obj_schema {
                            Value::Object(o) => {
                                let mut m: IndexMap<String, Value> = o.borrow().clone();
                                m.insert("strict".to_string(), Value::Bool(true));
                                Ok(Value::Object(Rc::new(RefCell::new(m))))
                            }
                            _ => unreachable!(),
                        }
                    }
                    _ => Err(AsError::at(
                        "schema.strict: argument must be an object schema",
                        span,
                    )
                    .into()),
                }
            }

            // schema.map(keySchema, valSchema) → {__kind:"map", key, val}
            "map" => {
                let key = arg(args, 0);
                let val = arg(args, 1);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("map".into()));
                m.insert("key".to_string(), key);
                m.insert("val".to_string(), val);
                Ok(Value::Object(Rc::new(RefCell::new(m))))
            }

            // schema.optional(innerSchema) → {__kind:"optional", inner}
            "optional" => {
                let inner = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("optional".into()));
                m.insert("inner".to_string(), inner);
                Ok(Value::Object(Rc::new(RefCell::new(m))))
            }

            // schema.union(list) → {__kind:"union", options:[...]}
            "union" => {
                let options = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("union".into()));
                m.insert("options".to_string(), options);
                Ok(Value::Object(Rc::new(RefCell::new(m))))
            }

            // schema.oneOf(list) → {__kind:"oneOf", values:[...]}
            // (`enum` is a reserved keyword in AScript, so `oneOf` is the exported name.)
            "oneOf" => {
                let values = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("oneOf".into()));
                m.insert("values".to_string(), values);
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

    // ── 6b composite: schema.array ────────────────────────────────────────────

    #[tokio::test]
    async fn array_schema_ok() {
        let interp = crate::interp::Interp::new();
        // Build array(number()) schema
        let num_schema = make_schema("number");
        let arr_schema = interp
            .call_schema("array", &[num_schema], sp())
            .await
            .unwrap();
        // [1, 2] → ok
        let arr = Value::Array(Rc::new(RefCell::new(vec![
            Value::Number(1.0),
            Value::Number(2.0),
        ])));
        let pair = interp
            .call_schema("parse", &[arr_schema, arr], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn array_schema_err_path() {
        let interp = crate::interp::Interp::new();
        let num_schema = make_schema("number");
        let arr_schema = interp
            .call_schema("array", &[num_schema], sp())
            .await
            .unwrap();
        // [1, "x"] → err path "[1]"
        let arr = Value::Array(Rc::new(RefCell::new(vec![
            Value::Number(1.0),
            Value::Str("x".into()),
        ])));
        let pair = interp
            .call_schema("parse", &[arr_schema, arr], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let err = err_val(&pair);
        assert_eq!(field(&err, "path"), Value::Str("[1]".into()));
    }

    #[tokio::test]
    async fn array_schema_not_array() {
        let interp = crate::interp::Interp::new();
        let num_schema = make_schema("number");
        let arr_schema = interp
            .call_schema("array", &[num_schema], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[arr_schema, Value::Str("x".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("expected array"), "msg: {}", msg);
    }

    // ── 6b composite: schema.object ───────────────────────────────────────────

    fn make_fields_obj(pairs: &[(&str, Value)]) -> Value {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v.clone());
        }
        Value::Object(Rc::new(RefCell::new(m)))
    }

    fn make_value_obj(pairs: &[(&str, Value)]) -> Value {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v.clone());
        }
        Value::Object(Rc::new(RefCell::new(m)))
    }

    #[tokio::test]
    async fn object_schema_ok() {
        let interp = crate::interp::Interp::new();
        let fields = make_fields_obj(&[
            ("a", make_schema("number")),
            ("b", make_schema("string")),
        ]);
        let obj_schema = interp
            .call_schema("object", &[fields], sp())
            .await
            .unwrap();
        let value = make_value_obj(&[
            ("a", Value::Number(1.0)),
            ("b", Value::Str("x".into())),
        ]);
        let pair = interp
            .call_schema("parse", &[obj_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn object_schema_err_path_at_root() {
        let interp = crate::interp::Interp::new();
        let fields = make_fields_obj(&[
            ("a", make_schema("number")),
            ("b", make_schema("string")),
        ]);
        let obj_schema = interp
            .call_schema("object", &[fields], sp())
            .await
            .unwrap();
        // b is a number but schema expects string
        let value = make_value_obj(&[
            ("a", Value::Number(1.0)),
            ("b", Value::Number(2.0)),
        ]);
        let pair = interp
            .call_schema("parse", &[obj_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let err = err_val(&pair);
        // at root, path is just "b"
        assert_eq!(field(&err, "path"), Value::Str("b".into()));
    }

    #[tokio::test]
    async fn object_schema_nested_err_path() {
        let interp = crate::interp::Interp::new();
        // object({user: object({name: string()})})
        let name_fields = make_fields_obj(&[("name", make_schema("string"))]);
        let inner_schema = interp
            .call_schema("object", &[name_fields], sp())
            .await
            .unwrap();
        let outer_fields = make_fields_obj(&[("user", inner_schema)]);
        let outer_schema = interp
            .call_schema("object", &[outer_fields], sp())
            .await
            .unwrap();
        // user.name is a number (bad)
        let inner_val = make_value_obj(&[("name", Value::Number(42.0))]);
        let value = make_value_obj(&[("user", inner_val)]);
        let pair = interp
            .call_schema("parse", &[outer_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let err = err_val(&pair);
        assert_eq!(field(&err, "path"), Value::Str("user.name".into()));
    }

    #[tokio::test]
    async fn object_schema_ignores_extra_keys() {
        let interp = crate::interp::Interp::new();
        let fields = make_fields_obj(&[("a", make_schema("number"))]);
        let obj_schema = interp
            .call_schema("object", &[fields], sp())
            .await
            .unwrap();
        // extra key "b" should be ignored
        let value = make_value_obj(&[
            ("a", Value::Number(1.0)),
            ("b", Value::Str("extra".into())),
        ]);
        let pair = interp
            .call_schema("parse", &[obj_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn object_schema_not_object() {
        let interp = crate::interp::Interp::new();
        let fields = make_fields_obj(&[("a", make_schema("number"))]);
        let obj_schema = interp
            .call_schema("object", &[fields], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[obj_schema, Value::Number(5.0)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("expected object"), "msg: {}", msg);
    }

    // ── 6b composite: schema.strict ──────────────────────────────────────────

    #[tokio::test]
    async fn strict_object_rejects_extra_keys() {
        let interp = crate::interp::Interp::new();
        let fields = make_fields_obj(&[("a", make_schema("number"))]);
        let obj_schema = interp
            .call_schema("object", &[fields], sp())
            .await
            .unwrap();
        let strict_schema = interp
            .call_schema("strict", &[obj_schema], sp())
            .await
            .unwrap();
        // a=1 and b=2 (extra) → error
        let value = make_value_obj(&[
            ("a", Value::Number(1.0)),
            ("b", Value::Number(2.0)),
        ]);
        let pair = interp
            .call_schema("parse", &[strict_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("unknown key") || msg.contains("unexpected key"), "msg: {}", msg);
    }

    #[tokio::test]
    async fn strict_object_ok_no_extra() {
        let interp = crate::interp::Interp::new();
        let fields = make_fields_obj(&[("a", make_schema("number"))]);
        let obj_schema = interp
            .call_schema("object", &[fields], sp())
            .await
            .unwrap();
        let strict_schema = interp
            .call_schema("strict", &[obj_schema], sp())
            .await
            .unwrap();
        let value = make_value_obj(&[("a", Value::Number(1.0))]);
        let pair = interp
            .call_schema("parse", &[strict_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    // ── 6b composite: schema.optional ────────────────────────────────────────

    #[tokio::test]
    async fn optional_nil_ok() {
        let interp = crate::interp::Interp::new();
        let num_schema = make_schema("number");
        let opt_schema = interp
            .call_schema("optional", &[num_schema], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[opt_schema, Value::Nil], sp())
            .await
            .unwrap();
        // nil → ok with value nil
        assert_eq!(ok_val(&pair), Value::Nil);
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn optional_value_passes_through() {
        let interp = crate::interp::Interp::new();
        let num_schema = make_schema("number");
        let opt_schema = interp
            .call_schema("optional", &[num_schema], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[opt_schema, Value::Number(5.0)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Number(5.0));
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn optional_mismatch_err() {
        let interp = crate::interp::Interp::new();
        let num_schema = make_schema("number");
        let opt_schema = interp
            .call_schema("optional", &[num_schema], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[opt_schema, Value::Str("x".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        assert!(matches!(err_val(&pair), Value::Object(_)));
    }

    // ── 6b composite: schema.union ───────────────────────────────────────────

    fn make_array_val(items: Vec<Value>) -> Value {
        Value::Array(Rc::new(RefCell::new(items)))
    }

    #[tokio::test]
    async fn union_accepts_first_match() {
        let interp = crate::interp::Interp::new();
        let options = make_array_val(vec![make_schema("string"), make_schema("number")]);
        let union_schema = interp
            .call_schema("union", &[options], sp())
            .await
            .unwrap();
        // string "x" matches first option
        let pair = interp
            .call_schema("parse", &[union_schema.clone(), Value::Str("x".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Str("x".into()));
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn union_accepts_second_match() {
        let interp = crate::interp::Interp::new();
        let options = make_array_val(vec![make_schema("string"), make_schema("number")]);
        let union_schema = interp
            .call_schema("union", &[options], sp())
            .await
            .unwrap();
        // number 5 matches second option
        let pair = interp
            .call_schema("parse", &[union_schema, Value::Number(5.0)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Number(5.0));
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn union_rejects_none_match() {
        let interp = crate::interp::Interp::new();
        let options = make_array_val(vec![make_schema("string"), make_schema("number")]);
        let union_schema = interp
            .call_schema("union", &[options], sp())
            .await
            .unwrap();
        // bool true doesn't match string or number
        let pair = interp
            .call_schema("parse", &[union_schema, Value::Bool(true)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        assert!(matches!(err_val(&pair), Value::Object(_)));
    }

    // ── 6b composite: schema.oneOf (enum-like) ───────────────────────────────

    #[tokio::test]
    async fn one_of_accepts_listed_value() {
        let interp = crate::interp::Interp::new();
        let vals = make_array_val(vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
        ]);
        let schema = interp.call_schema("oneOf", &[vals], sp()).await.unwrap();
        let pair = interp
            .call_schema("parse", &[schema, Value::Str("a".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Str("a".into()));
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn one_of_rejects_unlisted_value() {
        let interp = crate::interp::Interp::new();
        let vals = make_array_val(vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
        ]);
        let schema = interp.call_schema("oneOf", &[vals], sp()).await.unwrap();
        let pair = interp
            .call_schema("parse", &[schema, Value::Str("c".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("expected one of"), "msg: {}", msg);
    }

    // ── 6b composite: schema.map ─────────────────────────────────────────────

    #[tokio::test]
    async fn map_schema_from_object_ok() {
        let interp = crate::interp::Interp::new();
        let map_schema = interp
            .call_schema(
                "map",
                &[make_schema("string"), make_schema("number")],
                sp(),
            )
            .await
            .unwrap();
        // Object {"k": 1} → coerced to map at boundary
        let value = make_value_obj(&[("k", Value::Number(1.0))]);
        let pair = interp
            .call_schema("parse", &[map_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn map_schema_value_mismatch() {
        let interp = crate::interp::Interp::new();
        let map_schema = interp
            .call_schema(
                "map",
                &[make_schema("string"), make_schema("number")],
                sp(),
            )
            .await
            .unwrap();
        // value is string instead of number
        let value = make_value_obj(&[("k", Value::Str("bad".into()))]);
        let pair = interp
            .call_schema("parse", &[map_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        assert!(matches!(err_val(&pair), Value::Object(_)));
    }

    #[tokio::test]
    async fn map_schema_key_mismatch_has_key_marker() {
        let interp = crate::interp::Interp::new();
        // key must be a number, value a number; an Object always has string keys,
        // so the KEY validation fails — its err.path must carry the "(key)" marker
        // to distinguish it from a value-validation error at the same entry.
        let map_schema = interp
            .call_schema(
                "map",
                &[make_schema("number"), make_schema("number")],
                sp(),
            )
            .await
            .unwrap();
        let value = make_value_obj(&[("k", Value::Number(1.0))]);
        let pair = interp
            .call_schema("parse", &[map_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let err = err_val(&pair);
        let path = match field(&err, "path") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(path.contains("(key)"), "key error path should carry marker: {}", path);
        // sanity: the message is the key-mismatch one, not a value mismatch
        let msg = match field(&err, "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("expected number"), "msg: {}", msg);
    }

    // ── 6b composite: schema.union propagates InvalidSchema ───────────────────

    #[tokio::test]
    async fn union_propagates_invalid_schema_panic() {
        let interp = crate::interp::Interp::new();
        // First option is a malformed sub-schema (raw object, no __kind); union
        // must NOT swallow it as a mismatch — it must propagate InvalidSchema,
        // which surfaces at the parse boundary as a Tier-2 Control::Panic.
        let bad_sub = make_value_obj(&[("foo", Value::Number(1.0))]); // no __kind
        let options = make_array_val(vec![bad_sub, make_schema("number")]);
        let union_schema = interp
            .call_schema("union", &[options], sp())
            .await
            .unwrap();
        let result = interp
            .call_schema("parse", &[union_schema, Value::Str("x".into())], sp())
            .await;
        assert!(
            matches!(result, Err(Control::Panic(_))),
            "expected Tier-2 panic from malformed union option, got {:?}",
            result
        );
    }
}
