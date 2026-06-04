//! `std/object` — object (string-keyed map) operations.
//! Callback-taking functions (`mapValues`) live on `impl Interp`; everything
//! else is pure and lives in the top-level `call` function.

use super::{arg, bi, want_array, want_object, want_string};
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::{Instance, MapKey, Value};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("keys", bi("object.keys")),
        ("values", bi("object.values")),
        ("entries", bi("object.entries")),
        ("has", bi("object.has")),
        ("delete", bi("object.delete")),
        ("merge", bi("object.merge")),
        ("fromEntries", bi("object.fromEntries")),
        ("pick", bi("object.pick")),
        ("omit", bi("object.omit")),
        ("deepClone", bi("object.deepClone")),
        ("deepEqual", bi("object.deepEqual")),
        ("mapValues", bi("object.mapValues")),
        ("freeze", bi("object.freeze")),
        ("isFrozen", bi("object.isFrozen")),
    ]
}

/// Structural deep equality (distinct from `==`, which is identity for containers).
/// Cycle-safe: a `seen` set of pointer-pairs short-circuits revisited container
/// pairs to `true` — a re-encountered pair is already being compared up the stack,
/// so if it were unequal that outer comparison would already have returned false.
pub(crate) fn deep_equal(a: &Value, b: &Value) -> bool {
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    deep_equal_inner(a, b, &mut seen)
}

fn deep_equal_inner(
    a: &Value,
    b: &Value,
    seen: &mut std::collections::HashSet<(usize, usize)>,
) -> bool {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            if !seen.insert((crate::gc::cc_addr(x), crate::gc::cc_addr(y))) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(p, q)| deep_equal_inner(p, q, seen))
        }
        (Value::Object(x), Value::Object(y)) => {
            if !seen.insert((crate::gc::cc_addr(x), crate::gc::cc_addr(y))) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| deep_equal_inner(v, w, seen)))
        }
        (Value::Map(x), Value::Map(y)) => {
            if !seen.insert((crate::gc::cc_addr(x), crate::gc::cc_addr(y))) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| deep_equal_inner(v, w, seen)))
        }
        (Value::Set(x), Value::Set(y)) => {
            if !seen.insert((crate::gc::cc_addr(x), crate::gc::cc_addr(y))) {
                return true;
            }
            // Order-independent: same size and every element of x is in y.
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().all(|k| y.contains(k))
        }
        (Value::Bytes(x), Value::Bytes(y)) => *x.borrow() == *y.borrow(),
        (Value::Instance(x), Value::Instance(y)) => {
            if !seen.insert((crate::gc::cc_addr(x), crate::gc::cc_addr(y))) {
                return true;
            }
            let (x, y) = (x.borrow(), y.borrow());
            Rc::ptr_eq(&x.class, &y.class)
                && x.fields.len() == y.fields.len()
                && x.fields.iter().all(|(k, v)| {
                    y.fields
                        .get(k)
                        .is_some_and(|w| deep_equal_inner(v, w, seen))
                })
        }
        // Identity equality for regex/native/enum/function/future/generator/etc.
        // (two structurally-equal Regex objects compare unequal here — acceptable.)
        _ => a == b,
    }
}

