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
use crate::value::{Value, ValueKind};
use ciborium::value::Value as Cb;
use indexmap::IndexMap;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("encode", bi("cbor.encode")), ("decode", bi("cbor.decode"))]
}

/// AScript `Value` → `ciborium::value::Value`. Err(String) on an unrepresentable
/// handle or a reference cycle.
pub(crate) fn to_cbor(v: &Value, seen: &mut Vec<usize>) -> Result<Cb, String> {
    match v.kind() {
        ValueKind::Nil => Ok(Cb::Null),
        ValueKind::Bool(b) => Ok(Cb::Bool(b)),
        // NUM §4: an `Int` encodes as a CBOR integer directly.
        ValueKind::Int(i) => Ok(Cb::Integer(i.into())),
        ValueKind::Float(n) => Ok(number_to_cbor(n)),
        ValueKind::Decimal(d) => Ok(Cb::Text(d.to_string())),
        ValueKind::Str(s) => Ok(Cb::Text(s.to_string())),
        ValueKind::Bytes(b) => Ok(Cb::Bytes(b.borrow().clone())),
        ValueKind::Array(a) => {
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
        ValueKind::Set(s) => {
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
        ValueKind::Object(o) => {
            let ptr = crate::gc::cc_addr(o);
            if seen.contains(&ptr) {
                return Err("cannot serialize a cyclic structure to CBOR".into());
            }
            seen.push(ptr);
            let mut pairs = Vec::new();
            o.try_for_each::<String, _>(|k, val| {
                pairs.push((Cb::Text(k.to_string()), to_cbor(val, seen)?));
                Ok(())
            })?;
            seen.pop();
            Ok(Cb::Map(pairs))
        }
        ValueKind::Map(m) => {
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
        // SRV §3: a frozen value encodes like its underlying kind. Frozen containers
        // materialize one level + recurse; instance/enum-variant/regex error like a
        // live one (no live arm exists for them either).
        ValueKind::Shared(node) => match crate::interp::shared_to_value_shallow(node) {
            Some(live) => to_cbor(&live, seen),
            None => Err(format!(
                "cannot serialize a value of type {} to CBOR",
                node.kind_name()
            )),
        },
        _ => Err(format!(
            "cannot serialize a value of type {} to CBOR",
            crate::interp::type_name(v)
        )),
    }
}

/// Encode a number: integer-valued in i64/u64 range → integer, else → float.
fn number_to_cbor(n: f64) -> Cb {
    // STRICT upper bound: `u64::MAX as f64` rounds UP to 2^64 (out of u64 range), so
    // `<=` would admit 2^64 and `n as u64` would saturate. `2.0^64` is exact; `<`
    // excludes it (an out-of-range integral float falls through to `Cb::Float`). The
    // negative branch only reaches `n as i64` for n ≥ i64::MIN (≥ -2^63), in range.
    if n.fract() == 0.0 && n.is_finite() && n >= i64::MIN as f64 && n < 2.0_f64.powi(64) {
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
        Cb::Null => Value::nil(),
        // Undefined has no AScript equivalent → nil.
        Cb::Bool(b) => Value::bool_(*b),
        Cb::Integer(i) => {
            // NUM §4: a CBOR integer decodes to `Int` when it fits `i64`; a value
            // outside `i64` range is preserved as `Float` (the only lossy edge).
            let n: i128 = (*i).into();
            if n >= i64::MIN as i128 && n <= i64::MAX as i128 {
                Value::int(n as i64)
            } else {
                Value::float(n as f64)
            }
        }
        Cb::Float(f) => Value::float(*f),
        Cb::Text(s) => Value::str(s.as_str()),
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
        _ => Value::nil(),
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
                Ok(cb) => Ok(make_pair(from_cbor(&cb), Value::nil())),
                Err(e) => Ok(make_pair(
                    Value::nil(),
                    make_error(Value::str(format!("invalid CBOR: {}", e))),
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
        match pair.kind() {
            ValueKind::Array(a) => {
                let b = a.borrow();
                assert_eq!(b[1], Value::nil(), "decode err should be nil");
                b[0].clone()
            }
            _ => panic!("decode did not return a pair"),
        }
    }

    #[test]
    fn roundtrip_primitives() {
        assert_eq!(roundtrip(Value::float(42.0)), Value::float(42.0));
        assert_eq!(roundtrip(Value::float(3.5)), Value::float(3.5));
        assert_eq!(roundtrip(Value::float(-7.0)), Value::float(-7.0));
        assert_eq!(roundtrip(Value::str("hi")), Value::str("hi"));
        assert_eq!(roundtrip(Value::bool_(false)), Value::bool_(false));
        assert_eq!(roundtrip(Value::nil()), Value::nil());
    }

    #[test]
    fn roundtrip_bytes() {
        let b = Value::bytes(vec![0, 1, 254, 255]);
        match roundtrip(b).kind() {
            ValueKind::Bytes(out) => assert_eq!(*out.borrow(), vec![0, 1, 254, 255]),
            other => panic!("expected bytes, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_nested_object() {
        let mut m = IndexMap::new();
        m.insert("ok".to_string(), Value::bool_(true));
        m.insert(
            "xs".to_string(),
            Value::array(vec![Value::float(1.0)]),
        );
        let obj = Value::object(m);
        match roundtrip(obj).kind() {
            ValueKind::Object(o) => {
                assert_eq!(o.get("ok"), Some(Value::bool_(true)));
            }
            other => panic!("expected object, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_number_keyed_map_stays_map() {
        let mut m: IndexMap<crate::value::MapKey, Value> = IndexMap::new();
        m.insert(
            crate::value::MapKey::from_value(&Value::float(7.0)).unwrap(),
            Value::str("seven"),
        );
        assert!(matches!(roundtrip(Value::map(m)).kind(), ValueKind::Map(_)));
    }

    #[test]
    fn malformed_bytes_is_tier1_err() {
        // 0x1f is a reserved/ill-formed additional-info → decode error.
        let bad = Value::bytes(vec![0x1f]);
        let pair = call("decode", &[bad], sp()).unwrap();
        if let ValueKind::Array(a) = pair.kind() {
            let b = a.borrow();
            assert_eq!(b[0], Value::nil());
            assert!(matches!(b[1].kind(), ValueKind::Object(_)), "err should be set");
        }
    }

    #[test]
    fn encode_function_is_tier2_panic() {
        assert!(call("encode", &[Value::builtin("math.abs")], sp()).is_err());
    }

    #[test]
    fn fixture_decodes_to_expected() {
        // Canonical CBOR: map{1} "a":1 → a1 61 61 01
        let fixture = Value::bytes(vec![0xa1, 0x61, 0x61, 0x01]);
        let pair = call("decode", &[fixture], sp()).unwrap();
        if let ValueKind::Array(a) = pair.kind() {
            let b = a.borrow();
            match b[0].kind() {
                ValueKind::Object(o) => assert_eq!(o.get("a"), Some(Value::float(1.0))),
                other => panic!("expected object, got {:?}", other),
            }
        }
    }

    // SRV regression (holistic-review MAJOR): a frozen container must encode like its
    // live equivalent (before the fix, `to_cbor` errored on any `Value::Shared`).
    #[cfg(feature = "shared")]
    #[test]
    fn frozen_container_encodes_like_live() {
        use crate::stdlib::shared;
        let mut m = indexmap::IndexMap::new();
        m.insert("a".to_string(), Value::int(1));
        m.insert(
            "xs".to_string(),
            Value::array(vec![Value::int(2), Value::int(3)]),
        );
        let live = Value::object(m);
        let frozen = shared::freeze(&live, sp()).unwrap();
        // Encoding is deterministic: the frozen object must encode to the SAME bytes
        // as the live one (Value::Object `==` is identity, so compare the bytes).
        let bytes_of = |v: Value| match call("encode", &[v], sp()).unwrap().kind() {
            ValueKind::Bytes(b) => b.borrow().clone(),
            _ => panic!("encode did not return bytes"),
        };
        assert_eq!(bytes_of(live.clone()), bytes_of(frozen));
        assert!(matches!(roundtrip(live).kind(), ValueKind::Object(_)));
        assert!(call("encode", &[shared::freeze(&Value::int(7), sp()).unwrap()], sp()).is_ok());
    }
}
