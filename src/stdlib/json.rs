//! `std/json` — JSON parse/stringify, plus the shared AScript<->serde_json
//! converter reused by std/toml and std/yaml.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use indexmap::IndexMap;
use std::cell::RefCell;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("parse", bi("json.parse")),
        ("stringify", bi("json.stringify")),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("json.{}", f);
    match func {
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(jv) => Ok(make_pair(to_ascript(&jv), Value::Nil)),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("invalid JSON: {}", e).into())),
                )),
            }
        }
        "stringify" => {
            let v = arg(args, 0);
            let pretty = matches!(args.get(1), Some(Value::Number(n)) if *n > 0.0)
                || matches!(args.get(1), Some(Value::Bool(true)));
            match from_ascript(&v, &mut Vec::new()) {
                Ok(jv) => {
                    let s = if pretty {
                        serde_json::to_string_pretty(&jv)
                    } else {
                        serde_json::to_string(&jv)
                    };
                    match s {
                        Ok(text) => Ok(make_pair(Value::Str(text.into()), Value::Nil)),
                        Err(e) => Ok(make_pair(
                            Value::Nil,
                            make_error(Value::Str(format!("cannot serialize: {}", e).into())),
                        )),
                    }
                }
                Err(msg) => Ok(make_pair(Value::Nil, make_error(Value::Str(msg.into())))),
            }
        }
        _ => Err(AsError::at(format!("std/json has no function '{}'", func), span).into()),
    }
}

/// serde_json::Value -> AScript Value. Objects become insertion-ordered Objects.
pub(crate) fn to_ascript(jv: &serde_json::Value) -> Value {
    match jv {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
        serde_json::Value::String(s) => Value::Str(s.as_str().into()),
        serde_json::Value::Array(a) => {
            Value::Array(gcmodule::Cc::new(RefCell::new(a.iter().map(to_ascript).collect())))
        }
        serde_json::Value::Object(o) => {
            let mut m = IndexMap::new();
            for (k, v) in o {
                m.insert(k.clone(), to_ascript(v));
            }
            Value::Object(crate::value::ObjectCell::new(m))
        }
    }
}

/// AScript Value -> serde_json::Value. Err (String) on a non-serializable value
/// or a reference cycle (`seen` tracks Array/Object/Map Rc pointers in progress).
pub(crate) fn from_ascript(v: &Value, seen: &mut Vec<usize>) -> Result<serde_json::Value, String> {
    match v {
        Value::Nil => Ok(serde_json::Value::Null),
        Value::Bool(b) => Ok(serde_json::Value::Bool(*b)),
        // Decimal: emit as a JSON number literal from the canonical string.
        // `rust_decimal` always produces a valid JSON number string (no ±Inf/NaN),
        // so `serde_json::from_str` never fails here.
        Value::Decimal(d) => {
            let s = d.to_string();
            let raw: serde_json::Value = serde_json::from_str(&s)
                .map_err(|_| format!("cannot serialize decimal {} to JSON", d))?;
            Ok(raw)
        }
        Value::Number(n) => {
            if !n.is_finite() {
                return Err(format!("cannot serialize non-finite number {} to JSON", n));
            }
            // Match AScript's own number Display: an integer-valued float
            // serializes as a JSON integer (`1`, not `1.0`). serde_json's
            // Number::from_f64 always renders a float with a trailing `.0`.
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                Ok(serde_json::Value::Number(serde_json::Number::from(
                    *n as i64,
                )))
            } else {
                serde_json::Number::from_f64(*n)
                    .map(serde_json::Value::Number)
                    .ok_or_else(|| format!("cannot serialize number {} to JSON", n))
            }
        }
        Value::Str(s) => Ok(serde_json::Value::String(s.to_string())),
        Value::Array(a) => {
            let ptr = crate::gc::cc_addr(a);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to JSON".into());
            }
            seen.push(ptr);
            let mut out = Vec::new();
            for item in a.borrow().iter() {
                out.push(from_ascript(item, seen)?);
            }
            seen.pop();
            Ok(serde_json::Value::Array(out))
        }
        Value::Object(o) => {
            let ptr = crate::gc::cc_addr(o);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to JSON".into());
            }
            seen.push(ptr);
            let mut map = serde_json::Map::new();
            for (k, val) in o.borrow().iter() {
                map.insert(k.clone(), from_ascript(val, seen)?);
            }
            seen.pop();
            Ok(serde_json::Value::Object(map))
        }
        Value::Map(m) => {
            // A Map serializes as a JSON object only if every key is a string.
            let ptr = crate::gc::cc_addr(m);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to JSON".into());
            }
            seen.push(ptr);
            let mut map = serde_json::Map::new();
            for (k, val) in m.borrow().iter() {
                match k.to_value() {
                    Value::Str(s) => {
                        map.insert(s.to_string(), from_ascript(val, seen)?);
                    }
                    other => {
                        return Err(format!(
                            "cannot serialize a map with a non-string key ({}) to JSON",
                            crate::interp::type_name(&other)
                        ))
                    }
                }
            }
            seen.pop();
            Ok(serde_json::Value::Object(map))
        }
        Value::Set(s) => {
            // A Set serializes as a JSON array of its values (insertion order).
            let ptr = crate::gc::cc_addr(s);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to JSON".into());
            }
            seen.push(ptr);
            let mut out = Vec::new();
            for k in s.borrow().iter() {
                out.push(from_ascript(&k.to_value(), seen)?);
            }
            seen.pop();
            Ok(serde_json::Value::Array(out))
        }
        other => Err(format!(
            "cannot serialize a value of type {} to JSON",
            crate::interp::type_name(other)
        )),
    }
}

