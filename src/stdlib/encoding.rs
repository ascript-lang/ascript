//! `std/encoding` — base64, hex, url percent-encoding, utf8<->bytes.

use super::{arg, bi, want_bytes, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use base64::Engine;
use std::cell::RefCell;
use std::rc::Rc;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("base64Encode", bi("encoding.base64Encode")),
        ("base64Decode", bi("encoding.base64Decode")),
        ("hexEncode", bi("encoding.hexEncode")),
        ("hexDecode", bi("encoding.hexDecode")),
        ("urlEncode", bi("encoding.urlEncode")),
        ("urlDecode", bi("encoding.urlDecode")),
        ("utf8Encode", bi("encoding.utf8Encode")),
        ("utf8Decode", bi("encoding.utf8Decode")),
    ]
}

fn bytes_val(v: Vec<u8>) -> Value {
    Value::Bytes(Rc::new(RefCell::new(v)))
}

/// Accept bytes OR a string (encoded as UTF-8) as a source of raw bytes.
fn source_bytes(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v {
        Value::Bytes(b) => Ok(b.borrow().clone()),
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        _ => Err(AsError::at(
            format!(
                "{} expects bytes or a string, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("encoding.{}", f);
    match func {
        "base64Encode" => {
            let src = source_bytes(&arg(args, 0), span, &ctx("base64Encode"))?;
            Ok(Value::Str(
                base64::engine::general_purpose::STANDARD.encode(src).into(),
            ))
        }
        "base64Decode" => {
            let s = want_string(&arg(args, 0), span, &ctx("base64Decode"))?;
            match base64::engine::general_purpose::STANDARD.decode(s.as_bytes()) {
                Ok(bytes) => Ok(make_pair(bytes_val(bytes), Value::Nil)),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("invalid base64: {}", e).into())),
                )),
            }
        }
        "hexEncode" => {
            let src = source_bytes(&arg(args, 0), span, &ctx("hexEncode"))?;
            Ok(Value::Str(hex::encode(src).into()))
        }
        "hexDecode" => {
            let s = want_string(&arg(args, 0), span, &ctx("hexDecode"))?;
            match hex::decode(s.as_ref()) {
                Ok(bytes) => Ok(make_pair(bytes_val(bytes), Value::Nil)),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("invalid hex: {}", e).into())),
                )),
            }
        }
        "urlEncode" => {
            let s = want_string(&arg(args, 0), span, &ctx("urlEncode"))?;
            let encoded =
                percent_encoding::utf8_percent_encode(&s, percent_encoding::NON_ALPHANUMERIC)
                    .to_string();
            Ok(Value::Str(encoded.into()))
        }
        "urlDecode" => {
            let s = want_string(&arg(args, 0), span, &ctx("urlDecode"))?;
            match percent_encoding::percent_decode_str(&s).decode_utf8() {
                Ok(decoded) => Ok(make_pair(
                    Value::Str(decoded.into_owned().into()),
                    Value::Nil,
                )),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("invalid url encoding: {}", e).into())),
                )),
            }
        }
        "utf8Encode" => {
            let s = want_string(&arg(args, 0), span, &ctx("utf8Encode"))?;
            Ok(bytes_val(s.as_bytes().to_vec()))
        }
        "utf8Decode" => {
            let b = want_bytes(&arg(args, 0), span, &ctx("utf8Decode"))?;
            let raw = b.borrow().clone();
            match String::from_utf8(raw) {
                Ok(s) => Ok(make_pair(Value::Str(s.into()), Value::Nil)),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("invalid utf-8: {}", e).into())),
                )),
            }
        }
        _ => Err(AsError::at(format!("std/encoding has no function '{}'", func), span).into()),
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
    fn base64_hex_roundtrip() {
        let enc = call("base64Encode", &[s("hello")], sp()).unwrap();
        assert_eq!(enc, s("aGVsbG8="));
        let dec = call("base64Decode", std::slice::from_ref(&enc), sp()).unwrap();
        // dec = [bytes, nil]; decode back to utf8 to check
        assert!(dec.to_string().starts_with("[<bytes len 5>, nil]"));
        assert_eq!(call("hexEncode", &[s("AB")], sp()).unwrap(), s("4142"));
    }

    #[test]
    fn url_and_utf8() {
        assert_eq!(
            call("urlEncode", &[s("a b&c")], sp()).unwrap(),
            s("a%20b%26c")
        );
        assert_eq!(
            call("urlDecode", &[s("a%20b%26c")], sp())
                .unwrap()
                .to_string(),
            "[\"a b&c\", nil]"
        );
        let b = call("utf8Encode", &[s("hi")], sp()).unwrap();
        assert_eq!(crate::interp::type_name(&b), "bytes");
        assert_eq!(
            call("utf8Decode", std::slice::from_ref(&b), sp())
                .unwrap()
                .to_string(),
            "[\"hi\", nil]"
        );
    }

    #[test]
    fn bad_input_is_tier1_err() {
        assert!(call("base64Decode", &[s("!!!notb64")], sp())
            .unwrap()
            .to_string()
            .starts_with("[nil, {message:"));
        assert!(call("hexDecode", &[s("zz")], sp())
            .unwrap()
            .to_string()
            .starts_with("[nil, {message:"));
    }
}