/// Deep copy of containers; shares functions/natives/etc.
/// Cycle- and sharing-safe via an `Rc`-pointer identity map.
pub(crate) fn deep_clone(v: &Value, seen: &mut HashMap<usize, Value>) -> Value {
    match v {
        Value::Array(rc) => {
            let key = crate::gc::cc_addr(rc);
            if let Some(c) = seen.get(&key) {
                return c.clone();
            }
            let out = crate::value::ArrayCell::new(Vec::new());
            let cloned = Value::Array(out.clone());
            seen.insert(key, cloned.clone());
            let src = rc.borrow().clone();
            {
                let mut dst = out.borrow_mut();
                for el in src.iter() {
                    dst.push(deep_clone(el, seen));
                }
            }
            cloned
        }
        Value::Object(rc) => {
            let key = crate::gc::cc_addr(rc);
            if let Some(c) = seen.get(&key) {
                return c.clone();
            }
            let out = crate::value::ObjectCell::new(IndexMap::new());
            let cloned = Value::Object(out.clone());
            seen.insert(key, cloned.clone());
            let src = rc.borrow().clone();
            {
                let mut dst = out.borrow_mut();
                for (k, val) in src.iter() {
                    dst.insert(k.clone(), deep_clone(val, seen));
                }
            }
            cloned
        }
        Value::Map(rc) => {
            let key = crate::gc::cc_addr(rc);
            if let Some(c) = seen.get(&key) {
                return c.clone();
            }
            let out = crate::value::MapCell::new(IndexMap::<MapKey, Value>::new());
            let cloned = Value::Map(out.clone());
            seen.insert(key, cloned.clone());
            let src = rc.borrow().clone();
            {
                let mut dst = out.borrow_mut();
                for (k, val) in src.iter() {
                    dst.insert(k.clone(), deep_clone(val, seen));
                }
            }
            cloned
        }
        Value::Set(rc) => {
            let key = crate::gc::cc_addr(rc);
            if let Some(c) = seen.get(&key) {
                return c.clone();
            }
            let cloned_set = rc.borrow().clone();
            let out = Value::Set(crate::value::SetCell::new(cloned_set));
            seen.insert(key, out.clone());
            out
        }
        Value::Bytes(rc) => Value::Bytes(Rc::new(RefCell::new(rc.borrow().clone()))),
        Value::Instance(rc) => {
            let key = crate::gc::cc_addr(rc);
            if let Some(c) = seen.get(&key) {
                return c.clone();
            }
            let (class, fields) = {
                let src = rc.borrow();
                (src.class.clone(), src.fields.clone())
            };
            let out = gcmodule::Cc::new(RefCell::new(Instance {
                class,
                fields: IndexMap::new(),
                shape_id: std::cell::Cell::new(0),
                frozen: std::cell::Cell::new(false),
            }));
            let cloned = Value::Instance(out.clone());
            seen.insert(key, cloned.clone());
            {
                let mut dst = out.borrow_mut();
                for (k, val) in fields.iter() {
                    dst.fields.insert(k.clone(), deep_clone(val, seen));
                }
            }
            cloned
        }
        other => other.clone(),
    }
}

fn arr(v: Vec<Value>) -> Value {
    Value::Array(crate::value::ArrayCell::new(v))
}

