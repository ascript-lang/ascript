//! The class / `std/schema` → JSON-Schema projector (SP11's one genuinely new
//! mechanism). `validate_into` / `std/schema`'s `parse_value` check an
//! ALREADY-PARSED value, but a provider needs a JSON Schema UP FRONT to constrain
//! structured output. This module derives a minimal JSON Schema (provider-agnostic)
//! from:
//!
//! - a class `FieldSchema` map (`class_to_json_schema`) — required/optional/
//!   nullable, nested class (resolved by the runtime caller), `array<T>`,
//!   `map<K,V>`, with defaults; and
//! - a `std/schema` tagged Object (`schema_value_to_json_schema`) — object/array/
//!   string(+minLength/maxLength/pattern)/number(+min/max)/bool/literal/oneOf/union/
//!   optional/map/any/nil.
//!
//! Emission is unit-tested standalone (no network). The result is fed to genai's
//! structured-output config per provider (or embedded in the prompt as a fallback);
//! the returned value is then re-validated via `validate_into`/`parse_value`, so the
//! projector only needs to be a faithful *shape* — the decode step is the source of
//! truth for correctness.

use serde_json::{json, Map, Value as J};

use crate::ast::Type;
use crate::value::{Class, Value};

/// Project a class into a JSON Schema object (`{type:"object", properties, required,
/// additionalProperties:false}`). Walks the superclass chain (own fields override).
/// Nested class fields resolve via `resolve_named` (the runtime passes the class
/// table; the standalone tests pass a closure that resolves the classes they build).
pub fn class_to_json_schema(class: &Class, resolve_named: &dyn Fn(&str) -> Option<J>) -> J {
    let mut properties = Map::new();
    let mut required: Vec<J> = Vec::new();

    let mut chain: Vec<&Class> = Vec::new();
    collect_chain(class, &mut chain);
    // Base-first so subclasses override.
    for c in chain.iter().rev() {
        for (name, fs) in &c.fields {
            let (schema, is_required) = type_to_json_schema(&fs.ty, resolve_named);
            properties.insert(name.clone(), schema);
            required.retain(|r| r.as_str() != Some(name.as_str()));
            if is_required && fs.default.is_none() {
                required.push(J::String(name.clone()));
            }
        }
    }

    let mut obj = Map::new();
    obj.insert("type".to_string(), J::String("object".to_string()));
    obj.insert("properties".to_string(), J::Object(properties));
    obj.insert("required".to_string(), J::Array(required));
    obj.insert("additionalProperties".to_string(), J::Bool(false));
    J::Object(obj)
}

/// Project a class resolving nested `Type::Named` classes through `env` (the class's
/// `def_env`, the same environment `validate_into` uses). Recurses to arbitrary
/// nesting depth, guarding against cycles via a visited-name set.
pub fn class_to_json_schema_env(class: &Class, env: &crate::env::Environment) -> J {
    let mut visited = std::collections::HashSet::new();
    class_to_json_schema_env_inner(class, env, &mut visited)
}

fn class_to_json_schema_env_inner(
    class: &Class,
    env: &crate::env::Environment,
    visited: &mut std::collections::HashSet<String>,
) -> J {
    visited.insert(class.name.clone());
    let env_clone = env.clone();
    let visited_snapshot = visited.clone();
    let resolve = move |name: &str| -> Option<J> {
        if visited_snapshot.contains(name) {
            // Cycle — emit an open object to terminate.
            return Some(json!({"type": "object"}));
        }
        match env_clone.get(name) {
            Some(Value::Class(nested)) => {
                // Fresh visited set per branch (snapshot + this class) so siblings
                // referencing the same class each get a full projection, while a true
                // cycle (a class reachable from itself) still terminates.
                let mut branch_visited = visited_snapshot.clone();
                Some(class_to_json_schema_env_inner(
                    &nested,
                    &nested.def_env,
                    &mut branch_visited,
                ))
            }
            _ => None,
        }
    };
    class_to_json_schema(class, &resolve)
}

fn collect_chain<'a>(class: &'a Class, out: &mut Vec<&'a Class>) {
    out.push(class);
    if let Some(sup) = &class.superclass {
        collect_chain(sup, out);
    }
}

