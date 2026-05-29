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
                                let key = want_key(&p[0], span, "map.new")?;
                                m.borrow_mut().insert(key, p[1].clone());
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
            let got = m.borrow().get(&k).cloned();
            Ok(got.unwrap_or(Value::Nil))
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
            let has = m.borrow().contains_key(&k);
            Ok(Value::Bool(has))
        }
        "delete" => {
            let m = want_map(&arg(args, 0), span, &ctx("delete"))?;
            let k = want_key(&arg(args, 1), span, &ctx("delete"))?;
            let existed = m.borrow_mut().shift_remove(&k).is_some();
            Ok(Value::Bool(existed))
        }
        "keys" => {
            let m = want_map(&arg(args, 0), span, &ctx("keys"))?;
            let keys: Vec<Value> = m.borrow().keys().map(|k| k.to_value()).collect();
            Ok(arr(keys))
        }
        "values" => {
            let m = want_map(&arg(args, 0), span, &ctx("values"))?;
            let vals: Vec<Value> = m.borrow().values().cloned().collect();
            Ok(arr(vals))
        }
        "entries" => {
            let m = want_map(&arg(args, 0), span, &ctx("entries"))?;
            let entries: Vec<Value> = m.borrow().iter().map(|(k, v)| arr(vec![k.to_value(), v.clone()])).collect();
            Ok(arr(entries))
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
        assert_eq!(call("keys", std::slice::from_ref(&m), sp()).unwrap().to_string(), "[2]");
    }

    #[test]
    fn non_hashable_key_panics() {
        let m = call("new", &[], sp()).unwrap();
        let bad = Value::Array(Rc::new(RefCell::new(vec![])));
        assert!(matches!(call("set", &[m, bad, Value::Number(1.0)], sp()), Err(Control::Panic(_))));
    }
}
