//! `std/bytes` — a mutable byte buffer with int read/write and endian handling.

use super::{arg, bi, clamp_index, want_array, want_bytes, want_number};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("alloc", bi("bytes.alloc")),
        ("fromArray", bi("bytes.fromArray")),
        ("toArray", bi("bytes.toArray")),
        ("get", bi("bytes.get")),
        ("set", bi("bytes.set")),
        ("slice", bi("bytes.slice")),
        ("concat", bi("bytes.concat")),
        ("readUint", bi("bytes.readUint")),
        ("writeUint", bi("bytes.writeUint")),
        ("readInt", bi("bytes.readInt")),
        ("writeInt", bi("bytes.writeInt")),
    ]
}

fn bytes_val(v: Vec<u8>) -> Value {
    Value::Bytes(Rc::new(RefCell::new(v)))
}

/// A non-negative integer offset/index/size, validated BEFORE the f64→usize
/// cast so a pathological value (NaN, inf, 1e30) yields a clean AScript Tier-2
/// panic rather than a saturating cast that then overflows in `offset + n`.
fn want_index(v: &Value, span: Span, ctx: &str) -> Result<usize, Control> {
    let n = want_number(v, span, ctx)?;
    if !n.is_finite() || n.fract() != 0.0 || n < 0.0 || n > (u32::MAX as f64) {
        return Err(AsError::at(
            format!(
                "{}: expected a non-negative integer offset/size, got {}",
                ctx, n
            ),
            span,
        )
        .into());
    }
    Ok(n as usize)
}

fn want_byte(v: &Value, span: Span, ctx: &str) -> Result<u8, Control> {
    let n = want_number(v, span, ctx)?;
    if n.fract() != 0.0 || !(0.0..=255.0).contains(&n) {
        return Err(AsError::at(
            format!("{}: byte value must be an integer 0..=255, got {}", ctx, n),
            span,
        )
        .into());
    }
    Ok(n as u8)
}

