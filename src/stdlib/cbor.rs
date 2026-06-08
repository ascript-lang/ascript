//! `std/cbor` — CBOR (RFC 8949) binary serialization, bridged through the
//! `ciborium::value::Value` dynamic model (no serde derive; hand-bridged like
//! `std/json` / `std/msgpack`).
//!
//! - `encode(value) -> bytes` — TOTAL data mapping; Tier-2 panic only on a
//!   genuinely unrepresentable handle (function/native/future/etc.).
//! - `decode(bytes) -> [value, err]` — Tier-1; malformed input → err channel.
//! - `decode(bytes, Class|schema) -> [value, err]` — typed decode via the shared
//!   `typed_decode` helper (routed in `call_stdlib`, like json.parse).
//!
//! ## Value ↔ CBOR mapping (same shape as msgpack)
//! Number(int)→integer · Number(frac)→float · Str→text · Bool/Nil → bool/null ·
//! Bytes→byte-string · Array/Set→array · Object/Map→map (decode → Object if all
//! keys are text, else Map). A CBOR tag is transparent (the inner value is used).

use super::{arg, bi, want_bytes};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use ciborium::value::Value as Cb;
use indexmap::IndexMap;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("encode", bi("cbor.encode")), ("decode", bi("cbor.decode"))]
}

/// AScript `Value` → `ciborium::value::Value`. Err(String) on an unrepresentable
/// handle or a reference cycle.
pub(crate) fn to_cbor(v: &Value, seen: &mut Vec<usize>) -> Result<Cb, String> {
    match v {
        Value::Nil => Ok(Cb::Null),
        Value::Bool(b) => Ok(Cb::Bool(*b)),
        Value::Float(n) => Ok(number_to_cbor(*n)),
        Value::Decimal(d) => Ok(Cb::Text(d.to_string())),
        Value::Str(s) => Ok(Cb::Text(s.to_string())),
        Value::Bytes(b) => Ok(Cb::Bytes(b.borrow().clone())),
        Value::Array(a) => {
            let ptr = crate::gc::cc_addr(a);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to CBOR".into());
            }
            seen.push(ptr);
            let mut out = Vec::new();
            for item in a.borrow().iter() {
                out.push(to_cbor(item, seen)?);
            }
            seen.pop();
            Ok(Cb::Array(out))
        }
        Value::Set(s) => {
            let ptr = crate::gc::cc_addr(s);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to CBOR".into());
            }
            seen.push(ptr);
            let mut out = Vec::new();
            for k in s.borrow().iter() {
                out.push(to_cbor(&k.to_value(), seen)?);
            }
            seen.pop();
            Ok(Cb::Array(out))
        }
        Value::Object(o) => {
            let ptr = crate::gc::cc_addr(o);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to CBOR".into());
            }
            seen.push(ptr);
            let mut pairs = Vec::new();
            for (k, val) in o.borrow().iter() {
                pairs.push((Cb::Text(k.clone()), to_cbor(val, seen)?));
            }
            seen.pop();
            Ok(Cb::Map(pairs))
        }
        Value::Map(m) => {
            let ptr = crate::gc::cc_addr(m);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to CBOR".into());
            }
            seen.push(ptr);
            let mut pairs = Vec::new();
            for (k, val) in m.borrow().iter() {
                pairs.push((to_cbor(&k.to_value(), seen)?, to_cbor(val, seen)?));
            }
            seen.pop();
            Ok(Cb::Map(pairs))
        }
        other => Err(format!(
            "cannot serialize a value of type {} to CBOR",
            crate::interp::type_name(other)
        )),
    }
}

/// Encode a number: integer-valued in i128 range → integer, else → float.
fn number_to_cbor(n: f64) -> Cb {
    if n.fract() == 0.0 && n.is_finite() && n >= i64::MIN as f64 && n <= u64::MAX as f64 {
        if n >= 0.0 {
            return Cb::Integer((n as u64).into());
        }
        return Cb::Integer((n as i64).into());
    }
    Cb::Float(n)
}