/// Map an `ast::Type` to (json-schema, is_required). `is_required` is false for
/// `T?` / `T | nil` (nullable). A `Named(name)` resolves the nested class via
/// `resolve_named`; an unresolved name falls back to an open object.
fn type_to_json_schema(ty: &Type, resolve_named: &dyn Fn(&str) -> Option<J>) -> (J, bool) {
    match ty {
        Type::Number => (json!({"type": "number"}), true),
        Type::String => (json!({"type": "string"}), true),
        Type::Bool => (json!({"type": "boolean"}), true),
        Type::Nil => (json!({"type": "null"}), false),
        Type::Any => (json!({}), true),
        Type::Object => (json!({"type": "object"}), true),
        Type::Error => (json!({"type": ["object", "null"]}), false),
        Type::Fn => (json!({"type": "string"}), true),
        Type::Array(inner) => {
            let (items, _) = type_to_json_schema(inner, resolve_named);
            (json!({"type": "array", "items": items}), true)
        }
        Type::Map(_k, v) => {
            let (vals, _) = type_to_json_schema(v, resolve_named);
            (json!({"type": "object", "additionalProperties": vals}), true)
        }
        Type::Result(inner) => {
            let (val, _) = type_to_json_schema(inner, resolve_named);
            (
                json!({"type": "array", "items": [val, {"type": ["object", "null"]}]}),
                true,
            )
        }
        Type::Tuple(items) => {
            let arr: Vec<J> = items
                .iter()
                .map(|t| type_to_json_schema(t, resolve_named).0)
                .collect();
            (json!({"type": "array", "items": arr}), true)
        }
        Type::Future(inner) => type_to_json_schema(inner, resolve_named),
        Type::Optional(inner) => {
            let (mut schema, _) = type_to_json_schema(inner, resolve_named);
            make_nullable(&mut schema);
            (schema, false)
        }
        Type::Union(a, b) => {
            let nullable_a = matches!(**a, Type::Nil);
            let nullable_b = matches!(**b, Type::Nil);
            if nullable_b {
                let (mut s, _) = type_to_json_schema(a, resolve_named);
                make_nullable(&mut s);
                (s, false)
            } else if nullable_a {
                let (mut s, _) = type_to_json_schema(b, resolve_named);
                make_nullable(&mut s);
                (s, false)
            } else {
                let (sa, _) = type_to_json_schema(a, resolve_named);
                let (sb, _) = type_to_json_schema(b, resolve_named);
                (json!({"anyOf": [sa, sb]}), true)
            }
        }
        Type::Named(name) => match resolve_named(name) {
            Some(schema) => (schema, true),
            None => (json!({"type": "object"}), true),
        },
    }
}

/// Make a JSON-schema fragment accept `null` in addition to its declared type.
fn make_nullable(schema: &mut J) {
    if let J::Object(map) = schema {
        match map.get("type").cloned() {
            Some(J::String(t)) => {
                map.insert("type".to_string(), json!([t, "null"]));
            }
            Some(J::Array(mut arr)) => {
                if !arr.iter().any(|v| v.as_str() == Some("null")) {
                    arr.push(J::String("null".to_string()));
                }
                map.insert("type".to_string(), J::Array(arr));
            }
            _ => {
                let inner = schema.clone();
                *schema = json!({"anyOf": [inner, {"type": "null"}]});
            }
        }
    }
}

