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
            let keys: Vec<Value> = o.borrow().keys().map(|k| Value::Str(k.as_str().into())).collect();
            Ok(arr(keys))
        }
        "values" => {
            let o = want_object(&arg(args, 0), span, &ctx("values"))?;
            let vals: Vec<Value> = o.borrow().values().cloned().collect();
            Ok(arr(vals))
        }
        "entries" => {
            let o = want_object(&arg(args, 0), span, &ctx("entries"))?;
            let entries: Vec<Value> = o
                .borrow()
                .iter()
                .map(|(k, v)| arr(vec![Value::Str(k.as_str().into()), v.clone()]))
                .collect();
            Ok(arr(entries))
        }
        "has" => {
            let o = want_object(&arg(args, 0), span, &ctx("has"))?;
            let key = want_string(&arg(args, 1), span, &ctx("has"))?;
            let has = o.borrow().contains_key(key.as_ref());
            Ok(Value::Bool(has))
        }
        "delete" => {
            let o = want_object(&arg(args, 0), span, &ctx("delete"))?;
            let key = want_string(&arg(args, 1), span, &ctx("delete"))?;
            // shift_remove preserves the order of the remaining keys.
            let existed = o.borrow_mut().shift_remove(key.as_ref()).is_some();
            Ok(Value::Bool(existed))
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
        assert_eq!(call("keys", std::slice::from_ref(&o), sp()).unwrap().to_string(), "[\"a\", \"b\"]");
        assert_eq!(call("values", std::slice::from_ref(&o), sp()).unwrap().to_string(), "[1, 2]");
        assert_eq!(call("entries", std::slice::from_ref(&o), sp()).unwrap().to_string(), "[[\"a\", 1], [\"b\", 2]]");
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