/// Clone the field map of an object-like value (Object or class Instance).
/// Tier-2 panic on any other kind.
fn object_like_fields(
    v: &Value,
    span: Span,
    ctx: &str,
) -> Result<IndexMap<String, Value>, Control> {
    match v {
        Value::Object(o) => Ok(o.borrow().clone()),
        Value::Instance(i) => Ok(i.borrow().fields.clone()),
        _ => Err(AsError::at(format!("{} expects an object or instance", ctx), span).into()),
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("object.{}", f);
    match func {
        "keys" => {
            let o = want_object(&arg(args, 0), span, &ctx("keys"))?;
            let keys: Vec<Value> = o
                .borrow()
                .keys()
                .map(|k| Value::Str(k.as_str().into()))
                .collect();
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
                let o = want_object(v, span, &format!("{} (argument {})", ctx("merge"), i + 1))?;
                for (k, val) in o.borrow().iter() {
                    out.insert(k.clone(), val.clone());
                }
            }
            Ok(Value::Object(crate::value::ObjectCell::new(out)))
        }
        "fromEntries" => {
            let pairs = want_array(&arg(args, 0), span, &ctx("fromEntries"))?;
            let mut out = IndexMap::new();
            for pair in pairs.borrow().iter() {
                let p = want_array(pair, span, &ctx("fromEntries"))?;
                let p = p.borrow();
                let k = want_string(
                    &p.first().cloned().unwrap_or(Value::Nil),
                    span,
                    &ctx("fromEntries"),
                )?;
                let v = p.get(1).cloned().unwrap_or(Value::Nil);
                out.insert(k.to_string(), v);
            }
            Ok(Value::Object(crate::value::ObjectCell::new(out)))
        }
        "pick" => {
            let src = object_like_fields(&arg(args, 0), span, "object.pick")?;
            let keys = want_array(&arg(args, 1), span, &ctx("pick"))?;
            let mut out = IndexMap::new();
            for k in keys.borrow().iter() {
                let k = want_string(k, span, &ctx("pick"))?;
                if let Some(v) = src.get(k.as_ref()) {
                    out.insert(k.to_string(), v.clone());
                }
            }
            Ok(Value::Object(crate::value::ObjectCell::new(out)))
        }
        "omit" => {
            let src = object_like_fields(&arg(args, 0), span, "object.omit")?;
            let keys = want_array(&arg(args, 1), span, &ctx("omit"))?;
            let drop_set: std::collections::HashSet<String> = keys
                .borrow()
                .iter()
                .map(|k| want_string(k, span, &ctx("omit")).map(|s| s.to_string()))
                .collect::<Result<_, _>>()?;
            let out: IndexMap<String, Value> = src
                .iter()
                .filter(|(k, _)| !drop_set.contains(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Ok(Value::Object(crate::value::ObjectCell::new(out)))
        }
        "deepEqual" => Ok(Value::Bool(deep_equal(&arg(args, 0), &arg(args, 1)))),
        "deepClone" => {
            let mut seen = HashMap::new();
            Ok(deep_clone(&arg(args, 0), &mut seen))
        }
        // SP2 §4: shallow-freeze a mutable container in place and return it
        // (chainable). No-op for a non-container (JS ergonomics). Idempotent.
        "freeze" => {
            let v = arg(args, 0);
            crate::value::freeze_value(&v);
            Ok(v)
        }
        // SP2 §4: whether the value is a frozen container. `false` for any
        // non-container value.
        "isFrozen" => Ok(Value::Bool(crate::value::is_frozen_value(&arg(args, 0)))),
        _ => Err(AsError::at(format!("std/object has no function '{}'", func), span).into()),
    }
}

impl Interp {
    /// Object dispatch: callback-taking fns live here; everything else delegates
    /// to the pure `object::call`.
    pub(crate) async fn call_object(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "mapValues" => {
                // object_like_fields already clones out the map, so no RefCell
                // borrow is held across any .await point.
                let src = object_like_fields(&arg(args, 0), span, "object.mapValues")?;
                let f = arg(args, 1);
                let mut out = IndexMap::new();
                for (k, v) in src.iter() {
                    let mapped = self
                        .call_value(
                            f.clone(),
                            vec![v.clone(), Value::Str(k.as_str().into())],
                            span,
                        )
                        .await?;
                    out.insert(k.clone(), mapped);
                }
                Ok(Value::Object(crate::value::ObjectCell::new(out)))
            }
            _ => call(func, args, span),
        }
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
    fn obj(pairs: Vec<(&str, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        Value::Object(crate::value::ObjectCell::new(m))
    }
    fn obj_ref(pairs: &[(&str, Value)]) -> Value {
        obj(pairs.iter().map(|(k, v)| (*k, v.clone())).collect())
    }

    #[test]
    fn keys_values_entries() {
        let o = obj_ref(&[("a", Value::Number(1.0)), ("b", Value::Number(2.0))]);
        assert_eq!(
            call("keys", std::slice::from_ref(&o), sp())
                .unwrap()
                .to_string(),
            "[\"a\", \"b\"]"
        );
        assert_eq!(
            call("values", std::slice::from_ref(&o), sp())
                .unwrap()
                .to_string(),
            "[1, 2]"
        );
        assert_eq!(
            call("entries", std::slice::from_ref(&o), sp())
                .unwrap()
                .to_string(),
            "[[\"a\", 1], [\"b\", 2]]"
        );
    }

    #[test]
    fn has_delete_merge() {
        let o = obj_ref(&[("a", Value::Number(1.0))]);
        assert_eq!(
            call("has", &[o.clone(), Value::Str("a".into())], sp()).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call("has", &[o.clone(), Value::Str("z".into())], sp()).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            call("delete", &[o.clone(), Value::Str("a".into())], sp()).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call("has", &[o, Value::Str("a".into())], sp()).unwrap(),
            Value::Bool(false)
        );
        let merged = call(
            "merge",
            &[
                obj_ref(&[("a", Value::Number(1.0)), ("b", Value::Number(2.0))]),
                obj_ref(&[("b", Value::Number(9.0)), ("c", Value::Number(3.0))]),
            ],
            sp(),
        )
        .unwrap();
        assert_eq!(merged.to_string(), "{a: 1, b: 9, c: 3}");
    }

    #[test]
    fn merge_and_delete_edges() {
        let sp = sp();
        // delete of a non-existent key → false
        let o = obj_ref(&[("a", Value::Number(1.0))]);
        assert_eq!(
            call("delete", &[o, Value::Str("nope".into())], sp).unwrap(),
            Value::Bool(false)
        );
        // merge with zero args → empty object
        assert_eq!(call("merge", &[], sp).unwrap().to_string(), "{}");
        // merge with one arg → a copy (independent of the input)
        let src = obj_ref(&[("a", Value::Number(1.0))]);
        let copy = call("merge", std::slice::from_ref(&src), sp).unwrap();
        assert_eq!(copy.to_string(), "{a: 1}");
        // mutating the copy via delete does NOT affect the source (independence)
        call("delete", &[copy, Value::Str("a".into())], sp).unwrap();
        assert_eq!(
            call("keys", std::slice::from_ref(&src), sp)
                .unwrap()
                .to_string(),
            "[\"a\"]"
        );
    }

    #[test]
    fn object_pure() {
        let o = obj(vec![
            ("a", Value::Number(1.0)),
            ("b", Value::Number(2.0)),
            ("c", Value::Number(3.0)),
        ]);
        let keys = arr(vec![s("a"), s("c")]);
        assert_eq!(
            call("pick", &[o.clone(), keys.clone()], sp())
                .unwrap()
                .to_string(),
            obj(vec![("a", Value::Number(1.0)), ("c", Value::Number(3.0))]).to_string()
        );
        assert_eq!(
            call("omit", &[o.clone(), keys], sp()).unwrap().to_string(),
            obj(vec![("b", Value::Number(2.0))]).to_string()
        );
        let entries = arr(vec![arr(vec![s("x"), Value::Number(9.0)])]);
        assert_eq!(
            call("fromEntries", std::slice::from_ref(&entries), sp())
                .unwrap()
                .to_string(),
            obj(vec![("x", Value::Number(9.0))]).to_string()
        );
        // deepEqual: two distinct-but-equal objects
        let o2 = obj(vec![
            ("a", Value::Number(1.0)),
            ("b", Value::Number(2.0)),
            ("c", Value::Number(3.0)),
        ]);
        assert_eq!(
            call("deepEqual", &[o.clone(), o2], sp()).unwrap(),
            Value::Bool(true)
        );
        // deepEqual false on difference
        let o3 = obj(vec![("a", Value::Number(1.0))]);
        assert_eq!(
            call("deepEqual", &[o.clone(), o3], sp()).unwrap(),
            Value::Bool(false)
        );
        // deepClone makes an independent, structurally-equal copy
        let cloned = call("deepClone", std::slice::from_ref(&o), sp()).unwrap();
        assert_eq!(
            call("deepEqual", &[o.clone(), cloned], sp()).unwrap(),
            Value::Bool(true)
        );
        // nested deepEqual + deepClone independence
        let nested = obj(vec![(
            "inner",
            arr(vec![Value::Number(1.0), Value::Number(2.0)]),
        )]);
        let nclone = call("deepClone", std::slice::from_ref(&nested), sp()).unwrap();
        assert_eq!(
            call("deepEqual", &[nested, nclone], sp()).unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn deep_clone_and_equal_handle_cycles() {
        // self-referential array: a = [a]
        let a: gcmodule::Cc<crate::value::ArrayCell> = crate::value::ArrayCell::new(Vec::new());
        let arr_a = Value::Array(a.clone());
        a.borrow_mut().push(arr_a.clone());
        // deep_clone terminates and yields a distinct (by identity) container
        let mut seen = std::collections::HashMap::new();
        let cloned = deep_clone(&arr_a, &mut seen);
        assert!(
            !matches!((&cloned, &arr_a), (Value::Array(c), Value::Array(o)) if crate::gc::cc_ptr_eq(c, o))
        );
        // deep_equal on the cyclic structure vs itself terminates and is true
        assert!(
            call("deepEqual", &[arr_a.clone(), arr_a.clone()], sp()).unwrap() == Value::Bool(true)
        );
    }

    // ---- freeze / isFrozen (SP2 §4) ----

    #[test]
    fn freeze_sets_flag_returns_value_and_isfrozen_tracks() {
        let o = obj(vec![("a", Value::Number(1.0))]);
        // isFrozen false before
        assert_eq!(
            call("isFrozen", std::slice::from_ref(&o), sp()).unwrap(),
            Value::Bool(false)
        );
        // freeze returns the SAME value (identity) for chaining
        let r = call("freeze", std::slice::from_ref(&o), sp()).unwrap();
        assert!(matches!((&r, &o), (Value::Object(a), Value::Object(b)) if crate::gc::cc_ptr_eq(a, b)));
        // isFrozen true after, and the underlying cell flag is set
        assert_eq!(
            call("isFrozen", std::slice::from_ref(&o), sp()).unwrap(),
            Value::Bool(true)
        );
        if let Value::Object(cell) = &o {
            assert!(cell.is_frozen());
        }
    }

    #[test]
    fn freeze_each_container_kind() {
        // array
        let a = Value::Array(crate::value::ArrayCell::new(vec![Value::Number(1.0)]));
        call("freeze", std::slice::from_ref(&a), sp()).unwrap();
        assert!(crate::value::is_frozen_value(&a));
        assert_eq!(crate::value::frozen_kind(&a), Some("array"));
        // map
        let m = Value::Map(crate::value::MapCell::new(IndexMap::new()));
        call("freeze", std::slice::from_ref(&m), sp()).unwrap();
        assert_eq!(crate::value::frozen_kind(&m), Some("map"));
        // set
        let st = Value::Set(crate::value::SetCell::new(indexmap::IndexSet::new()));
        call("freeze", std::slice::from_ref(&st), sp()).unwrap();
        assert_eq!(crate::value::frozen_kind(&st), Some("set"));
    }

    #[test]
    fn freeze_noncontainer_is_noop() {
        let n = Value::Number(5.0);
        // no-op freeze returns the value unchanged
        assert_eq!(call("freeze", std::slice::from_ref(&n), sp()).unwrap(), n);
        // never reports frozen / kind
        assert_eq!(
            call("isFrozen", std::slice::from_ref(&n), sp()).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(crate::value::frozen_kind(&n), None);
    }

    #[test]
    fn freeze_is_idempotent() {
        let o = obj(vec![("a", Value::Number(1.0))]);
        call("freeze", std::slice::from_ref(&o), sp()).unwrap();
        call("freeze", std::slice::from_ref(&o), sp()).unwrap();
        assert!(crate::value::is_frozen_value(&o));
    }

    #[test]
    fn deep_clone_of_frozen_is_unfrozen() {
        let o = obj(vec![("a", Value::Number(1.0))]);
        crate::value::freeze_value(&o);
        let mut seen = HashMap::new();
        let c = deep_clone(&o, &mut seen);
        // the clone is a fresh, UNFROZEN container (JS semantics)
        assert!(!crate::value::is_frozen_value(&c));
    }

    #[test]
    fn pick_follows_keylist_order() {
        let o = obj(vec![
            ("a", Value::Number(1.0)),
            ("b", Value::Number(2.0)),
            ("c", Value::Number(3.0)),
        ]);
        let keys = arr(vec![s("c"), s("a")]);
        assert_eq!(
            call("pick", &[o, keys], sp()).unwrap().to_string(),
            obj(vec![("c", Value::Number(3.0)), ("a", Value::Number(1.0))]).to_string()
        );
    }

    /// Compile a single AScript expression to a runtime `Value` (callback helper).
    /// Mirrors the same idiom used in `array.rs` tests.
    async fn val(interp: &Interp, src: &str) -> Value {
        let program = format!("let __v = {};", src);
        let tokens = crate::lexer::lex(&program).expect("lex");
        let stmts = crate::parser::parse(&tokens).expect("parse");
        let env = crate::interp::global_env().child();
        interp.exec(&stmts, &env).await.expect("exec");
        env.get("__v").expect("binding")
    }

    #[tokio::test]
    async fn map_values_routes_and_panics_on_noncallable() {
        let interp = Interp::new();
        let o = obj(vec![("a", Value::Number(1.0)), ("b", Value::Number(2.0))]);
        // A non-callable callback must produce a Tier-2 panic — proves that
        // call_object routing reaches call_value rather than "unknown function".
        let r = interp
            .call_object("mapValues", &[o, Value::Number(0.0)], sp())
            .await;
        assert!(matches!(r, Err(Control::Panic(_))));
    }

    #[tokio::test]
    async fn map_values_doubles() {
        let interp = Interp::new();
        // callback: (v, k) => v * 2
        let f = val(&interp, "(v, k) => v * 2").await;
        let o = obj(vec![("a", Value::Number(1.0)), ("b", Value::Number(2.0))]);
        let result = interp
            .call_object("mapValues", &[o, f], sp())
            .await
            .unwrap();
        assert_eq!(result.to_string(), "{a: 2, b: 4}");
    }

    #[tokio::test]
    async fn map_values_receives_key() {
        let interp = Interp::new();
        // callback: (v, k) => k — maps every value to its own key name
        let f = val(&interp, "(v, k) => k").await;
        let o = obj(vec![("x", Value::Number(99.0))]);
        let result = interp
            .call_object("mapValues", &[o, f], sp())
            .await
            .unwrap();
        assert_eq!(result.to_string(), "{x: \"x\"}");
    }

    #[tokio::test]
    async fn map_values_empty_object() {
        let interp = Interp::new();
        let f = val(&interp, "(v) => v").await;
        let o = obj(vec![]);
        let result = interp
            .call_object("mapValues", &[o, f], sp())
            .await
            .unwrap();
        assert_eq!(result.to_string(), "{}");
    }

    #[tokio::test]
    async fn call_object_delegates_pure_fns() {
        let interp = Interp::new();
        let o = obj(vec![("a", Value::Number(1.0)), ("b", Value::Number(2.0))]);
        // keys/values/entries etc. must still work through call_object
        let keys = interp
            .call_object("keys", std::slice::from_ref(&o), sp())
            .await
            .unwrap();
        assert_eq!(keys.to_string(), "[\"a\", \"b\"]");
        let vals = interp
            .call_object("values", std::slice::from_ref(&o), sp())
            .await
            .unwrap();
        assert_eq!(vals.to_string(), "[1, 2]");
        // unknown function still panics
        assert!(matches!(
            interp.call_object("nope", &[o], sp()).await,
            Err(Control::Panic(_))
        ));
    }

    /// Run a full multi-statement AScript program and retrieve a named binding.
    async fn prog(interp: &Interp, src: &str, binding: &str) -> Value {
        let tokens = crate::lexer::lex(src).expect("lex");
        let stmts = crate::parser::parse(&tokens).expect("parse");
        let env = crate::interp::global_env().child();
        interp.exec(&stmts, &env).await.expect("exec");
        env.get(binding).expect("binding")
    }

    #[tokio::test]
    async fn object_fns_accept_instances() {
        let interp = Interp::new();
        // Build a class with default fields and construct an instance.
        let inst = prog(
            &interp,
            r#"class P { name: string = "x"
age: number = 3 }
let __inst = P()"#,
            "__inst",
        )
        .await;
        // Confirm we actually got an Instance value
        assert!(
            matches!(inst, Value::Instance(_)),
            "expected Instance, got: {inst}"
        );

        // pick on an instance -> Object with only picked fields
        let picked = interp
            .call_object("pick", &[inst.clone(), arr(vec![s("name")])], sp())
            .await
            .unwrap();
        assert_eq!(picked.to_string(), obj(vec![("name", s("x"))]).to_string());

        // omit on an instance -> Object without omitted field
        let omitted = interp
            .call_object("omit", &[inst.clone(), arr(vec![s("name")])], sp())
            .await
            .unwrap();
        assert_eq!(
            omitted.to_string(),
            obj(vec![("age", Value::Number(3.0))]).to_string()
        );

        // mapValues on an instance -> Object with mapped values
        let f = val(&interp, "(v, k) => k").await;
        let mapped = interp
            .call_object("mapValues", &[inst.clone(), f], sp())
            .await
            .unwrap();
        // Each value maps to its key name
        assert_eq!(
            mapped.to_string(),
            obj(vec![("name", s("name")), ("age", s("age"))]).to_string()
        );

        // a non-object/instance still produces a Tier-2 panic
        assert!(matches!(
            interp
                .call_object("pick", &[Value::Number(1.0), arr(vec![])], sp())
                .await,
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            interp
                .call_object("omit", &[Value::Number(1.0), arr(vec![])], sp())
                .await,
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            interp
                .call_object(
                    "mapValues",
                    &[Value::Number(1.0), val(&interp, "(v) => v").await],
                    sp()
                )
                .await,
            Err(Control::Panic(_))
        ));
    }
}
