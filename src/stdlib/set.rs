//! `std/set` — insertion-ordered hash set of hashable values.
//! Mirrors `std/map` (pure `call` dispatch, no feature gate, no callback args).

use super::{arg, bi};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::{MapKey, Value, ValueKind};
use indexmap::IndexSet;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("new", bi("set.new")),
        ("from", bi("set.from")),
        ("add", bi("set.add")),
        ("has", bi("set.has")),
        ("delete", bi("set.delete")),
        ("size", bi("set.size")),
        ("values", bi("set.values")),
        ("union", bi("set.union")),
        ("intersection", bi("set.intersection")),
        ("difference", bi("set.difference")),
    ]
}

fn want_set(
    v: &Value,
    span: Span,
    ctx: &str,
) -> Result<gcmodule::Cc<crate::value::SetCell>, Control> {
    match v.kind() {
        ValueKind::Set(s) => Ok(s.clone()),
        _ => Err(AsError::at(
            format!("{} expects a set, got {}", ctx, crate::interp::type_name(v)),
            span,
        )
        .into()),
    }
}

fn want_element(v: &Value, span: Span, ctx: &str) -> Result<MapKey, Control> {
    MapKey::from_value(v).ok_or_else(|| {
        AsError::at(
            format!(
                "{}: set elements must be nil, bool, number, or string, got {}",
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

fn empty_set() -> Value {
    Value::set(IndexSet::new())
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("set.{}", f);
    match func {
        "new" => Ok(empty_set()),

        "from" => {
            // Build a set from an array, deduplicating elements.
            let s = crate::value::SetCell::new(IndexSet::new());
            let seed = arg(args, 0);
            if !matches!(seed.kind(), ValueKind::Nil) {
                let elements = match seed.kind() {
                    ValueKind::Array(a) => a.borrow().clone(),
                    _ => {
                        return Err(AsError::at(
                            format!(
                                "set.from expects an array, got {}",
                                crate::interp::type_name(&seed)
                            ),
                            span,
                        )
                        .into())
                    }
                };
                for el in elements {
                    let key = want_element(&el, span, "set.from")?;
                    s.borrow_mut().insert(key);
                }
            }
            Ok(Value::set_cell(s))
        }

        "add" => {
            let s = want_set(&arg(args, 0), span, &ctx("add"))?;
            crate::interp::check_not_frozen(&arg(args, 0), span)?;
            let key = want_element(&arg(args, 1), span, &ctx("add"))?;
            s.borrow_mut().insert(key);
            Ok(arg(args, 0)) // return the set (chainable)
        }

        "has" => {
            let s = want_set(&arg(args, 0), span, &ctx("has"))?;
            let key = want_element(&arg(args, 1), span, &ctx("has"))?;
            let has = s.borrow().contains(&key);
            Ok(Value::bool_(has))
        }

        "delete" => {
            let s = want_set(&arg(args, 0), span, &ctx("delete"))?;
            crate::interp::check_not_frozen(&arg(args, 0), span)?;
            let key = want_element(&arg(args, 1), span, &ctx("delete"))?;
            let existed = s.borrow_mut().shift_remove(&key);
            Ok(Value::bool_(existed))
        }

        "size" => {
            let s = want_set(&arg(args, 0), span, &ctx("size"))?;
            // NUM §4: a count is an `Int`.
            let n = s.borrow().len() as i64;
            Ok(Value::int(n))
        }

        "values" => {
            let s = want_set(&arg(args, 0), span, &ctx("values"))?;
            let vals: Vec<Value> = s.borrow().iter().map(|k| k.to_value()).collect();
            Ok(arr(vals))
        }

        "union" => {
            // Returns a NEW set: all elements from a, then elements from b not in a.
            let a = want_set(&arg(args, 0), span, &ctx("union"))?;
            let b = want_set(&arg(args, 1), span, &ctx("union"))?;
            let mut out: IndexSet<MapKey> = a.borrow().clone();
            for k in b.borrow().iter() {
                out.insert(k.clone());
            }
            Ok(Value::set(out))
        }

        "intersection" => {
            // Returns a NEW set: elements in a that also exist in b (preserving a's order).
            let a = want_set(&arg(args, 0), span, &ctx("intersection"))?;
            let b = want_set(&arg(args, 1), span, &ctx("intersection"))?;
            let b_ref = b.borrow();
            let out: IndexSet<MapKey> = a
                .borrow()
                .iter()
                .filter(|k| b_ref.contains(*k))
                .cloned()
                .collect();
            Ok(Value::set(out))
        }

        "difference" => {
            // Returns a NEW set: elements in a that do NOT exist in b (a − b).
            let a = want_set(&arg(args, 0), span, &ctx("difference"))?;
            let b = want_set(&arg(args, 1), span, &ctx("difference"))?;
            let b_ref = b.borrow();
            let out: IndexSet<MapKey> = a
                .borrow()
                .iter()
                .filter(|k| !b_ref.contains(*k))
                .cloned()
                .collect();
            Ok(Value::set(out))
        }

        _ => Err(AsError::at(format!("std/set has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn mk_set(elems: &[Value]) -> Value {
        let s = crate::value::SetCell::new(IndexSet::new());
        for el in elems {
            s.borrow_mut().insert(MapKey::from_value(el).unwrap());
        }
        Value::set_cell(s)
    }

    // ---- new ----

    #[test]
    fn new_creates_empty_set() {
        let s = call("new", &[], sp()).unwrap();
        assert_eq!(
            call("size", std::slice::from_ref(&s), sp()).unwrap(),
            Value::float(0.0)
        );
    }

    // ---- from / dedup ----

    #[test]
    fn from_deduplicates() {
        let arr = Value::array(vec![
            Value::float(1.0),
            Value::float(1.0),
            Value::float(2.0),
        ]);
        let s = call("from", std::slice::from_ref(&arr), sp()).unwrap();
        assert_eq!(
            call("size", std::slice::from_ref(&s), sp()).unwrap(),
            Value::float(2.0)
        );
    }

    #[test]
    fn from_empty_array() {
        let arr = Value::array(vec![]);
        let s = call("from", std::slice::from_ref(&arr), sp()).unwrap();
        assert_eq!(
            call("size", std::slice::from_ref(&s), sp()).unwrap(),
            Value::float(0.0)
        );
    }

    #[test]
    fn from_non_array_panics() {
        assert!(matches!(
            call("from", &[Value::float(5.0)], sp()),
            Err(Control::Panic(_))
        ));
    }

    // ---- add / has / delete / size ----

    #[test]
    fn add_has_delete_size() {
        let s = call("new", &[], sp()).unwrap();
        // add "hello"
        call("add", &[s.clone(), Value::str("hello")], sp()).unwrap();
        // add 42
        call("add", &[s.clone(), Value::float(42.0)], sp()).unwrap();

        assert_eq!(
            call("size", std::slice::from_ref(&s), sp()).unwrap(),
            Value::float(2.0)
        );
        assert_eq!(
            call("has", &[s.clone(), Value::str("hello")], sp()).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            call("has", &[s.clone(), Value::float(42.0)], sp()).unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            call("has", &[s.clone(), Value::float(99.0)], sp()).unwrap(),
            Value::bool_(false)
        );

        // delete existing key → true
        assert_eq!(
            call("delete", &[s.clone(), Value::str("hello")], sp()).unwrap(),
            Value::bool_(true)
        );
        // delete non-existent → false
        assert_eq!(
            call("delete", &[s.clone(), Value::str("hello")], sp()).unwrap(),
            Value::bool_(false)
        );
        assert_eq!(
            call("size", std::slice::from_ref(&s), sp()).unwrap(),
            Value::float(1.0)
        );
    }

    #[test]
    fn add_is_chainable() {
        let s = call("new", &[], sp()).unwrap();
        let s2 = call("add", &[s.clone(), Value::float(1.0)], sp()).unwrap();
        // returned value is the SAME set (by identity)
        assert_eq!(s, s2);
    }

    #[test]
    fn add_duplicate_noop() {
        let s = call("new", &[], sp()).unwrap();
        call("add", &[s.clone(), Value::float(1.0)], sp()).unwrap();
        call("add", &[s.clone(), Value::float(1.0)], sp()).unwrap();
        assert_eq!(
            call("size", std::slice::from_ref(&s), sp()).unwrap(),
            Value::float(1.0)
        );
    }

    // ---- values (insertion order) ----

    #[test]
    fn values_insertion_order() {
        let s = call("new", &[], sp()).unwrap();
        call("add", &[s.clone(), Value::float(3.0)], sp()).unwrap();
        call("add", &[s.clone(), Value::float(1.0)], sp()).unwrap();
        call("add", &[s.clone(), Value::float(2.0)], sp()).unwrap();
        let vals = call("values", std::slice::from_ref(&s), sp()).unwrap();
        assert_eq!(vals.to_string(), "[3, 1, 2]");
    }

    // ---- non-hashable element panics ----

    #[test]
    fn non_hashable_add_panics() {
        let s = call("new", &[], sp()).unwrap();
        let bad = Value::array(vec![Value::float(1.0)]);
        assert!(matches!(
            call("add", &[s, bad], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn non_hashable_from_panics() {
        let arr = Value::array(vec![Value::array(vec![Value::float(1.0)])]);
        assert!(matches!(call("from", &[arr], sp()), Err(Control::Panic(_))));
    }

    #[test]
    fn non_hashable_has_panics() {
        let s = call("new", &[], sp()).unwrap();
        let bad = Value::array(vec![]);
        assert!(matches!(
            call("has", &[s, bad], sp()),
            Err(Control::Panic(_))
        ));
    }

    // ---- union / intersection / difference ----

    #[test]
    fn union_combines_and_deduplicates() {
        let a = mk_set(&[Value::float(1.0), Value::float(2.0), Value::float(3.0)]);
        let b = mk_set(&[Value::float(2.0), Value::float(3.0), Value::float(4.0)]);
        let u = call("union", &[a, b], sp()).unwrap();
        let vals = call("values", std::slice::from_ref(&u), sp()).unwrap();
        assert_eq!(vals.to_string(), "[1, 2, 3, 4]");
    }

    #[test]
    fn intersection_returns_common_elements() {
        let a = mk_set(&[Value::float(1.0), Value::float(2.0), Value::float(3.0)]);
        let b = mk_set(&[Value::float(2.0), Value::float(3.0), Value::float(4.0)]);
        let inter = call("intersection", &[a, b], sp()).unwrap();
        let vals = call("values", std::slice::from_ref(&inter), sp()).unwrap();
        assert_eq!(vals.to_string(), "[2, 3]");
    }

    #[test]
    fn difference_a_minus_b() {
        let a = mk_set(&[Value::float(1.0), Value::float(2.0), Value::float(3.0)]);
        let b = mk_set(&[Value::float(2.0), Value::float(3.0), Value::float(4.0)]);
        let diff = call("difference", &[a, b], sp()).unwrap();
        let vals = call("values", std::slice::from_ref(&diff), sp()).unwrap();
        assert_eq!(vals.to_string(), "[1]");
    }

    #[test]
    fn set_operations_return_new_sets() {
        // union/intersection/difference must NOT mutate the originals
        let a = mk_set(&[Value::float(1.0), Value::float(2.0)]);
        let b = mk_set(&[Value::float(2.0), Value::float(3.0)]);
        let _u = call("union", &[a.clone(), b.clone()], sp()).unwrap();
        let _i = call("intersection", &[a.clone(), b.clone()], sp()).unwrap();
        let _d = call("difference", &[a.clone(), b.clone()], sp()).unwrap();
        // original a still has exactly 2 elements
        assert_eq!(
            call("size", std::slice::from_ref(&a), sp()).unwrap(),
            Value::float(2.0)
        );
    }

    // ---- deep_equal order-independence ----

    #[test]
    fn deep_equal_order_independence() {
        // Two sets with the same elements in different insertion order must be deep_equal.
        let a = mk_set(&[Value::float(1.0), Value::float(2.0), Value::float(3.0)]);
        let b = mk_set(&[Value::float(3.0), Value::float(1.0), Value::float(2.0)]);
        assert!(crate::stdlib::object::deep_equal(&a, &b));
        // But a set with different elements is NOT equal.
        let c = mk_set(&[Value::float(1.0), Value::float(2.0)]);
        assert!(!crate::stdlib::object::deep_equal(&a, &c));
    }

    // ---- display ----

    #[test]
    fn set_display() {
        let s = mk_set(&[Value::float(1.0), Value::str("two")]);
        // format: set {1, "two"} — mirrors Map's `map {...}` (space before brace).
        assert_eq!(s.to_string(), "set {1, \"two\"}");
    }

    #[test]
    fn empty_set_display() {
        let s = call("new", &[], sp()).unwrap();
        assert_eq!(s.to_string(), "set {}");
    }
}