fn want_endian(v: &Value, span: Span, ctx: &str) -> Result<bool /*little*/, Control> {
    match v {
        Value::Str(s) if s.as_ref() == "le" => Ok(true),
        Value::Str(s) if s.as_ref() == "be" => Ok(false),
        Value::Nil => Ok(false), // default big-endian (network order)
        _ => Err(AsError::at(format!("{}: endian must be \"le\" or \"be\"", ctx), span).into()),
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("bytes.{}", f);
    match func {
        "alloc" => {
            let n = want_index(&arg(args, 0), span, &ctx("alloc"))?;
            Ok(bytes_val(vec![0u8; n]))
        }
        "fromArray" => {
            let a = want_array(&arg(args, 0), span, &ctx("fromArray"))?;
            let mut out = Vec::with_capacity(a.borrow().len());
            for v in a.borrow().iter() {
                out.push(want_byte(v, span, &ctx("fromArray"))?);
            }
            Ok(bytes_val(out))
        }
        "toArray" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("toArray"))?;
            let arr: Vec<Value> = b
                .borrow()
                .iter()
                .map(|&x| Value::Number(x as f64))
                .collect();
            Ok(Value::Array(Rc::new(RefCell::new(arr))))
        }
        "get" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("get"))?;
            let i = want_number(&arg(args, 1), span, &ctx("get"))?;
            if i < 0.0 || i.fract() != 0.0 {
                return Ok(Value::Nil);
            }
            let out = b
                .borrow()
                .get(i as usize)
                .map(|&x| Value::Number(x as f64))
                .unwrap_or(Value::Nil);
            Ok(out)
        }
        "set" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("set"))?;
            let idx = want_index(&arg(args, 1), span, &ctx("set"))?;
            let v = want_byte(&arg(args, 2), span, &ctx("set"))?;
            let mut bb = b.borrow_mut();
            if idx >= bb.len() {
                return Err(AsError::at(
                    format!("bytes.set index {} out of bounds (len {})", idx, bb.len()),
                    span,
                )
                .into());
            }
            bb[idx] = v;
            Ok(Value::Nil)
        }
        "slice" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("slice"))?;
            let bb = b.borrow();
            let len = bb.len();
            let start = clamp_index(want_number(&arg(args, 1), span, &ctx("slice"))?, len);
            let end = match args.get(2) {
                None | Some(Value::Nil) => len,
                Some(v) => clamp_index(want_number(v, span, &ctx("slice"))?, len),
            };
            let out = if start < end {
                bb[start..end].to_vec()
            } else {
                Vec::new()
            };
            Ok(bytes_val(out))
        }
        "concat" => {
            let mut out = Vec::new();
            for (i, v) in args.iter().enumerate() {
                let b = want_bytes(v, span, &format!("{} (argument {})", ctx("concat"), i + 1))?;
                out.extend_from_slice(&b.borrow());
            }
            Ok(bytes_val(out))
        }
        "readUint" | "readInt" => {
            let b = want_bytes(&arg(args, 0), span, &ctx(func))?;
            let offset = want_index(&arg(args, 1), span, &ctx(func))?;
            let n = want_index(&arg(args, 2), span, &ctx(func))?;
            let little = want_endian(&arg(args, 3), span, &ctx(func))?;
            if !(1..=8).contains(&n) {
                return Err(
                    AsError::at(format!("{}: byte length must be 1..=8", ctx(func)), span).into(),
                );
            }
            let bb = b.borrow();
            if offset + n > bb.len() {
                return Err(AsError::at(format!("{}: read out of bounds", ctx(func)), span).into());
            }
            let mut buf = [0u8; 8];
            // Place the n source bytes into the low n bytes (little-endian) so
            // `from_le_bytes` yields the unsigned value regardless of source order.
            if little {
                buf[..n].copy_from_slice(&bb[offset..offset + n]);
            } else {
                for (i, &byte) in bb[offset..offset + n].iter().enumerate() {
                    buf[n - 1 - i] = byte;
                }
            }
            let raw = u64::from_le_bytes(buf);
            if func == "readUint" {
                Ok(Value::Number(raw as f64))
            } else {
                // sign-extend from the top bit of the n-byte value
                let bits = 8 * n as u32;
                let signed = if bits < 64 && (raw & (1 << (bits - 1))) != 0 {
                    (raw as i64) - (1i64 << bits)
                } else {
                    raw as i64
                };
                Ok(Value::Number(signed as f64))
            }
        }
        "writeUint" | "writeInt" => {
            let b = want_bytes(&arg(args, 0), span, &ctx(func))?;
            let offset = want_index(&arg(args, 1), span, &ctx(func))?;
            let value = want_number(&arg(args, 2), span, &ctx(func))?;
            let n = want_index(&arg(args, 3), span, &ctx(func))?;
            let little = want_endian(&arg(args, 4), span, &ctx(func))?;
            if !(1..=8).contains(&n) {
                return Err(
                    AsError::at(format!("{}: byte length must be 1..=8", ctx(func)), span).into(),
                );
            }
            if !value.is_finite() || value.fract() != 0.0 {
                return Err(AsError::at(
                    format!(
                        "{}: value must be a finite integer, got {}",
                        ctx(func),
                        value
                    ),
                    span,
                )
                .into());
            }
            let raw = if func == "writeUint" {
                if value < 0.0 {
                    return Err(
                        AsError::at("bytes.writeUint value must be non-negative", span).into(),
                    );
                }
                let raw = value as u64;
                // Reject values that don't fit in n bytes (n == 8 always fits in u64).
                if n < 8 && raw >= (1u64 << (8 * n)) {
                    return Err(AsError::at(
                        format!(
                            "bytes.writeUint: value {} does not fit in {} byte(s)",
                            raw, n
                        ),
                        span,
                    )
                    .into());
                }
                raw
            } else {
                // n == 8 always fits in i64; otherwise require -(2^(8n-1)) ..= 2^(8n-1)-1.
                if n < 8 {
                    let bits = 8 * n as u32;
                    let min = -(1i64 << (bits - 1));
                    let max = (1i64 << (bits - 1)) - 1;
                    let v = value as i64;
                    if v < min || v > max {
                        return Err(AsError::at(
                            format!("bytes.writeInt: value {} does not fit in {} byte(s)", v, n),
                            span,
                        )
                        .into());
                    }
                }
                (value as i64) as u64
            };
            let le = raw.to_le_bytes();
            let mut bb = b.borrow_mut();
            if offset + n > bb.len() {
                return Err(
                    AsError::at(format!("{}: write out of bounds", ctx(func)), span).into(),
                );
            }
            if little {
                bb[offset..offset + n].copy_from_slice(&le[..n]);
            } else {
                let be = raw.to_be_bytes();
                bb[offset..offset + n].copy_from_slice(&be[8 - n..]);
            }
            Ok(Value::Nil)
        }
        _ => Err(AsError::at(format!("std/bytes has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn num(n: f64) -> Value {
        Value::Number(n)
    }

    #[test]
    fn alloc_from_to_array_get_set() {
        let b = call("alloc", &[num(3.0)], sp()).unwrap();
        assert_eq!(
            call("toArray", std::slice::from_ref(&b), sp())
                .unwrap()
                .to_string(),
            "[0, 0, 0]"
        );
        call("set", &[b.clone(), num(1.0), num(255.0)], sp()).unwrap();
        assert_eq!(
            call("get", &[b.clone(), num(1.0)], sp()).unwrap(),
            num(255.0)
        );
        assert_eq!(
            call("get", &[b.clone(), num(9.0)], sp()).unwrap(),
            Value::Nil
        );
        // out-of-range byte → panic
        assert!(matches!(
            call("set", &[b.clone(), num(0.0), num(300.0)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn endian_roundtrip() {
        let b = call("alloc", &[num(4.0)], sp()).unwrap();
        call(
            "writeUint",
            &[
                b.clone(),
                num(0.0),
                num(0x01020304 as f64),
                num(4.0),
                Value::Str("be".into()),
            ],
            sp(),
        )
        .unwrap();
        assert_eq!(
            call("toArray", std::slice::from_ref(&b), sp())
                .unwrap()
                .to_string(),
            "[1, 2, 3, 4]"
        );
        assert_eq!(
            call(
                "readUint",
                &[b.clone(), num(0.0), num(4.0), Value::Str("be".into())],
                sp()
            )
            .unwrap(),
            num(0x01020304 as f64)
        );
        // little-endian write of the same value
        let b2 = call("alloc", &[num(4.0)], sp()).unwrap();
        call(
            "writeUint",
            &[
                b2.clone(),
                num(0.0),
                num(0x01020304 as f64),
                num(4.0),
                Value::Str("le".into()),
            ],
            sp(),
        )
        .unwrap();
        assert_eq!(
            call("toArray", std::slice::from_ref(&b2), sp())
                .unwrap()
                .to_string(),
            "[4, 3, 2, 1]"
        );
    }

    #[test]
    fn signed_roundtrip_and_concat() {
        let b = call("alloc", &[num(2.0)], sp()).unwrap();
        call(
            "writeInt",
            &[
                b.clone(),
                num(0.0),
                num(-1.0),
                num(2.0),
                Value::Str("be".into()),
            ],
            sp(),
        )
        .unwrap();
        assert_eq!(
            call("toArray", std::slice::from_ref(&b), sp())
                .unwrap()
                .to_string(),
            "[255, 255]"
        );
        assert_eq!(
            call(
                "readInt",
                &[b.clone(), num(0.0), num(2.0), Value::Str("be".into())],
                sp()
            )
            .unwrap(),
            num(-1.0)
        );
        let c = call("concat", &[b.clone(), b.clone()], sp()).unwrap();
        assert_eq!(crate::interp::type_name(&c), "bytes");
    }

    #[test]
    fn pathological_offset_is_clean_panic_not_abort() {
        let b = call("alloc", &[num(4.0)], sp()).unwrap();
        // A huge offset must NOT saturate-cast + overflow into a Rust abort;
        // it must be a clean AScript Tier-2 panic.
        assert!(matches!(
            call(
                "readUint",
                &[b.clone(), num(1e30), num(2.0), Value::Str("be".into())],
                sp()
            ),
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            call(
                "writeUint",
                &[
                    b.clone(),
                    num(1e30),
                    num(0.0),
                    num(2.0),
                    Value::Str("be".into())
                ],
                sp()
            ),
            Err(Control::Panic(_))
        ));
        // NaN / non-integer offsets too.
        assert!(matches!(
            call(
                "readUint",
                &[b.clone(), num(f64::NAN), num(2.0), Value::Str("be".into())],
                sp()
            ),
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            call("alloc", &[num(1e30)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn write_value_range_checks() {
        let b = call("alloc", &[num(1.0)], sp()).unwrap();
        // 256 doesn't fit in 1 byte → panic
        assert!(matches!(
            call(
                "writeUint",
                &[
                    b.clone(),
                    num(0.0),
                    num(256.0),
                    num(1.0),
                    Value::Str("be".into())
                ],
                sp()
            ),
            Err(Control::Panic(_))
        ));
        // 255 fits in 1 byte → ok
        call(
            "writeUint",
            &[
                b.clone(),
                num(0.0),
                num(255.0),
                num(1.0),
                Value::Str("be".into()),
            ],
            sp(),
        )
        .unwrap();
        assert_eq!(
            call("toArray", std::slice::from_ref(&b), sp())
                .unwrap()
                .to_string(),
            "[255]"
        );
        // 200 > 127 → out of range for a signed byte → panic
        assert!(matches!(
            call(
                "writeInt",
                &[
                    b.clone(),
                    num(0.0),
                    num(200.0),
                    num(1.0),
                    Value::Str("be".into())
                ],
                sp()
            ),
            Err(Control::Panic(_))
        ));
        // -128 is the min signed byte → ok
        call(
            "writeInt",
            &[
                b.clone(),
                num(0.0),
                num(-128.0),
                num(1.0),
                Value::Str("be".into()),
            ],
            sp(),
        )
        .unwrap();
        assert_eq!(
            call(
                "readInt",
                &[b.clone(), num(0.0), num(1.0), Value::Str("be".into())],
                sp()
            )
            .unwrap(),
            num(-128.0)
        );
        // -129 is out of range → panic
        assert!(matches!(
            call(
                "writeInt",
                &[
                    b.clone(),
                    num(0.0),
                    num(-129.0),
                    num(1.0),
                    Value::Str("be".into())
                ],
                sp()
            ),
            Err(Control::Panic(_))
        ));
    }
}
