//! `std/msgpack` — MessagePack binary serialization, bridged through the
//! `rmpv::Value` dynamic model (no serde derive: AScript's `Value` is dynamic,
//! so we convert by hand, mirroring `std/json`'s `serde_json::Value` bridge).
//!
//! - `encode(value) -> bytes` — serialize a `Value` to MessagePack bytes. This
//!   is a TOTAL data mapping: it only fails (Tier-2 panic) on a genuinely
//!   unrepresentable handle (function/native/future/etc.), never on data.
//! - `decode(bytes) -> [value, err]` — Tier-1; malformed input → err channel.
//! - `decode(bytes, Class|schema) -> [value, err]` — typed decode, routed in
//!   `call_stdlib` via the shared `typed_decode` helper (like json.parse).
//!
//! ## Value ↔ MessagePack mapping
//! | AScript        | MessagePack         | Decode back            |
//! |----------------|---------------------|------------------------|
//! | `Number` (int) | integer             | `Number`               |
//! | `Number` (frac)| float (f64)         | `Number`               |
//! | `Str`          | string              | `Str`                  |
//! | `Bool`/`Nil`   | bool / nil          | `Bool`/`Nil`           |
//! | `Bytes`        | binary              | `Bytes`                |
//! | `Array`/`Set`  | array               | `Array`                |
//! | `Object`/`Map` | map                 | `Object` if all keys are strings, else `Map` |
//!
//! A `Number` that round-trips losslessly as an `i64`/`u64` integer is encoded as
//! an integer, else as a float — matching AScript's own integer-valued-float
//! display convention.

use super::{arg, bi, want_bytes};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use indexmap::IndexMap;
use rmpv::Value as Mp;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("encode", bi("msgpack.encode")), ("decode", bi("msgpack.decode"))]
}

/// AScript `Value` → `rmpv::Value`. Err(String) on an unrepresentable handle or
/// a reference cycle (`seen` tracks Array/Object/Map/Set Cc pointers in progress).
pub(crate) fn to_mp(v: &Value, seen: &mut Vec<usize>) -> Result<Mp, String> {
    match v {
        Value::Nil => Ok(Mp::Nil),
        Value::Bool(b) => Ok(Mp::Boolean(*b)),
        // NUM §4: an `Int` encodes as a MessagePack integer directly.
        Value::Int(i) => Ok(Mp::Integer((*i).into())),
        Value::Float(n) => Ok(number_to_mp(*n)),
        Value::Decimal(d) => Ok(Mp::String(d.to_string().into())),
        Value::Str(s) => Ok(Mp::String(s.to_string().into())),
        Value::Bytes(b) => Ok(Mp::Binary(b.borrow().clone())),
        Value::Array(a) => {
            let ptr = crate::gc::cc_addr(a);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to MessagePack".into());
            }
            seen.push(ptr);
            let mut out = Vec::new();
            for item in a.borrow().iter() {
                out.push(to_mp(item, seen)?);
            }
            seen.pop();
            Ok(Mp::Array(out))
        }
        Value::Set(s) => {
            let ptr = crate::gc::cc_addr(s);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to MessagePack".into());
            }
            seen.push(ptr);
            let mut out = Vec::new();
            for k in s.borrow().iter() {
                out.push(to_mp(&k.to_value(), seen)?);
            }
            seen.pop();
            Ok(Mp::Array(out))
        }
        Value::Object(o) => {
            let ptr = crate::gc::cc_addr(o);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to MessagePack".into());
            }
            seen.push(ptr);
            let mut pairs = Vec::new();
            for (k, val) in o.borrow().iter() {
                pairs.push((Mp::String(k.clone().into()), to_mp(val, seen)?));
            }
            seen.pop();
            Ok(Mp::Map(pairs))
        }
        Value::Map(m) => {
            let ptr = crate::gc::cc_addr(m);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to MessagePack".into());
            }
            seen.push(ptr);
            let mut pairs = Vec::new();
            for (k, val) in m.borrow().iter() {
                pairs.push((to_mp(&k.to_value(), seen)?, to_mp(val, seen)?));
            }
            seen.pop();
            Ok(Mp::Map(pairs))
        }
        other => Err(format!(
            "cannot serialize a value of type {} to MessagePack",
            crate::interp::type_name(other)
        )),
    }
}

