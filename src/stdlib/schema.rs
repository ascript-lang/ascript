//! `std/schema` — composable schema validators.
//!
//! Schemas are tagged AScript Objects `{__kind: "<t>", ...}`.
//! `schema.parse(schema, value)` dispatches on `__kind` and returns a
//! Tier-1 `[value, err]` pair; err is an Object `{path, message}` on
//! failure, or `nil` on success.
//!
//! The internal parse engine is the async method `Interp::parse_value(schema,
//! value, path, coerce, span)` which returns `Result<Value, ParseFail>`.
//!
//! ## Sub-phase 6c additions
//!
//! ### Constraint refiners (chainable)
//! All refiners clone the incoming tagged schema Object and insert a constraint
//! field; the base `__kind` is preserved unchanged.
//!
//! - `schema.min(s, n)` / `schema.max(s, n)` — numeric value bounds (applied
//!   in the `"number"` arm after the base type check).
//! - `schema.minLength(s, n)` / `schema.maxLength(s, n)` — character count for
//!   strings, element count for arrays.
//! - `schema.pattern(s, regexStr)` — string must match the regex.  The
//!   constructor is always available; enforcement is `#[cfg(feature="data")]`
//!   because `regex::Regex` is gated behind that feature.  Without `data`, a
//!   stored `pattern` field causes `ParseFail::InvalidSchema` at parse time.
//! - `schema.refine(s, fn, message)` — custom async predicate stored on the
//!   schema; called via `call_value` after base validation succeeds.  A falsy
//!   return → `ParseFail::Mismatch` with the user-supplied message; a
//!   `Control::Panic` / `Control::Propagate` from the fn → `ParseFail::Control`
//!   (re-raised as-is so refine-fn panics are genuine Tier-2 panics).
//! - `schema.default(s, v)` — when the incoming value is `nil`, substitute `v`
//!   and skip all further checks (trust the stored default).
//!
//! ### Coerce option
//! `schema.parse(s, v, {coerce: true})` enables conservative coercions applied
//! **before** base-kind dispatch:
//!
//! | Input           | Target kind | Coerced to            |
//! |-----------------|-------------|---------------------- |
//! | `Str(s)`        | `"number"`  | parse as f64; if ok   |
//! | `Number(n)`     | `"string"`  | `n.to_string()`       |
//! | `Str("true")`   | `"bool"`    | `Bool(true)`          |
//! | `Str("false")`  | `"bool"`    | `Bool(false)`         |
//!
//! ### Error type
//! `SchemaErr` is replaced by `ParseFail` which adds a `Control` variant so that
//! panics originating inside a `refine` user-function propagate as real Tier-2
//! errors rather than being swallowed as validation mismatches.

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
        // ── 6c constraints / refinements / coerce ────────────────────────────
        ("min", bi("schema.min")),
        ("max", bi("schema.max")),
        ("minLength", bi("schema.minLength")),
        ("maxLength", bi("schema.maxLength")),
        // `pattern` constructor is always available; enforcement is
        // feature-gated (see parse_value "string" arm).
        ("pattern", bi("schema.pattern")),
        ("refine", bi("schema.refine")),
        ("default", bi("schema.default")),
        // ── 6d bridge ────────────────────────────────────────────────────────
        // `schema.fromClass(Class)` derives a schema from the class's declared
        // fields; uses the same Type→schema mapping as `validate_into`.
        ("fromClass", bi("schema.fromClass")),
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
/// - `Control` wraps a `Control` that emerged from a `refine` user-function call
///   (panic or propagate). The parse boundary re-raises it as-is so refine-fn
///   panics are genuine Tier-2 panics, not validation mismatches.
///
/// `pub(crate)` so that `mod.rs` can pattern-match on `ParseFail` variants when
/// bridging `json.parse(text, schema)` and `resp.json(schema)`.
#[derive(Debug)]
pub(crate) enum ParseFail {
    Mismatch(Value),
    InvalidSchema(String),
    Control(Control),
}

impl From<Control> for ParseFail {
    fn from(c: Control) -> Self {
        ParseFail::Control(c)
    }
}

// ── tagged-object helpers ─────────────────────────────────────────────────────

/// Build a schema tag object `{__kind: kind}`.
fn make_schema(kind: &str) -> Value {
    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert("__kind".to_string(), Value::Str(kind.into()));
    Value::Object(crate::value::ObjectCell::new(m))
}

/// Build a `{path, message}` error detail object for the Tier-1 err slot.
fn err_obj(path: &str, message: String) -> Value {
    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert("path".to_string(), Value::Str(path.into()));
    m.insert("message".to_string(), Value::Str(message.into()));
    Value::Object(crate::value::ObjectCell::new(m))
}