/// `ciborium::value::Value` → AScript `Value`. Map decodes to `Object` if every
/// key is text, else to `Map`.
pub(crate) fn from_cbor(cb: &Cb) -> Value {
    match cb {
        Cb::Null => Value::Nil,
        // Undefined has no AScript equivalent → nil.
        Cb::Bool(b) => Value::Bool(*b),
        Cb::Integer(i) => {
            let n: i128 = (*i).into();
            Value::Float(n as f64)
        }
        Cb::Float(f) => Value::Float(*f),
        Cb::Text(s) => Value::Str(s.as_str().into()),
        Cb::Bytes(b) => Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(b.clone()))),
        Cb::Array(a) => {
            Value::Array(crate::value::ArrayCell::new(a.iter().map(from_cbor).collect()))
        }
        Cb::Map(pairs) => {
            let all_text = pairs.iter().all(|(k, _)| matches!(k, Cb::Text(_)));
            if all_text {
                let mut m = IndexMap::new();
                for (k, v) in pairs {
                    if let Cb::Text(s) = k {
                        m.insert(s.clone(), from_cbor(v));
                    }
                }
                Value::Object(crate::value::ObjectCell::new(m))
            } else {
                let mut m: IndexMap<crate::value::MapKey, Value> = IndexMap::new();
                for (k, v) in pairs {
                    if let Some(mk) = crate::value::MapKey::from_value(&from_cbor(k)) {
                        m.insert(mk, from_cbor(v));
                    }
                }
                Value::Map(crate::value::MapCell::new(m))
            }
        }
        // A tagged value: use the inner value (the tag semantics are not modeled).
        Cb::Tag(_, inner) => from_cbor(inner),
        // ciborium::value::Value is non_exhaustive; any future variant → nil.
        _ => Value::Nil,
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "encode" => {
            let v = arg(args, 0);
            let cb = to_cbor(&v, &mut Vec::new()).map_err(|e| AsError::at(e, span))?;
            let mut buf = Vec::new();
            match ciborium::into_writer(&cb, &mut buf) {
                Ok(()) => Ok(Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(buf)))),
                Err(e) => Err(AsError::at(format!("cbor.encode: {}", e), span).into()),
            }
        }
        "decode" => {
            let bytes = want_bytes(&arg(args, 0), span, "cbor.decode")?;
            let buf = bytes.borrow();
            let slice: &[u8] = &buf;
            match ciborium::from_reader::<Cb, _>(slice) {
                Ok(cb) => Ok(make_pair(from_cbor(&cb), Value::Nil)),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("invalid CBOR: {}", e).into())),
                )),
            }
        }
        _ => Err(AsError::at(format!("std/cbor has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

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
        assert_eq!(roundtrip(Value::Str("hi".into())), Value::Str("hi".into()));
        assert_eq!(roundtrip(Value::Bool(false)), Value::Bool(false));
        assert_eq!(roundtrip(Value::Nil), Value::Nil);
    }

    #[test]
    fn roundtrip_bytes() {
        let b = Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(vec![0, 1, 254, 255])));
        match roundtrip(b) {
            Value::Bytes(out) => assert_eq!(*out.borrow(), vec![0, 1, 254, 255]),
            other => panic!("expected bytes, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_nested_object() {
        let mut m = IndexMap::new();
        m.insert("ok".to_string(), Value::Bool(true));
        m.insert(
            "xs".to_string(),
            Value::Array(crate::value::ArrayCell::new(vec![Value::Float(1.0)])),
        );
        let obj = Value::Object(crate::value::ObjectCell::new(m));
        match roundtrip(obj) {
            Value::Object(o) => {
                let b = o.borrow();
                assert_eq!(b.get("ok"), Some(&Value::Bool(true)));
            }
            other => panic!("expected object, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_number_keyed_map_stays_map() {
        let mut m: IndexMap<crate::value::MapKey, Value> = IndexMap::new();
        m.insert(
            crate::value::MapKey::from_value(&Value::Float(7.0)).unwrap(),
            Value::Str("seven".into()),
        );
        assert!(matches!(roundtrip(Value::Map(crate::value::MapCell::new(m))), Value::Map(_)));
    }

    #[test]
    fn malformed_bytes_is_tier1_err() {
        // 0x1f is a reserved/ill-formed additional-info → decode error.
        let bad = Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(vec![0x1f])));
        let pair = call("decode", &[bad], sp()).unwrap();
        if let Value::Array(a) = pair {
            let b = a.borrow();
            assert_eq!(b[0], Value::Nil);
            assert!(matches!(b[1], Value::Object(_)), "err should be set");
        }
    }

    #[test]
    fn encode_function_is_tier2_panic() {
        assert!(call("encode", &[Value::Builtin("math.abs".into())], sp()).is_err());
    }

    #[test]
    fn fixture_decodes_to_expected() {
        // Canonical CBOR: map{1} "a":1 → a1 61 61 01
        let fixture = Value::Bytes(std::rc::Rc::new(std::cell::RefCell::new(vec![
            0xa1, 0x61, 0x61, 0x01,
        ])));
        let pair = call("decode", &[fixture], sp()).unwrap();
        if let Value::Array(a) = pair {
            let b = a.borrow();
            match &b[0] {
                Value::Object(o) => assert_eq!(o.borrow().get("a"), Some(&Value::Float(1.0))),
                other => panic!("expected object, got {:?}", other),
            }
        }
    }
}
