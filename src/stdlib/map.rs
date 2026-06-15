//! `std/map` — the `Map` collection (insertion-ordered, hashable keys).

use super::{arg, bi};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::{MapKey, Value, ValueKind};
use indexmap::IndexMap;

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

fn want_map(
    v: &Value,
    span: Span,
    ctx: &str,
) -> Result<gcmodule::Cc<crate::value::MapCell>, Control> {
    match v.kind() {
        ValueKind::Map(m) => Ok(m.clone()),
        _ => Err(AsError::at(
            format!("{} expects a map, got {}", ctx, crate::interp::type_name(v)),
            span,
        )
        .into()),
    }
}

fn want_key(v: &Value, span: Span, ctx: &str) -> Result<MapKey, Control> {
    MapKey::from_value(v).ok_or_else(|| {
        AsError::at(
            format!(
                "{}: map keys must be nil, bool, number, or string, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()
    })
}

fn arr(v: Vec<Value>) -> Value {
    Value::array(v)
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("map.{}", f);
    match func {
        "new" => {
            let m = crate::value::MapCell::new(IndexMap::new());
            // Optional: seed from an array of [k, v] entry pairs.
            if let Some(seed) = args.first() {
                if !matches!(seed.kind(), ValueKind::Nil) {
                    let entries = match seed.kind() {
                        ValueKind::Array(a) => a.borrow().clone(),
                        _ => return Err(AsError::at(format!("map.new optional argument must be an array of [key, value] pairs, got {}", crate::interp::type_name(seed)), span).into()),
                    };
                    for e in entries {
                        match e.kind() {
                            ValueKind::Array(pair) if pair.borrow().len() == 2 => {
                                let p = pair.borrow();
                                let key = want_key(&p[0], span, "map.new")?;
                                m.borrow_mut().insert(key, p[1].clone());
                            }
                            _ => {
                                return Err(AsError::at(
                                    "map.new entries must be [key, value] pairs",
                                    span,
                                )
                                .into())
                            }
                        }
                    }
                }
            }
            Ok(Value::map_cell(m))
        }
        "get" => {
            let m = want_map(&arg(args, 0), span, &ctx("get"))?;
            let k = want_key(&arg(args, 1), span, &ctx("get"))?;
            let got = m.borrow().get(&k).cloned();
            Ok(got.unwrap_or(Value::nil()))
        }
        "set" => {
            let m = want_map(&arg(args, 0), span, &ctx("set"))?;
            crate::interp::check_not_frozen(&arg(args, 0), span)?;
            let k = want_key(&arg(args, 1), span, &ctx("set"))?;
            let v = arg(args, 2);
            m.borrow_mut().insert(k, v);
            Ok(arg(args, 0)) // return the map (chainable)
        }
        "has" => {
            let m = want_map(&arg(args, 0), span, &ctx("has"))?;
            let k = want_key(&arg(args, 1), span, &ctx("has"))?;
            let has = m.borrow().contains_key(&k);
            Ok(Value::bool_(has))
        }
        "delete" => {
            let m = want_map(&arg(args, 0), span, &ctx("delete"))?;
            crate::interp::check_not_frozen(&arg(args, 0), span)?;
            let k = want_key(&arg(args, 1), span, &ctx("delete"))?;
            let existed = m.borrow_mut().shift_remove(&k).is_some();
            Ok(Value::bool_(existed))
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
            let entries: Vec<Value> = m
                .borrow()
                .iter()
                .map(|(k, v)| arr(vec![k.to_value(), v.clone()]))
                .collect();
            Ok(arr(entries))
        }
        _ => Err(AsError::at(format!("std/map has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn new_set_get_has_delete() {
        let m = call("new", &[], sp()).unwrap();
        call(
            "set",
            &[m.clone(), Value::str("a"), Value::float(1.0)],
            sp(),
        )
        .unwrap();
        call(
            "set",
            &[m.clone(), Value::float(2.0), Value::str("two")],
            sp(),
        )
        .unwrap();
        assert_eq!(
            call("get", &[m.clone(), Value::str("a")], sp()).unwrap(),
            Value::float(1.0)
        );
        assert_eq!(
            call("get", &[m.clone(), Value::float(2.0)], sp()).unwrap(),
            Value::str("two")
        );
        assert_eq!(
            call("get", &[m.clone(), Value::str("z")], sp()).unwrap(),
            Value::nil()
        );
        assert_eq!(
            call("has", &[m.clone(), Value::str("a")], sp()).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            call("delete", &[m.clone(), Value::str("a")], sp()).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            call("has", &[m.clone(), Value::str("a")], sp()).unwrap(),
            Value::bool_(false)
        );
        assert_eq!(
            call("keys", std::slice::from_ref(&m), sp())
                .unwrap()
                .to_string(),
            "[2]"
        );
    }

    #[test]
    fn new_with_seed_and_bad_seed() {
        let sp = sp();
        let seed = Value::array(vec![
            Value::array(vec![
                Value::str("a"),
                Value::float(1.0),
            ]),
            Value::array(vec![
                Value::str("b"),
                Value::float(2.0),
            ]),
        ]);
        let m = call("new", std::slice::from_ref(&seed), sp).unwrap();
        assert_eq!(
            call("get", &[m.clone(), Value::str("b")], sp).unwrap(),
            Value::float(2.0)
        );
        // non-array seed → panic
        assert!(matches!(
            call("new", &[Value::float(5.0)], sp),
            Err(Control::Panic(_))
        ));
        // wrong-arity entry → panic
        let bad = Value::array(vec![Value::array(vec![Value::float(1.0)])]);
        assert!(matches!(call("new", &[bad], sp), Err(Control::Panic(_))));
    }

    #[test]
    fn nan_and_neg_zero_keys_collide() {
        let sp = sp();
        let m = call("new", &[], sp).unwrap();
        // -0.0 and 0.0 are the same key
        call(
            "set",
            &[m.clone(), Value::float(-0.0), Value::str("z")],
            sp,
        )
        .unwrap();
        assert_eq!(
            call("get", &[m.clone(), Value::float(0.0)], sp).unwrap(),
            Value::str("z")
        );
        // setting NaN twice collapses to one entry
        call(
            "set",
            &[m.clone(), Value::float(f64::NAN), Value::float(1.0)],
            sp,
        )
        .unwrap();
        call(
            "set",
            &[m.clone(), Value::float(f64::NAN), Value::float(2.0)],
            sp,
        )
        .unwrap();
        assert_eq!(
            call("get", &[m.clone(), Value::float(f64::NAN)], sp).unwrap(),
            Value::float(2.0)
        );
        // len: keys {0.0, NaN} = 2
        assert_eq!(m, m.clone()); // sanity
    }

    #[test]
    fn non_hashable_key_panics() {
        let m = call("new", &[], sp()).unwrap();
        let bad = Value::array(vec![]);
        assert!(matches!(
            call("set", &[m, bad, Value::float(1.0)], sp()),
            Err(Control::Panic(_))
        ));
    }
}