/// Extract the `__kind` field from a schema Object, or return `None` if the
/// value is not an Object or has no `__kind` string.
///
/// `pub(crate)` so that `mod.rs` can detect a schema value as the 2nd arg of
/// `json.parse(text, schema)` without importing the entire engine.
pub(crate) fn schema_kind(schema: &Value) -> Option<Rc<str>> {
    match schema {
        Value::Object(o) => match o.borrow().get("__kind") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The set of known schema `__kind` tags. Used by `is_schema_value` to keep
/// the fluent call-site hook NARROW — only objects tagged with one of these
/// kinds are treated as schema receivers, so a module namespace or an
/// unrelated user object (even one that happens to carry a `__kind` field) is
/// never hijacked.
const SCHEMA_KINDS: &[&str] = &[
    "string", "number", "bool", "nil", "any", "literal", "array", "object", "map", "optional",
    "union", "oneOf",
];

/// True iff `v` is a schema value: a `Value::Object` whose `__kind` field is a
/// String equal to one of the known schema kinds (see `SCHEMA_KINDS`).
///
/// Deliberately narrow: it MUST NOT match a stdlib module namespace object or
/// an arbitrary user object. This is the receiver test for the fluent
/// method-chaining call-site hook in `interp.rs`.
pub(crate) fn is_schema_value(v: &Value) -> bool {
    match schema_kind(v) {
        Some(k) => SCHEMA_KINDS.contains(&k.as_ref()),
        None => false,
    }
}

/// True iff `name` is a `call_schema` op whose FIRST parameter is the receiver
/// schema, i.e. one that reads naturally as a method `s.<name>(...)` →
/// `call_schema(name, [s, ...rest])`.
///
/// Included (refiners / receiver-wrapping composites / terminal):
///   `minLength`, `maxLength`, `pattern`, `min`, `max`, `refine`, `default`,
///   `optional`, `strict`, `parse`.
///
/// EXCLUDED — source constructors that do NOT take a receiver schema first and
/// stay `schema.*(...)` module functions: `string`, `number`, `bool`,
/// `nilType`, `any`, `literal`, `object`, `array`, `union`, `oneOf`,
/// `fromClass`. Also EXCLUDED is `map` — although `schema.map(key, val)` takes
/// schema args, it is a CONSTRUCTOR (builds a map schema from key+val), not a
/// refiner of a receiver, so `s.map(...)` would not read as wrapping `s`.
pub(crate) fn is_schema_method(name: &str) -> bool {
    matches!(
        name,
        "minLength"
            | "maxLength"
            | "pattern"
            | "min"
            | "max"
            | "refine"
            | "default"
            | "optional"
            | "strict"
            | "parse"
    )
}

/// Get a field from a `Value::Object`.
fn obj_field(obj: &Value, key: &str) -> Option<Value> {
    match obj {
        Value::Object(o) => o.borrow().get(key).cloned(),
        _ => None,
    }
}

// ── Type → schema conversion (used by fromClass) ─────────────────────────────

/// Convert an AScript `Type` annotation to its schema-tagged Object equivalent.
///
/// ## Coverage
/// | Type                  | Schema                           |
/// |-----------------------|----------------------------------|
/// | `number`              | `{__kind:"number"}`              |
/// | `string`              | `{__kind:"string"}`              |
/// | `bool`                | `{__kind:"bool"}`                |
/// | `nil`                 | `{__kind:"nil"}`                 |
/// | `any`                 | `{__kind:"any"}`                 |
/// | `T?` (Optional)       | `{__kind:"optional", inner:T}`   |
/// | `array<T>`            | `{__kind:"array", elem:T}`       |
/// | `map<K,V>`            | `{__kind:"map", key:K, val:V}`   |
/// | `Named(ClassName)`    | nested object schema (recurse) — see note |
/// | `Union(A,B)`          | `{__kind:"union", options:[A,B]}`|
/// | `fn`/`object`/`error` | `{__kind:"any"}` (permissive)   |
/// | `Result<T>`/`Tuple`/`Future` | `{__kind:"any"}` (permissive) |
///
/// ## Named class types — recurse, never silent accept-all
/// A `Named` type refers to a class by name (e.g. a field `addr: Address`).
/// It is resolved in `def_env` — the declaring class's definition environment,
/// the same scope `validate_into` uses for nested-class coercion. When the name
/// resolves to a `Value::Class`, we recurse via `class_to_object_schema_inner`
/// to build the nested `{__kind:"object", fields:{...}}` schema — so a nested
/// field is fully validated.
///
/// `visited` guards against self-referential / mutually-recursive classes: a
/// name already on the stack (or one that does not resolve to a class in
/// `def_env`) falls back to a **bare object schema `{__kind:"object",
/// fields:{}}`** — which accepts any *object* but **rejects non-objects /
/// primitives**. This preserves the object-shape requirement (a non-object
/// nested value is a Tier-1 error) without infinite recursion and without ever
/// silently accepting non-object values.
fn type_to_schema(
    ty: &crate::ast::Type,
    def_env: &crate::env::Environment,
    visited: &mut std::collections::HashSet<String>,
) -> Value {
    use crate::ast::Type;
    match ty {
        Type::Number => make_schema("number"),
        Type::String => make_schema("string"),
        Type::Bool => make_schema("bool"),
        Type::Nil => make_schema("nil"),
        // any, fn, object, error — accept-all
        Type::Any | Type::Fn | Type::Object | Type::Error => make_schema("any"),
        // T? → {__kind:"optional", inner: type_to_schema(T)}
        Type::Optional(inner) => {
            let inner_schema = type_to_schema(inner, def_env, visited);
            let mut m: IndexMap<String, Value> = IndexMap::new();
            m.insert("__kind".to_string(), Value::Str("optional".into()));
            m.insert("inner".to_string(), inner_schema);
            Value::Object(crate::value::ObjectCell::new(m))
        }
        // array<T> → {__kind:"array", elem: type_to_schema(T)}
        Type::Array(elem) => {
            let elem_schema = type_to_schema(elem, def_env, visited);
            let mut m: IndexMap<String, Value> = IndexMap::new();
            m.insert("__kind".to_string(), Value::Str("array".into()));
            m.insert("elem".to_string(), elem_schema);
            Value::Object(crate::value::ObjectCell::new(m))
        }
        // map<K,V> → {__kind:"map", key: type_to_schema(K), val: type_to_schema(V)}
        Type::Map(k, v) => {
            let key_schema = type_to_schema(k, def_env, visited);
            let val_schema = type_to_schema(v, def_env, visited);
            let mut m: IndexMap<String, Value> = IndexMap::new();
            m.insert("__kind".to_string(), Value::Str("map".into()));
            m.insert("key".to_string(), key_schema);
            m.insert("val".to_string(), val_schema);
            Value::Object(crate::value::ObjectCell::new(m))
        }
        // Union(A, B) → {__kind:"union", options:[schema_A, schema_B]}
        // Flattened so nested Union(Union(A,B),C) → options:[A,B,C].
        Type::Union(a, b) => {
            fn collect_union_arms(
                t: &crate::ast::Type,
                def_env: &crate::env::Environment,
                visited: &mut std::collections::HashSet<String>,
                out: &mut Vec<Value>,
            ) {
                if let crate::ast::Type::Union(a, b) = t {
                    collect_union_arms(a, def_env, visited, out);
                    collect_union_arms(b, def_env, visited, out);
                } else {
                    out.push(type_to_schema(t, def_env, visited));
                }
            }
            let mut opts: Vec<Value> = Vec::new();
            collect_union_arms(
                &Type::Union(a.clone(), b.clone()),
                def_env,
                visited,
                &mut opts,
            );
            let mut m: IndexMap<String, Value> = IndexMap::new();
            m.insert("__kind".to_string(), Value::Str("union".into()));
            m.insert(
                "options".to_string(),
                Value::Array(Rc::new(RefCell::new(opts))),
            );
            Value::Object(crate::value::ObjectCell::new(m))
        }
        // Named class type: a field declared `fieldName: SomeClass`. Resolve the
        // class in `def_env` and recurse to a nested object schema. On a cycle
        // (name already being expanded) or an unresolvable name, fall back to a
        // bare object schema — accepts any object, rejects primitives — NEVER
        // `any()` (see fn-level doc).
        Type::Named(name) => match def_env.get(name) {
            Some(Value::Class(c)) if !visited.contains(name) => {
                visited.insert(name.clone());
                let nested = class_to_object_schema_inner(&c, visited);
                visited.remove(name); // siblings of the same type still resolve
                nested
            }
            // Cycle (already in `visited`) or not a class in scope → object-shape
            // requirement only (rejects non-objects), never silent accept-all.
            _ => {
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("object".into()));
                m.insert(
                    "fields".to_string(),
                    Value::Object(crate::value::ObjectCell::new(IndexMap::new())),
                );
                m.insert("strict".to_string(), Value::Bool(false));
                Value::Object(crate::value::ObjectCell::new(m))
            }
        },
        // Result<T>, Tuple, Future — accept-all (no clean schema mapping)
        Type::Result(_) | Type::Tuple(_) | Type::Future(_) => make_schema("any"),
    }
}

/// Build an `{__kind:"object", fields:{...}, strict:false}` schema from a
/// class's merged field schema. Each declared field is converted via
/// `type_to_schema`, resolving nested class names in that field's *declaring*
/// class def env (the same scoping `validate_into` uses), with `visited`
/// carried through to guard against recursive class graphs.
fn class_to_object_schema_inner(
    class: &std::rc::Rc<crate::value::Class>,
    visited: &mut std::collections::HashSet<String>,
) -> Value {
    use crate::value::merged_field_schema;
    let fields_map = merged_field_schema(class);

    let mut fields: IndexMap<String, Value> = IndexMap::new();
    for (name, (fs, defining_class)) in &fields_map {
        // Nested class names resolve in the env of the class that DECLARED the
        // field (matches validate_into's per-field def_class.def_env scoping).
        fields.insert(
            name.clone(),
            type_to_schema(&fs.ty, &defining_class.def_env, visited),
        );
    }

    let fields_obj = Value::Object(crate::value::ObjectCell::new(fields));
    let mut m: IndexMap<String, Value> = IndexMap::new();
    m.insert("__kind".to_string(), Value::Str("object".into()));
    m.insert("fields".to_string(), fields_obj);
    m.insert("strict".to_string(), Value::Bool(false));
    Value::Object(crate::value::ObjectCell::new(m))
}

/// Build an object schema from a class's merged field schema (the `fromClass`
/// entry point). Seeds the cycle-guard with the class's own name so a directly
/// self-referential field falls back to the bare-object schema rather than
/// recursing forever.
fn class_to_object_schema(class: &std::rc::Rc<crate::value::Class>) -> Value {
    let mut visited = std::collections::HashSet::new();
    visited.insert(class.name.clone());
    class_to_object_schema_inner(class, &mut visited)
}

// ── Interp dispatch + parse engine (async, on &self for 6b/6c) ──────────────────

impl Interp {
    /// The recursive parse engine.  Accepts the schema node, the candidate
    /// value, the current dot-path string (empty at top level), a `coerce`
    /// flag (enables conservative type coercions before kind dispatch), and
    /// the call `span` (used by `call_value` for `refine` fn invocations).
    ///
    /// Returns `Ok(coerced_value)` on success or `Err(ParseFail)` on mismatch,
    /// malformed schema, or a panic/propagate from a `refine` user-function.
    ///
    /// ## Error variants
    /// - `ParseFail::Mismatch(errObj)` — Tier-1 validation failure; the caller
    ///   surfaces it as the `err` slot of `[nil, err]`.
    /// - `ParseFail::InvalidSchema(msg)` — Tier-2 programmer error (malformed
    ///   schema node); the caller escalates it to `Control::Panic`.
    /// - `ParseFail::Control(c)` — a `Control::Panic` / `Control::Propagate`
    ///   that emerged inside a `refine` user-function call; re-raised unchanged.
    ///
    /// ## Invariant
    /// Never hold a `RefCell` borrow across an `.await`.  The primitive arms
    /// take no borrow across a yield; the `refine` arm clones the fn value out
    /// before awaiting `call_value`.
    #[async_recursion::async_recursion(?Send)]
    pub(crate) async fn parse_value(
        &self,
        schema: &Value,
        value: &Value,
        path: &str,
        coerce: bool,
        span: Span,
    ) -> Result<Value, ParseFail> {
        let kind = match schema_kind(schema) {
            Some(k) => k,
            // Not a schema object (no __kind) → Tier-2 (escalated to a panic by
            // the caller), never a silent validation failure.
            None => {
                return Err(ParseFail::InvalidSchema(format!(
                    "schema.parse: not a valid schema object (missing __kind){}",
                    if path.is_empty() {
                        String::new()
                    } else {
                        format!(" at '{}'", path)
                    }
                )))
            }
        };

        // ── default: substitute when value is nil and schema has a default ────
        // Applied BEFORE coerce and kind dispatch.  We trust the stored default
        // (no further validation) and return it immediately.
        if matches!(value, Value::Nil) {
            if let Some(default_val) = obj_field(schema, "default") {
                return Ok(default_val);
            }
        }

        // ── coerce: conservative value coercions before kind dispatch ─────────
        // Each coercion is attempted conservatively: if it succeeds, replace
        // `value` for the rest of validation; if it fails (e.g. a non-numeric
        // string → number), fall through to the normal check which produces a
        // Mismatch.
        //
        // Coercion table (only applied when `coerce` is true):
        //   Str(s)       → "number" : parse s as f64; if ok → Number(n)
        //   Number(n)    → "string" : n.to_string()           → Str
        //   Str("true")  → "bool"   : Bool(true)
        //   Str("false") → "bool"   : Bool(false)
        let coerced: Option<Value> = if coerce {
            match (kind.as_ref(), value) {
                ("number", Value::Str(s)) => s.parse::<f64>().ok().map(Value::Number),
                ("string", Value::Number(n)) => {
                    Some(Value::Str(Value::Number(*n).to_string().into()))
                }
                ("bool", Value::Str(s)) if s.as_ref() == "true" => Some(Value::Bool(true)),
                ("bool", Value::Str(s)) if s.as_ref() == "false" => Some(Value::Bool(false)),
                _ => None,
            }
        } else {
            None
        };
        let value: &Value = coerced.as_ref().unwrap_or(value);

        // ── dispatch on base kind, collecting the validated value ─────────────
        //
        // Primitive arms ("string", "number", "bool", "nil", "any", "literal")
        // produce a `Value` on success, or `return Err(...)` early on failure.
        // Constraint checks (min, max, minLength, maxLength, pattern) are
        // inlined in the relevant primitive arms.
        //
        // Composite arms (array, object, map, optional, union, oneOf) handle
        // their own recursion and `return Ok(...)` / `return Err(...)` directly,
        // so they bypass the refine step below.  Refine on composite schemas is
        // not tested; add it here if needed in a later sub-phase.
        let validated: Value = match kind.as_ref() {
            // ── "string" ──────────────────────────────────────────────────────
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
                        //
                        // The `schema.pattern` constructor is always available; only
                        // the regex enforcement is feature-gated.  Without `data`,
                        // a stored pattern field triggers InvalidSchema so misuse is
                        // caught at parse time rather than silently ignored.
                        #[cfg(feature = "data")]
                        if let Some(Value::Str(pat)) = obj_field(schema, "pattern") {
                            match regex::Regex::new(&pat) {
                                Ok(re) => {
                                    if !re.is_match(s) {
                                        return Err(ParseFail::Mismatch(err_obj(
                                            path,
                                            format!(
                                                "expected string matching pattern /{}/, got \"{}\"",
                                                pat, s
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
                                "schema.pattern requires the 'data' feature to enforce; \
                                 rebuild with `--features data` (or use the default feature set)"
                                    .into(),
                            ));
                        }
                        value.clone()
                    }
                    _ => {
                        return Err(ParseFail::Mismatch(err_obj(
                            path,
                            format!("expected string, got {}", type_name(value)),
                        )))
                    }
                }
            }

            // ── "number" ──────────────────────────────────────────────────────
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
                        value.clone()
                    }
                    _ => {
                        return Err(ParseFail::Mismatch(err_obj(
                            path,
                            format!("expected number, got {}", type_name(value)),
                        )))
                    }
                }
            }

            // ── "bool" ────────────────────────────────────────────────────────
            "bool" => {
                if matches!(value, Value::Bool(_)) {
                    value.clone()
                } else {
                    return Err(ParseFail::Mismatch(err_obj(
                        path,
                        format!("expected bool, got {}", type_name(value)),
                    )));
                }
            }

            // ── "nil" ─────────────────────────────────────────────────────────
            "nil" => {
                if matches!(value, Value::Nil) {
                    value.clone()
                } else {
                    return Err(ParseFail::Mismatch(err_obj(
                        path,
                        format!("expected nil, got {}", type_name(value)),
                    )));
                }
            }

            // ── "any" ─────────────────────────────────────────────────────────
            "any" => value.clone(),

            // ── "literal" ─────────────────────────────────────────────────────
            "literal" => {
                let expected = obj_field(schema, "value").unwrap_or(Value::Nil);
                if value == &expected {
                    value.clone()
                } else {
                    return Err(ParseFail::Mismatch(err_obj(
                        path,
                        format!("expected literal {}, got {}", expected, value),
                    )));
                }
            }

            // ── 6b: array ─────────────────────────────────────────────────────
            "array" => {
                let elem_schema = obj_field(schema, "elem").ok_or_else(|| {
                    ParseFail::InvalidSchema("schema.parse: 'array' schema missing 'elem'".into())
                })?;
                match value {
                    Value::Array(arr) => {
                        let items: Vec<Value> = arr.borrow().clone();
                        let mut out = Vec::with_capacity(items.len());
                        for (i, item) in items.iter().enumerate() {
                            let item_path = format!("{}[{}]", path, i);
                            let v = self
                                .parse_value(&elem_schema, item, &item_path, coerce, span)
                                .await?;
                            out.push(v);
                        }
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
                        return Ok(Value::Array(Rc::new(RefCell::new(out))));
                    }
                    _ => {
                        return Err(ParseFail::Mismatch(err_obj(
                            path,
                            format!("expected array, got {}", type_name(value)),
                        )))
                    }
                }
            }

            // ── 6b: object ────────────────────────────────────────────────────
            "object" => {
                let fields_schema = obj_field(schema, "fields").ok_or_else(|| {
                    ParseFail::InvalidSchema(
                        "schema.parse: 'object' schema missing 'fields'".into(),
                    )
                })?;
                let is_strict = matches!(obj_field(schema, "strict"), Some(Value::Bool(true)));

                match value {
                    Value::Object(val_obj) => {
                        // Collect declared field names and schemas
                        let field_pairs: Vec<(String, Value)> = match &fields_schema {
                            Value::Object(fs) => fs
                                .borrow()
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect(),
                            _ => {
                                return Err(ParseFail::InvalidSchema(
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
                                    return Err(ParseFail::Mismatch(err_obj(
                                        &key_path,
                                        format!("unknown key '{}' not allowed in strict object", k),
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
                                .parse_value(field_schema, &field_val, &field_path, coerce, span)
                                .await?;
                            out.insert(field_name.clone(), parsed);
                        }
                        return Ok(Value::Object(crate::value::ObjectCell::new(out)));
                    }
                    _ => {
                        return Err(ParseFail::Mismatch(err_obj(
                            path,
                            format!("expected object, got {}", type_name(value)),
                        )))
                    }
                }
            }

            // ── 6b: map ───────────────────────────────────────────────────────
            "map" => {
                use crate::value::MapKey;
                let key_schema = obj_field(schema, "key").ok_or_else(|| {
                    ParseFail::InvalidSchema("schema.parse: 'map' schema missing 'key'".into())
                })?;
                let val_schema = obj_field(schema, "val").ok_or_else(|| {
                    ParseFail::InvalidSchema("schema.parse: 'map' schema missing 'val'".into())
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
                        return Err(ParseFail::Mismatch(err_obj(
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
                    let parsed_key = self
                        .parse_value(&key_schema, &raw_key, &key_path, coerce, span)
                        .await?;
                    let parsed_val = self
                        .parse_value(&val_schema, &raw_val, &val_path, coerce, span)
                        .await?;
                    let map_key = MapKey::from_value(&parsed_key).ok_or_else(|| {
                        ParseFail::Mismatch(err_obj(
                            &key_path,
                            format!("map key type {} is not hashable", type_name(&parsed_key)),
                        ))
                    })?;
                    out.insert(map_key, parsed_val);
                }
                return Ok(Value::Map(Rc::new(RefCell::new(out))));
            }

            // ── 6b: optional ──────────────────────────────────────────────────
            "optional" => {
                if matches!(value, Value::Nil) {
                    return Ok(Value::Nil);
                }
                let inner = obj_field(schema, "inner").ok_or_else(|| {
                    ParseFail::InvalidSchema(
                        "schema.parse: 'optional' schema missing 'inner'".into(),
                    )
                })?;
                return self.parse_value(&inner, value, path, coerce, span).await;
            }

            // ── 6b: union ─────────────────────────────────────────────────────
            "union" => {
                let options = obj_field(schema, "options").ok_or_else(|| {
                    ParseFail::InvalidSchema(
                        "schema.parse: 'union' schema missing 'options'".into(),
                    )
                })?;
                let opts: Vec<Value> = match &options {
                    Value::Array(a) => a.borrow().clone(),
                    _ => {
                        return Err(ParseFail::InvalidSchema(
                            "schema.parse: 'union' options must be an Array".into(),
                        ))
                    }
                };
                let mut kinds: Vec<String> = Vec::new();
                for opt in &opts {
                    match self.parse_value(opt, value, path, coerce, span).await {
                        Ok(v) => return Ok(v),
                        Err(ParseFail::Mismatch(_)) => {
                            kinds.push(
                                schema_kind(opt)
                                    .map(|k| k.to_string())
                                    .unwrap_or_else(|| "?".into()),
                            );
                        }
                        Err(e @ ParseFail::InvalidSchema(_)) => return Err(e),
                        Err(e @ ParseFail::Control(_)) => return Err(e),
                    }
                }
                return Err(ParseFail::Mismatch(err_obj(
                    path,
                    format!(
                        "expected one of [{}], got {}",
                        kinds.join(", "),
                        type_name(value)
                    ),
                )));
            }

            // ── 6b: oneOf (enum-like, `enum` is a keyword) ───────────────────
            "oneOf" => {
                let values_field = obj_field(schema, "values").ok_or_else(|| {
                    ParseFail::InvalidSchema("schema.parse: 'oneOf' schema missing 'values'".into())
                })?;
                let allowed: Vec<Value> = match &values_field {
                    Value::Array(a) => a.borrow().clone(),
                    _ => {
                        return Err(ParseFail::InvalidSchema(
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
                return Err(ParseFail::Mismatch(err_obj(
                    path,
                    format!("expected one of [{}], got {}", listed.join(", "), value),
                )));
            }

            other => {
                return Err(ParseFail::InvalidSchema(format!(
                    "schema.parse: unknown schema kind '{}'",
                    other
                )))
            }
        };
        // ── reached only for primitive arms (string/number/bool/nil/any/literal)
        // after the base kind check has passed and constraints have been applied.

        // ── refine: invoke user predicate ─────────────────────────────────────
        // Clone the fn and the validated value out before awaiting — no
        // RefCell borrow held across the .await.
        if let Some(refine_fn) = obj_field(schema, "refine") {
            let ok = self
                .call_value(refine_fn, vec![validated.clone()], span)
                .await
                .map_err(ParseFail::Control)?;
            if !ok.is_truthy() {
                let msg = match obj_field(schema, "refineMessage") {
                    Some(Value::Str(s)) => s.to_string(),
                    _ => "value failed refinement check".to_string(),
                };
                return Err(ParseFail::Mismatch(err_obj(path, msg)));
            }
        }

        Ok(validated)
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
                Ok(Value::Object(crate::value::ObjectCell::new(m)))
            }

            // ── 6b composite constructors ─────────────────────────────────────

            // schema.array(elemSchema) → {__kind:"array", elem}
            "array" => {
                let elem = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("array".into()));
                m.insert("elem".to_string(), elem);
                Ok(Value::Object(crate::value::ObjectCell::new(m)))
            }

            // schema.object(fieldsObj) → {__kind:"object", fields, strict:false}
            "object" => {
                let fields = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("object".into()));
                m.insert("fields".to_string(), fields);
                m.insert("strict".to_string(), Value::Bool(false));
                Ok(Value::Object(crate::value::ObjectCell::new(m)))
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
                                Ok(Value::Object(crate::value::ObjectCell::new(m)))
                            }
                            _ => unreachable!(),
                        }
                    }
                    _ => Err(
                        AsError::at("schema.strict: argument must be an object schema", span)
                            .into(),
                    ),
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
                Ok(Value::Object(crate::value::ObjectCell::new(m)))
            }

            // schema.optional(innerSchema) → {__kind:"optional", inner}
            "optional" => {
                let inner = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("optional".into()));
                m.insert("inner".to_string(), inner);
                Ok(Value::Object(crate::value::ObjectCell::new(m)))
            }

            // schema.union(list) → {__kind:"union", options:[...]}
            "union" => {
                let options = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("union".into()));
                m.insert("options".to_string(), options);
                Ok(Value::Object(crate::value::ObjectCell::new(m)))
            }

            // schema.oneOf(list) → {__kind:"oneOf", values:[...]}
            // (`enum` is a reserved keyword in AScript, so `oneOf` is the exported name.)
            "oneOf" => {
                let values = arg(args, 0);
                let mut m: IndexMap<String, Value> = IndexMap::new();
                m.insert("__kind".to_string(), Value::Str("oneOf".into()));
                m.insert("values".to_string(), values);
                Ok(Value::Object(crate::value::ObjectCell::new(m)))
            }

            // ── 6c constraint refiners ────────────────────────────────────────
            // All refiners clone the incoming tagged schema Object and insert a
            // constraint field.  The base `__kind` is unchanged.

            // schema.min(s, n) → clone schema + {min: n}
            "min" => {
                let s = arg(args, 0);
                let n = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("min".to_string(), n);
                        Ok(Value::Object(crate::value::ObjectCell::new(m)))
                    }
                    _ => Err(AsError::at(
                        "schema.min: first argument must be a schema object",
                        span,
                    )
                    .into()),
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
                        Ok(Value::Object(crate::value::ObjectCell::new(m)))
                    }
                    _ => Err(AsError::at(
                        "schema.max: first argument must be a schema object",
                        span,
                    )
                    .into()),
                }
            }

            // schema.minLength(s, n) → clone schema + {minLength: n}
            "minLength" => {
                let s = arg(args, 0);
                let n = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("minLength".to_string(), n);
                        Ok(Value::Object(crate::value::ObjectCell::new(m)))
                    }
                    _ => Err(AsError::at(
                        "schema.minLength: first argument must be a schema object",
                        span,
                    )
                    .into()),
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
                        Ok(Value::Object(crate::value::ObjectCell::new(m)))
                    }
                    _ => Err(AsError::at(
                        "schema.maxLength: first argument must be a schema object",
                        span,
                    )
                    .into()),
                }
            }

            // schema.pattern(s, regexStr) → clone schema + {pattern: regexStr}
            //
            // The constructor is unconditionally available; enforcement is gated
            // on `#[cfg(feature="data")]` in parse_value because `regex::Regex`
            // only exists with that feature.
            "pattern" => {
                let s = arg(args, 0);
                let pat = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("pattern".to_string(), pat);
                        Ok(Value::Object(crate::value::ObjectCell::new(m)))
                    }
                    _ => Err(AsError::at(
                        "schema.pattern: first argument must be a schema object",
                        span,
                    )
                    .into()),
                }
            }

            // schema.refine(s, fn, message) → clone schema + {refine: fn, refineMessage: msg}
            //
            // The user fn is stored as a `Value`.  It is called via
            // `call_value` in parse_value after the base kind check succeeds.
            // A falsy return → Mismatch with `message`.
            // A Control::Panic / Control::Propagate from the fn → ParseFail::Control
            // → re-raised at the parse boundary as a genuine Tier-2 panic.
            "refine" => {
                let s = arg(args, 0);
                let f = arg(args, 1);
                let msg = arg(args, 2);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("refine".to_string(), f);
                        m.insert("refineMessage".to_string(), msg);
                        Ok(Value::Object(crate::value::ObjectCell::new(m)))
                    }
                    _ => Err(AsError::at(
                        "schema.refine: first argument must be a schema object",
                        span,
                    )
                    .into()),
                }
            }

            // schema.default(s, value) → clone schema + {default: value}
            //
            // When the incoming value is nil and the schema has a `default`,
            // the default is substituted and all further checks are skipped.
            "default" => {
                let s = arg(args, 0);
                let v = arg(args, 1);
                match &s {
                    Value::Object(o) => {
                        let mut m = o.borrow().clone();
                        m.insert("default".to_string(), v);
                        Ok(Value::Object(crate::value::ObjectCell::new(m)))
                    }
                    _ => Err(AsError::at(
                        "schema.default: first argument must be a schema object",
                        span,
                    )
                    .into()),
                }
            }

            // ── schema.parse(schema, value[, options]) → [value, err] ─────────
            "parse" => {
                let schema = arg(args, 0);
                let value = arg(args, 1);
                // Optional third arg: options object with `coerce` field.
                // `{coerce: true}` enables conservative coercions before the
                // base-kind check (see parse_value coercion table in module doc).
                let coerce = match args.get(2) {
                    Some(Value::Object(o)) => {
                        matches!(o.borrow().get("coerce"), Some(Value::Bool(true)))
                    }
                    _ => false,
                };

                match self.parse_value(&schema, &value, "", coerce, span).await {
                    Ok(v) => Ok(make_pair(v, Value::Nil)),
                    // Tier-1 validation failure → [nil, errObj].
                    Err(ParseFail::Mismatch(err)) => Ok(make_pair(Value::Nil, err)),
                    // Tier-2 programmer error (malformed schema) → panic.
                    Err(ParseFail::InvalidSchema(msg)) => Err(AsError::at(msg, span).into()),
                    // A panic/propagate from inside a refine fn — re-raise unchanged.
                    Err(ParseFail::Control(c)) => Err(c),
                }
            }

            // ── 6d: schema.fromClass(Class) → object schema ───────────────────
            //
            // Derives a `{__kind:"object", fields:{...}, strict:false}` schema
            // from the class's declared fields, recursively via `type_to_schema`.
            //
            // Type→schema mapping:
            //   number → schema.number()
            //   string → schema.string()
            //   bool   → schema.bool()
            //   nil    → schema.nilType()
            //   any/fn/object/error → schema.any()
            //   T?     → schema.optional(inner)
            //   array<T> → schema.array(inner)
            //   map<K,V> → schema.map(key, val)
            //   Union(A,B) → schema.union([A,B]) (flattened)
            //   Named(ClassName) → nested object schema (recurses via def_env;
            //                       cycle/unresolved → bare object schema, never any())
            //   Result/Tuple/Future → schema.any()
            //
            // Misuse (non-class argument) → Tier-2 panic.
            "fromClass" => match arg(args, 0) {
                Value::Class(c) => Ok(class_to_object_schema(&c)),
                other => Err(AsError::at(
                    format!(
                        "schema.fromClass: expected a class, got {}",
                        crate::interp::type_name(&other)
                    ),
                    span,
                )
                .into()),
            },

            _ => Err(AsError::at(format!("std/schema has no function '{}'", func), span).into()),
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

    // ── fluent hook helper tests ──────────────────────────────────────────────

    #[test]
    fn is_schema_value_matches_known_kinds_only() {
        // Every known kind is a schema value.
        for k in SCHEMA_KINDS {
            assert!(is_schema_value(&make_schema(k)), "kind {k} should match");
        }
        // An object tagged with a NON-schema __kind must NOT match (narrowness).
        let bogus = {
            let mut m: IndexMap<String, Value> = IndexMap::new();
            m.insert("__kind".to_string(), Value::Str("widget".into()));
            Value::Object(crate::value::ObjectCell::new(m))
        };
        assert!(!is_schema_value(&bogus));
        // An object with no __kind, and non-object values, must NOT match.
        let plain = Value::Object(crate::value::ObjectCell::new(IndexMap::new()));
        assert!(!is_schema_value(&plain));
        assert!(!is_schema_value(&Value::Number(1.0)));
        assert!(!is_schema_value(&Value::Str("string".into())));
        assert!(!is_schema_value(&Value::Nil));
    }

    #[test]
    fn is_schema_method_set() {
        for m in [
            "minLength",
            "maxLength",
            "pattern",
            "min",
            "max",
            "refine",
            "default",
            "optional",
            "strict",
            "parse",
        ] {
            assert!(is_schema_method(m), "{m} should be a schema method");
        }
        // Source constructors are NOT methods.
        for c in [
            "string",
            "number",
            "bool",
            "nilType",
            "any",
            "literal",
            "object",
            "array",
            "union",
            "oneOf",
            "map",
            "fromClass",
        ] {
            assert!(!is_schema_method(c), "{c} must NOT be a schema method");
        }
    }

    // ── constructor smoke tests ───────────────────────────────────────────────

    #[test]
    fn constructor_string_kind() {
        let s = make_schema("string");
        assert_eq!(schema_kind(&s).as_deref(), Some("string"));
    }

    #[test]
    fn constructor_literal_value() {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("__kind".to_string(), Value::Str("literal".into()));
        m.insert("value".to_string(), Value::Number(5.0));
        let lit = Value::Object(crate::value::ObjectCell::new(m));
        assert_eq!(schema_kind(&lit).as_deref(), Some("literal"));
        assert_eq!(obj_field(&lit, "value"), Some(Value::Number(5.0)));
    }

    /// Unwrap a `ParseFail::Mismatch` err Object; panic on other variants.
    fn mismatch(e: ParseFail) -> Value {
        match e {
            ParseFail::Mismatch(v) => v,
            ParseFail::InvalidSchema(m) => panic!("expected Mismatch, got InvalidSchema: {}", m),
            ParseFail::Control(c) => panic!("expected Mismatch, got Control: {:?}", c),
        }
    }

    // ── parse_value: success cases ────────────────────────────────────────────

    #[tokio::test]
    async fn parse_string_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("string");
        let v = Value::Str("hi".into());
        let result = interp.parse_value(&schema, &v, "", false, sp()).await;
        assert_eq!(result.unwrap(), Value::Str("hi".into()));
    }

    #[tokio::test]
    async fn parse_number_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("number");
        let v = Value::Number(42.0);
        let result = interp.parse_value(&schema, &v, "", false, sp()).await;
        assert_eq!(result.unwrap(), Value::Number(42.0));
    }

    #[tokio::test]
    async fn parse_bool_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("bool");
        let v = Value::Bool(true);
        let result = interp.parse_value(&schema, &v, "", false, sp()).await;
        assert_eq!(result.unwrap(), Value::Bool(true));
    }

    #[tokio::test]
    async fn parse_nil_ok() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("nil");
        let result = interp
            .parse_value(&schema, &Value::Nil, "", false, sp())
            .await;
        assert_eq!(result.unwrap(), Value::Nil);
    }

    #[tokio::test]
    async fn parse_any_passes_everything() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("any");
        assert_eq!(
            interp
                .parse_value(&schema, &Value::Number(1.0), "", false, sp())
                .await
                .unwrap(),
            Value::Number(1.0)
        );
        assert_eq!(
            interp
                .parse_value(&schema, &Value::Str("x".into()), "", false, sp())
                .await
                .unwrap(),
            Value::Str("x".into())
        );
        assert_eq!(
            interp
                .parse_value(&schema, &Value::Nil, "", false, sp())
                .await
                .unwrap(),
            Value::Nil
        );
    }

    #[tokio::test]
    async fn parse_literal_ok() {
        let interp = crate::interp::Interp::new();
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("__kind".to_string(), Value::Str("literal".into()));
        m.insert("value".to_string(), Value::Number(5.0));
        let lit = Value::Object(crate::value::ObjectCell::new(m));
        let result = interp
            .parse_value(&lit, &Value::Number(5.0), "", false, sp())
            .await;
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
                .parse_value(&schema, &Value::Number(1.0), "", false, sp())
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
        let ok = interp
            .parse_value(&schema, &v, "", false, sp())
            .await
            .unwrap();
        assert_eq!(ok, Value::Str("x".into()));
    }

    #[tokio::test]
    async fn parse_number_fail_string_input() {
        let interp = crate::interp::Interp::new();
        let schema = make_schema("number");
        let err = mismatch(
            interp
                .parse_value(&schema, &Value::Str("x".into()), "", false, sp())
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
        let lit = Value::Object(crate::value::ObjectCell::new(m));
        let err = mismatch(
            interp
                .parse_value(&lit, &Value::Number(6.0), "", false, sp())
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
        // An object without __kind yields ParseFail::InvalidSchema (Tier-2),
        // NOT a Mismatch — so 6b recursion can't swallow it as validation error.
        let interp = crate::interp::Interp::new();
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("a".to_string(), Value::Number(1.0));
        let bad = Value::Object(crate::value::ObjectCell::new(m));
        let err = interp
            .parse_value(&bad, &Value::Nil, "", false, sp())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ParseFail::InvalidSchema(_)),
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
            .call_schema("parse", &[schema, Value::Str("x".into())], sp())
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
            Value::Object(crate::value::ObjectCell::new(m))
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
            .call_schema("parse", &[schema, Value::Str("x".into())], sp())
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
            Value::Object(crate::value::ObjectCell::new(m))
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
        Value::Object(crate::value::ObjectCell::new(m))
    }

    fn make_value_obj(pairs: &[(&str, Value)]) -> Value {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v.clone());
        }
        Value::Object(crate::value::ObjectCell::new(m))
    }

    #[tokio::test]
    async fn object_schema_ok() {
        let interp = crate::interp::Interp::new();
        let fields = make_fields_obj(&[("a", make_schema("number")), ("b", make_schema("string"))]);
        let obj_schema = interp.call_schema("object", &[fields], sp()).await.unwrap();
        let value = make_value_obj(&[("a", Value::Number(1.0)), ("b", Value::Str("x".into()))]);
        let pair = interp
            .call_schema("parse", &[obj_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn object_schema_err_path_at_root() {
        let interp = crate::interp::Interp::new();
        let fields = make_fields_obj(&[("a", make_schema("number")), ("b", make_schema("string"))]);
        let obj_schema = interp.call_schema("object", &[fields], sp()).await.unwrap();
        // b is a number but schema expects string
        let value = make_value_obj(&[("a", Value::Number(1.0)), ("b", Value::Number(2.0))]);
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
        let obj_schema = interp.call_schema("object", &[fields], sp()).await.unwrap();
        // extra key "b" should be ignored
        let value = make_value_obj(&[("a", Value::Number(1.0)), ("b", Value::Str("extra".into()))]);
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
        let obj_schema = interp.call_schema("object", &[fields], sp()).await.unwrap();
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
        let obj_schema = interp.call_schema("object", &[fields], sp()).await.unwrap();
        let strict_schema = interp
            .call_schema("strict", &[obj_schema], sp())
            .await
            .unwrap();
        // a=1 and b=2 (extra) → error
        let value = make_value_obj(&[("a", Value::Number(1.0)), ("b", Value::Number(2.0))]);
        let pair = interp
            .call_schema("parse", &[strict_schema, value], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(
            msg.contains("unknown key") || msg.contains("unexpected key"),
            "msg: {}",
            msg
        );
    }

    #[tokio::test]
    async fn strict_object_ok_no_extra() {
        let interp = crate::interp::Interp::new();
        let fields = make_fields_obj(&[("a", make_schema("number"))]);
        let obj_schema = interp.call_schema("object", &[fields], sp()).await.unwrap();
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
        let union_schema = interp.call_schema("union", &[options], sp()).await.unwrap();
        // string "x" matches first option
        let pair = interp
            .call_schema(
                "parse",
                &[union_schema.clone(), Value::Str("x".into())],
                sp(),
            )
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Str("x".into()));
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn union_accepts_second_match() {
        let interp = crate::interp::Interp::new();
        let options = make_array_val(vec![make_schema("string"), make_schema("number")]);
        let union_schema = interp.call_schema("union", &[options], sp()).await.unwrap();
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
        let union_schema = interp.call_schema("union", &[options], sp()).await.unwrap();
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
        let vals = make_array_val(vec![Value::Str("a".into()), Value::Str("b".into())]);
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
        let vals = make_array_val(vec![Value::Str("a".into()), Value::Str("b".into())]);
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
            .call_schema("map", &[make_schema("string"), make_schema("number")], sp())
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
            .call_schema("map", &[make_schema("string"), make_schema("number")], sp())
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
            .call_schema("map", &[make_schema("number"), make_schema("number")], sp())
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
        assert!(
            path.contains("(key)"),
            "key error path should carry marker: {}",
            path
        );
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
        let union_schema = interp.call_schema("union", &[options], sp()).await.unwrap();
        let result = interp
            .call_schema("parse", &[union_schema, Value::Str("x".into())], sp())
            .await;
        assert!(
            matches!(result, Err(Control::Panic(_))),
            "expected Tier-2 panic from malformed union option, got {:?}",
            result
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 6c: constraints / refine / default / coerce tests
    // ─────────────────────────────────────────────────────────────────────────

    // ── min / max numeric ────────────────────────────────────────────────────

    #[tokio::test]
    async fn min_ok() {
        let interp = crate::interp::Interp::new();
        // schema.min(schema.number(), 5.0) — value 10 is >= 5
        let num = make_schema("number");
        let s = interp
            .call_schema("min", &[num, Value::Number(5.0)], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Number(10.0)], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        assert_eq!(ok_val(&pair), Value::Number(10.0));
    }

    #[tokio::test]
    async fn min_fail() {
        let interp = crate::interp::Interp::new();
        let num = make_schema("number");
        let s = interp
            .call_schema("min", &[num, Value::Number(5.0)], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Number(3.0)], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(s) => s,
            other => panic!("{:?}", other),
        };
        assert!(msg.contains("min"), "message should mention 'min': {}", msg);
        assert!(
            msg.contains("5"),
            "message should mention the bound 5: {}",
            msg
        );
    }

    #[tokio::test]
    async fn max_ok() {
        let interp = crate::interp::Interp::new();
        let num = make_schema("number");
        let s = interp
            .call_schema("max", &[num, Value::Number(10.0)], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Number(7.0)], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn max_fail() {
        let interp = crate::interp::Interp::new();
        let num = make_schema("number");
        let s = interp
            .call_schema("max", &[num, Value::Number(10.0)], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Number(15.0)], sp())
            .await
            .unwrap();
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
        let s = interp
            .call_schema("minLength", &[str_s, Value::Number(3.0)], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Str("hello".into())], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn min_length_string_fail() {
        let interp = crate::interp::Interp::new();
        let str_s = make_schema("string");
        let s = interp
            .call_schema("minLength", &[str_s, Value::Number(5.0)], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Str("hi".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(m) => m,
            other => panic!("{:?}", other),
        };
        assert!(
            msg.contains("minLength") || msg.contains("min length"),
            "msg: {}",
            msg
        );
        assert!(msg.contains("5"), "msg: {}", msg);
    }

    #[tokio::test]
    async fn max_length_string_fail() {
        let interp = crate::interp::Interp::new();
        let str_s = make_schema("string");
        let s = interp
            .call_schema("maxLength", &[str_s, Value::Number(3.0)], sp())
            .await
            .unwrap();
        let pair = interp
            .call_schema("parse", &[s, Value::Str("hello".into())], sp())
            .await
            .unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(m) => m,
            other => panic!("{:?}", other),
        };
        assert!(
            msg.contains("maxLength") || msg.contains("max length"),
            "msg: {}",
            msg
        );
        assert!(msg.contains("3"), "msg: {}", msg);
    }

    // ── minLength / maxLength on array ────────────────────────────────────────

    #[tokio::test]
    async fn min_length_array_ok() {
        let interp = crate::interp::Interp::new();
        let arr_s = interp
            .call_schema("array", &[make_schema("number")], sp())
            .await
            .unwrap();
        let s = interp
            .call_schema("minLength", &[arr_s, Value::Number(2.0)], sp())
            .await
            .unwrap();
        let arr = Value::Array(Rc::new(RefCell::new(vec![
            Value::Number(1.0),
            Value::Number(2.0),
        ])));
        let pair = interp.call_schema("parse", &[s, arr], sp()).await.unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
    }

    #[tokio::test]
    async fn min_length_array_fail() {
        let interp = crate::interp::Interp::new();
        let arr_s = interp
            .call_schema("array", &[make_schema("number")], sp())
            .await
            .unwrap();
        let s = interp
            .call_schema("minLength", &[arr_s, Value::Number(3.0)], sp())
            .await
            .unwrap();
        let arr = Value::Array(Rc::new(RefCell::new(vec![Value::Number(1.0)])));
        let pair = interp.call_schema("parse", &[s, arr], sp()).await.unwrap();
        assert_eq!(ok_val(&pair), Value::Nil);
        let msg = match field(&err_val(&pair), "message") {
            Value::Str(m) => m,
            other => panic!("{:?}", other),
        };
        assert!(
            msg.contains("minLength") || msg.contains("min length"),
            "msg: {}",
            msg
        );
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
        // Build a trivial AScript fn `fn(x) { return true }` by constructing
        // an AST Function node, then call refine with it.
        use crate::ast::{CallArg, Expr, ExprKind, Param, Stmt};
        use crate::span::Span as S;

        let interp = crate::interp::Interp::new();
        let num_s = make_schema("number");

        // fn(x) { return true }
        let body = vec![Stmt::Return(Some(Expr {
            kind: ExprKind::Bool(true),
            span: S::new(0, 0),
        }))];
        let func = crate::value::Function {
            name: Some("truePred".into()),
            params: vec![Param {
                name: "x".into(),
                ty: None,
                name_span: S::new(0, 0),
                rest: false,
            }],
            ret: None,
            body,
            closure: crate::interp::global_env(),
            is_async: false,
            is_generator: false,
        };
        // Suppress unused import warning on CallArg
        let _ = CallArg::Pos(Expr {
            kind: ExprKind::Nil,
            span: S::new(0, 0),
        });
        let fn_val = Value::Function(std::rc::Rc::new(func));

        let s = interp
            .call_schema(
                "refine",
                &[num_s, fn_val, Value::Str("must pass".into())],
                sp(),
            )
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
        use crate::ast::{Expr, ExprKind, Param, Stmt};
        use crate::span::Span as S;

        let interp = crate::interp::Interp::new();
        let num_s = make_schema("number");

        // fn(x) { return false } — always fails
        let body = vec![Stmt::Return(Some(Expr {
            kind: ExprKind::Bool(false),
            span: S::new(0, 0),
        }))];
        let func = crate::value::Function {
            name: Some("falsePred".into()),
            params: vec![Param {
                name: "x".into(),
                ty: None,
                name_span: S::new(0, 0),
                rest: false,
            }],
            ret: None,
            body,
            closure: crate::interp::global_env(),
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
        use crate::ast::{CallArg, Expr, ExprKind, Param, Stmt};
        use crate::span::Span as S;

        let interp = crate::interp::Interp::new();

        // fn(x) { assert(false) } — panics
        let assert_call = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Ident("assert".into()),
                    span: S::new(0, 0),
                }),
                args: vec![CallArg::Pos(Expr {
                    kind: ExprKind::Bool(false),
                    span: S::new(0, 0),
                })],
            },
            span: S::new(0, 0),
        };
        let body = vec![Stmt::Expr(assert_call)];
        let func = crate::value::Function {
            name: Some("panicPred".into()),
            params: vec![Param {
                name: "x".into(),
                ty: None,
                name_span: S::new(0, 0),
                rest: false,
            }],
            ret: None,
            body,
            closure: crate::interp::global_env(),
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
        Value::Object(crate::value::ObjectCell::new(m))
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
            .call_schema("parse", &[str_s, Value::Number(1.5), opts], sp())
            .await
            .unwrap();
        assert_eq!(err_val(&pair), Value::Nil);
        // The coerced string should contain "1.5"
        match ok_val(&pair) {
            Value::Str(s) => assert!(s.contains("1.5"), "coerced string: {}", s),
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
}