/// AScript Value -> serde_json::Value, TOTAL: never errors. Cycles → "[Circular]",
/// non-finite numbers → null, functions/native → "<function>"/"<type>". Used by
/// std/log so a logging call never crashes the program.
pub(crate) fn to_json_lossy(v: &Value, seen: &mut Vec<usize>) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Nil => J::Null,
        Value::Bool(b) => J::Bool(*b),
        // Decimal: emit as a JSON number from the canonical string (always finite).
        Value::Decimal(d) => serde_json::from_str::<J>(&d.to_string()).unwrap_or(J::Null),
        Value::Number(n) => {
            if !n.is_finite() {
                return J::Null;
            }
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                J::Number(serde_json::Number::from(*n as i64))
            } else {
                serde_json::Number::from_f64(*n)
                    .map(J::Number)
                    .unwrap_or(J::Null)
            }
        }
        Value::Str(s) => J::String(s.to_string()),
        Value::Array(a) => {
            let ptr = crate::gc::cc_addr(a);
            if seen.contains(&ptr) {
                return J::String("[Circular]".into());
            }
            seen.push(ptr);
            let out = a.borrow().iter().map(|x| to_json_lossy(x, seen)).collect();
            seen.pop();
            J::Array(out)
        }
        Value::Object(o) => {
            let ptr = crate::gc::cc_addr(o);
            if seen.contains(&ptr) {
                return J::String("[Circular]".into());
            }
            seen.push(ptr);
            let mut m = serde_json::Map::new();
            for (k, val) in o.borrow().iter() {
                m.insert(k.clone(), to_json_lossy(val, seen));
            }
            seen.pop();
            J::Object(m)
        }
        Value::Instance(i) => {
            let ptr = crate::gc::cc_addr(i);
            if seen.contains(&ptr) {
                return J::String("[Circular]".into());
            }
            seen.push(ptr);
            let mut m = serde_json::Map::new();
            for (k, val) in i.borrow().fields.iter() {
                m.insert(k.clone(), to_json_lossy(val, seen));
            }
            seen.pop();
            J::Object(m)
        }
        Value::Map(mp) => {
            let ptr = crate::gc::cc_addr(mp);
            if seen.contains(&ptr) {
                return J::String("[Circular]".into());
            }
            seen.push(ptr);
            let mut m = serde_json::Map::new();
            for (k, val) in mp.borrow().iter() {
                let key = match k.to_value() {
                    Value::Str(s) => s.to_string(),
                    other => other.to_string(),
                };
                m.insert(key, to_json_lossy(val, seen));
            }
            seen.pop();
            J::Object(m)
        }
        Value::Set(s) => {
            let ptr = crate::gc::cc_addr(s);
            if seen.contains(&ptr) {
                return J::String("[Circular]".into());
            }
            seen.push(ptr);
            let out = s
                .borrow()
                .iter()
                .map(|k| to_json_lossy(&k.to_value(), seen))
                .collect();
            seen.pop();
            J::Array(out)
        }
        Value::Function(_) | Value::Closure(_) | Value::Builtin(_) => {
            J::String("<function>".into())
        }
        other => J::String(format!("<{}>", crate::interp::type_name(other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }

    #[test]
    fn parse_roundtrip() {
        let parsed = call(
            "parse",
            &[s("{\"a\": 1, \"b\": [true, null, \"x\"]}")],
            sp(),
        )
        .unwrap();
        // parsed is [value, nil]; pull the value (index 0)
        assert!(parsed
            .to_string()
            .starts_with("[{a: 1, b: [true, nil, \"x\"]}, nil]"));
    }

    #[test]
    fn parse_preserves_key_order() {
        // serde_json's preserve_order feature keeps object keys in source order
        // (not alphabetical), matching AScript's insertion-ordered objects.
        let parsed = call(
            "parse",
            &[Value::Str("{\"name\": 1, \"age\": 2, \"zoo\": 3}".into())],
            Span::new(0, 0),
        )
        .unwrap();
        assert!(parsed
            .to_string()
            .starts_with("[{name: 1, age: 2, zoo: 3}, nil]"));
    }

    #[test]
    fn stringify_and_errors() {
        let obj = {
            let mut m = IndexMap::new();
            m.insert("n".to_string(), Value::Number(2.0));
            Value::Object(crate::value::ObjectCell::new(m))
        };
        let out = call("stringify", std::slice::from_ref(&obj), sp()).unwrap();
        // out is the pair [resultString, nil]; the result string is the JSON
        // text `{"n":2}` (integer 2, not 2.0). Inside the pair's array Display
        // the string is quoted+escaped, hence the `\"` in the expected text.
        assert_eq!(out.to_string(), "[\"{\\\"n\\\":2}\", nil]");
        // a function is not serializable → [nil, err]
        let f = Value::Builtin("print".into());
        let err = call("stringify", std::slice::from_ref(&f), sp()).unwrap();
        assert!(err.to_string().starts_with("[nil, {message:"));
    }

    #[test]
    fn lossy_serializer_never_errors() {
        let a = Value::Array(gcmodule::Cc::new(RefCell::new(vec![])));
        if let Value::Array(inner) = &a {
            inner.borrow_mut().push(a.clone());
        }
        assert_eq!(
            to_json_lossy(&a, &mut Vec::new()).to_string(),
            "[\"[Circular]\"]"
        );
        assert_eq!(
            to_json_lossy(&Value::Builtin("print".into()), &mut Vec::new()).to_string(),
            "\"<function>\""
        );
        assert_eq!(
            to_json_lossy(&Value::Number(f64::NAN), &mut Vec::new()),
            serde_json::Value::Null
        );
    }

    #[test]
    fn parse_invalid_is_tier1_err() {
        let err = call("parse", &[s("{bad")], sp()).unwrap();
        assert!(err.to_string().starts_with("[nil, {message:"));
    }
}