/// Project a `std/schema` tagged Object into a JSON Schema. Unknown / malformed
/// schema objects yield an open `{}` (accept-anything) rather than erroring, since
/// the decode step re-validates.
pub fn schema_value_to_json_schema(schema: &Value) -> J {
    let kind = match schema {
        Value::Object(o) => match o.borrow().get("__kind") {
            Some(Value::Str(s)) => s.to_string(),
            _ => return json!({}),
        },
        _ => return json!({}),
    };
    match kind.as_str() {
        "string" => {
            let mut m = Map::new();
            m.insert("type".to_string(), J::String("string".to_string()));
            if let Some(Value::Number(n)) = field(schema, "minLength") {
                m.insert("minLength".to_string(), json!(n as i64));
            }
            if let Some(Value::Number(n)) = field(schema, "maxLength") {
                m.insert("maxLength".to_string(), json!(n as i64));
            }
            if let Some(Value::Str(p)) = field(schema, "pattern") {
                m.insert("pattern".to_string(), J::String(p.to_string()));
            }
            J::Object(m)
        }
        "number" => {
            let mut m = Map::new();
            m.insert("type".to_string(), J::String("number".to_string()));
            if let Some(Value::Number(n)) = field(schema, "min") {
                m.insert("minimum".to_string(), json!(n));
            }
            if let Some(Value::Number(n)) = field(schema, "max") {
                m.insert("maximum".to_string(), json!(n));
            }
            J::Object(m)
        }
        "bool" => json!({"type": "boolean"}),
        "nil" => json!({"type": "null"}),
        "any" => json!({}),
        "literal" => {
            let v = field(schema, "value").unwrap_or(Value::Nil);
            json!({ "const": value_to_json(&v) })
        }
        "oneOf" => {
            let vals = match field(schema, "values") {
                Some(Value::Array(a)) => a.borrow().iter().map(value_to_json).collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            json!({ "enum": vals })
        }
        "array" => {
            let elem = field(schema, "elem").map(|e| schema_value_to_json_schema(&e));
            json!({ "type": "array", "items": elem.unwrap_or(json!({})) })
        }
        "map" => {
            let val = field(schema, "val").map(|v| schema_value_to_json_schema(&v));
            json!({ "type": "object", "additionalProperties": val.unwrap_or(json!({})) })
        }
        "object" => {
            let mut properties = Map::new();
            let mut required: Vec<J> = Vec::new();
            if let Some(Value::Object(fields)) = field(schema, "fields") {
                for (name, fschema) in fields.borrow().iter() {
                    let js = schema_value_to_json_schema(fschema);
                    let optional = schema_is_optional(fschema);
                    properties.insert(name.clone(), js);
                    if !optional {
                        required.push(J::String(name.clone()));
                    }
                }
            }
            let strict = matches!(field(schema, "strict"), Some(Value::Bool(true)));
            let mut m = Map::new();
            m.insert("type".to_string(), J::String("object".to_string()));
            m.insert("properties".to_string(), J::Object(properties));
            m.insert("required".to_string(), J::Array(required));
            // additionalProperties:false only when the schema is strict; else open.
            m.insert("additionalProperties".to_string(), J::Bool(!strict));
            J::Object(m)
        }
        "optional" => {
            let mut inner = field(schema, "inner")
                .map(|i| schema_value_to_json_schema(&i))
                .unwrap_or(json!({}));
            make_nullable(&mut inner);
            inner
        }
        "union" => {
            let opts = match field(schema, "options") {
                Some(Value::Array(a)) => a
                    .borrow()
                    .iter()
                    .map(schema_value_to_json_schema)
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            json!({ "anyOf": opts })
        }
        _ => json!({}),
    }
}

/// Is a `std/schema` value the `optional` kind (so its object field is not required)?
fn schema_is_optional(schema: &Value) -> bool {
    matches!(schema, Value::Object(o)
        if matches!(o.borrow().get("__kind"), Some(Value::Str(s)) if s.as_ref() == "optional"))
}

fn field(v: &Value, key: &str) -> Option<Value> {
    match v {
        Value::Object(o) => o.borrow().get(key).cloned(),
        _ => None,
    }
}

fn value_to_json(v: &Value) -> J {
    match v {
        Value::Nil => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Number(n) => json!(n),
        Value::Str(s) => J::String(s.to_string()),
        _ => J::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{Class, FieldSchema};
    use indexmap::IndexMap;

    fn class(name: &str, fields: Vec<(&str, Type, Option<crate::ast::Expr>)>) -> Class {
        let mut fmap: IndexMap<String, FieldSchema> = IndexMap::new();
        for (n, ty, default) in fields {
            fmap.insert(n.to_string(), FieldSchema { ty, default });
        }
        Class {
            name: name.to_string(),
            superclass: None,
            fields: fmap,
            methods: IndexMap::new(),
            static_methods: IndexMap::new(),
            def_env: crate::interp::global_env(),
            is_worker: false,
        }
    }

    fn no_resolve(_: &str) -> Option<J> {
        None
    }

    #[test]
    fn class_required_optional_default() {
        let c = class(
            "User",
            vec![
                ("id", Type::Number, None),
                ("name", Type::Optional(Box::new(Type::String)), None),
                (
                    "role",
                    Type::String,
                    Some(crate::ast::Expr {
                        kind: crate::ast::ExprKind::Str("guest".to_string()),
                        span: crate::span::Span::new(0, 0),
                    }),
                ),
            ],
        );
        let js = class_to_json_schema(&c, &no_resolve);
        assert_eq!(js["type"], "object");
        assert_eq!(js["properties"]["id"]["type"], "number");
        // name: nullable string, not required
        assert_eq!(js["properties"]["name"]["type"], json!(["string", "null"]));
        // role has a default → not required
        let req: Vec<&str> = js["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(req, vec!["id"]);
        assert_eq!(js["additionalProperties"], json!(false));
    }

    #[test]
    fn class_array_and_map_and_nested() {
        let c = class(
            "Doc",
            vec![
                ("tags", Type::Array(Box::new(Type::String)), None),
                (
                    "counts",
                    Type::Map(Box::new(Type::String), Box::new(Type::Number)),
                    None,
                ),
                ("author", Type::Named("Person".into()), None),
            ],
        );
        let person = class("Person", vec![("name", Type::String, None)]);
        let person_js = class_to_json_schema(&person, &no_resolve);
        let resolve = move |name: &str| {
            if name == "Person" {
                Some(person_js.clone())
            } else {
                None
            }
        };
        let js = class_to_json_schema(&c, &resolve);
        assert_eq!(js["properties"]["tags"]["type"], "array");
        assert_eq!(js["properties"]["tags"]["items"]["type"], "string");
        assert_eq!(js["properties"]["counts"]["type"], "object");
        assert_eq!(
            js["properties"]["counts"]["additionalProperties"]["type"],
            "number"
        );
        // nested class fully projected
        assert_eq!(js["properties"]["author"]["type"], "object");
        assert_eq!(js["properties"]["author"]["properties"]["name"]["type"], "string");
    }

    fn schema_obj(kind: &str, extra: Vec<(&str, Value)>) -> Value {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("__kind".to_string(), Value::Str(kind.into()));
        for (k, v) in extra {
            m.insert(k.to_string(), v);
        }
        Value::Object(crate::value::ObjectCell::new(m))
    }

    fn obj(entries: Vec<(&str, Value)>) -> Value {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        for (k, v) in entries {
            m.insert(k.to_string(), v);
        }
        Value::Object(crate::value::ObjectCell::new(m))
    }

    fn arr(items: Vec<Value>) -> Value {
        Value::Array(crate::value::ArrayCell::new(items))
    }

    #[test]
    fn schema_string_constraints() {
        let s = schema_obj(
            "string",
            vec![
                ("minLength", Value::Number(3.0)),
                ("pattern", Value::Str("^a".into())),
            ],
        );
        let js = schema_value_to_json_schema(&s);
        assert_eq!(js["type"], "string");
        assert_eq!(js["minLength"], json!(3));
        assert_eq!(js["pattern"], "^a");
    }

    #[test]
    fn schema_number_min_max() {
        let s = schema_obj(
            "number",
            vec![("min", Value::Number(0.0)), ("max", Value::Number(10.0))],
        );
        let js = schema_value_to_json_schema(&s);
        assert_eq!(js["type"], "number");
        assert_eq!(js["minimum"], json!(0.0));
        assert_eq!(js["maximum"], json!(10.0));
    }

    #[test]
    fn schema_object_oneof_union_optional() {
        // object({ sentiment: oneOf(["pos","neg"]), score: number(), note: optional(string()) })
        let sentiment = schema_obj(
            "oneOf",
            vec![(
                "values",
                arr(vec![Value::Str("pos".into()), Value::Str("neg".into())]),
            )],
        );
        let score = schema_obj("number", vec![]);
        let note = schema_obj("optional", vec![("inner", schema_obj("string", vec![]))]);
        let fields = obj(vec![
            ("sentiment", sentiment),
            ("score", score),
            ("note", note),
        ]);
        let s = schema_obj("object", vec![("fields", fields)]);
        let js = schema_value_to_json_schema(&s);
        assert_eq!(js["type"], "object");
        assert_eq!(
            js["properties"]["sentiment"]["enum"],
            json!(["pos", "neg"])
        );
        assert_eq!(js["properties"]["score"]["type"], "number");
        // optional note → nullable, not required
        assert_eq!(js["properties"]["note"]["type"], json!(["string", "null"]));
        let req: Vec<&str> = js["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(req, vec!["sentiment", "score"]);
    }

    #[test]
    fn schema_array_and_map() {
        let arr_s = schema_obj("array", vec![("elem", schema_obj("string", vec![]))]);
        let js = schema_value_to_json_schema(&arr_s);
        assert_eq!(js["type"], "array");
        assert_eq!(js["items"]["type"], "string");

        let map_s = schema_obj(
            "map",
            vec![
                ("key", schema_obj("string", vec![])),
                ("val", schema_obj("number", vec![])),
            ],
        );
        let mjs = schema_value_to_json_schema(&map_s);
        assert_eq!(mjs["type"], "object");
        assert_eq!(mjs["additionalProperties"]["type"], "number");
    }
}