/// Encode an AScript number: an integer-valued float in i64/u64 range becomes an
/// integer, otherwise a float (matches AScript's number display).
fn number_to_mp(n: f64) -> Mp {
    if n.fract() == 0.0 && n.is_finite() {
        if n >= 0.0 && n <= u64::MAX as f64 {
            return Mp::Integer((n as u64).into());
        }
        if n >= i64::MIN as f64 && n <= i64::MAX as f64 {
            return Mp::Integer((n as i64).into());
        }
    }
    Mp::F64(n)
}

/// `rmpv::Value` → AScript `Value`. A map decodes to an `Object` if every key is
/// a string, else to a `Map` (matching json's decode convention). Integers and
/// floats both become `Number`.
pub(crate) fn from_mp(mp: &Mp) -> Value {
    match mp {
        Mp::Nil => Value::Nil,
        Mp::Boolean(b) => Value::Bool(*b),
        Mp::Integer(i) => {
            // NUM §4: a MessagePack integer decodes to `Int` when it fits `i64`; a
            // `u64` value above `i64::MAX` is preserved as `Float` (the only lossy
            // edge — `Int` is `i64`).
            if let Some(s) = i.as_i64() {
                Value::Int(s)
            } else if let Some(u) = i.as_u64() {
                Value::Float(u as f64)
            } else {
                Value::Float(f64::NAN)
            }
        }
        Mp::F32(f) => Value::Float(*f as f64),
        Mp::F64(f) => Value::Float(*f),
        Mp::String(s) => match s.as_str() {
            Some(st) => Value::Str(st.into()),
            // Non-UTF-8 msgpack string → expose the raw bytes.
            None => Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(
                s.as_bytes().to_vec(),
            ))),
        },
        Mp::Binary(b) => Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(b.clone()))),
        Mp::Array(a) => {
            Value::Array(crate::value::ArrayCell::new(a.iter().map(from_mp).collect()))
        }
        Mp::Map(pairs) => {
            // All-string keys → Object; otherwise → Map.
            let all_strings = pairs.iter().all(|(k, _)| matches!(k, Mp::String(s) if s.as_str().is_some()));
            if all_strings {
                let mut m = IndexMap::new();
                for (k, v) in pairs {
                    if let Mp::String(s) = k {
                        m.insert(s.as_str().unwrap_or_default().to_string(), from_mp(v));
                    }
                }
                Value::Object(crate::value::ObjectCell::new(m))
            } else {
                let mut m: IndexMap<crate::value::MapKey, Value> = IndexMap::new();
                for (k, v) in pairs {
                    if let Some(mk) = crate::value::MapKey::from_value(&from_mp(k)) {
                        m.insert(mk, from_mp(v));
                    }
                }
                Value::Map(crate::value::MapCell::new(m))
            }
        }
        // Extension types are not part of AScript's value model → represent the
        // payload bytes (the type tag is dropped — documented).
        Mp::Ext(_, data) => Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(data.clone()))),
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "encode" => {
            let v = arg(args, 0);
            let mp = to_mp(&v, &mut Vec::new()).map_err(|e| AsError::at(e, span))?;
            let mut buf = Vec::new();
            match rmpv::encode::write_value(&mut buf, &mp) {
                Ok(()) => Ok(Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(buf)))),
                Err(e) => Err(AsError::at(
                    format!("msgpack.encode: {}", e),
                    span,
                )
                .into()),
            }
        }
        "decode" => {
            let bytes = want_bytes(&arg(args, 0), span, "msgpack.decode")?;
            let buf = bytes.borrow();
            let mut slice: &[u8] = &buf;
            match rmpv::decode::read_value(&mut slice) {
                Ok(mp) => Ok(make_pair(from_mp(&mp), Value::Nil)),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("invalid MessagePack: {}", e).into())),
                )),
            }
        }
        _ => Err(AsError::at(format!("std/msgpack has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    /// encode→decode round-trip equality check for a single value.
    fn roundtrip(v: Value) -> Value {
        let bytes = call("encode", &[v], sp()).unwrap();
        let pair = call("decode", &[bytes], sp()).unwrap();
        match pair {
            Value::Array(a) => {
                let b = a.borrow();
                assert_eq!(b[1], Value::Nil, "decode err should be nil");
                b[0].clone()
            }
            _ => panic!("decode did not return a pair"),
        }
    }

    #[test]
    fn roundtrip_primitives() {
        assert_eq!(roundtrip(Value::Float(42.0)), Value::Float(42.0));
        assert_eq!(roundtrip(Value::Float(3.5)), Value::Float(3.5));
        assert_eq!(roundtrip(Value::Float(-7.0)), Value::Float(-7.0));
        assert_eq!(roundtrip(Value::Str("hello".into())), Value::Str("hello".into()));
        assert_eq!(roundtrip(Value::Bool(true)), Value::Bool(true));
        assert_eq!(roundtrip(Value::Nil), Value::Nil);
    }

    #[test]
    fn roundtrip_bytes() {
        let b = Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(vec![1, 2, 3, 255])));
        match roundtrip(b) {
            Value::Bytes(out) => assert_eq!(*out.borrow(), vec![1, 2, 3, 255]),
            other => panic!("expected bytes, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_nested_array_object() {
        let mut m = IndexMap::new();
        m.insert("name".to_string(), Value::Str("Ada".into()));
        m.insert(
            "nums".to_string(),
            Value::Array(crate::value::ArrayCell::new(vec![
                Value::Float(1.0),
                Value::Float(2.0),
            ])),
        );
        let obj = Value::Object(crate::value::ObjectCell::new(m));
        let out = roundtrip(obj);
        match out {
            Value::Object(o) => {
                let b = o.borrow();
                assert_eq!(b.get("name"), Some(&Value::Str("Ada".into())));
                match b.get("nums") {
                    Some(Value::Array(a)) => assert_eq!(a.borrow().len(), 2),
                    other => panic!("nums not an array: {:?}", other),
                }
            }
            other => panic!("expected object, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_map_with_number_keys_stays_map() {
        let mut m: IndexMap<crate::value::MapKey, Value> = IndexMap::new();
        m.insert(
            crate::value::MapKey::from_value(&Value::Float(1.0)).unwrap(),
            Value::Str("one".into()),
        );
        let map = Value::Map(crate::value::MapCell::new(m));
        // number-keyed map → msgpack map with non-string keys → decodes to Map.
        assert!(matches!(roundtrip(map), Value::Map(_)));
    }

    #[test]
    fn malformed_bytes_is_tier1_err() {
        // A fixarray header claiming 5 elements but with no element bytes that
        // follow → a truncated/invalid stream → decode error.
        let bad = Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(vec![0x95])));
        let pair = call("decode", &[bad], sp()).unwrap();
        match pair {
            Value::Array(a) => {
                let b = a.borrow();
                assert_eq!(b[0], Value::Nil);
                assert!(matches!(b[1], Value::Object(_)), "err should be set");
            }
            _ => panic!("expected pair"),
        }
    }

    #[test]
    fn encode_function_is_tier2_panic() {
        let f = Value::Builtin("math.abs".into());
        assert!(call("encode", &[f], sp()).is_err());
    }

    #[test]
    fn fixture_decodes_to_expected() {
        // Canonical MessagePack: fixmap{1} "a":1 → 81 a1 61 01
        let fixture = Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(vec![
            0x81, 0xa1, 0x61, 0x01,
        ])));
        let pair = call("decode", &[fixture], sp()).unwrap();
        if let Value::Array(a) = pair {
            let b = a.borrow();
            match &b[0] {
                Value::Object(o) => {
                    assert_eq!(o.borrow().get("a"), Some(&Value::Float(1.0)))
                }
                other => panic!("expected object, got {:?}", other),
            }
        }
    }
}
