//! `std/convert` — parsing and coercions.

use super::{arg, bi, want_number, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("parseNumber", bi("convert.parseNumber")),
        ("parseInt", bi("convert.parseInt")),
        ("toString", bi("convert.toString")),
        ("toNumber", bi("convert.toNumber")),
        ("toBool", bi("convert.toBool")),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("convert.{}", f);
    match func {
        "parseNumber" => {
            let s = want_string(&arg(args, 0), span, &ctx("parseNumber"))?;
            match s.trim().parse::<f64>() {
                Ok(n) => Ok(make_pair(Value::Number(n), Value::Nil)),
                Err(_) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("cannot parse '{}' as a number", s).into())),
                )),
            }
        }
        "parseInt" => {
            let s = want_string(&arg(args, 0), span, &ctx("parseInt"))?;
            let radix = match args.get(1) {
                None | Some(Value::Nil) => 10u32,
                Some(v) => want_number(v, span, &ctx("parseInt"))? as u32,
            };
            if !(2..=36).contains(&radix) {
                return Err(AsError::at("convert.parseInt radix must be between 2 and 36", span).into());
            }
            match i64::from_str_radix(s.trim(), radix) {
                Ok(n) => Ok(make_pair(Value::Number(n as f64), Value::Nil)),
                Err(_) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("cannot parse '{}' as an integer (radix {})", s, radix).into())),
                )),
            }
        }
        "toString" => Ok(Value::Str(arg(args, 0).to_string().into())),
        "toNumber" => {
            let v = arg(args, 0);
            let n = match &v {
                Value::Number(n) => *n,
                Value::Bool(b) => if *b { 1.0 } else { 0.0 },
                Value::Nil => 0.0,
                Value::Str(s) => match s.trim().parse::<f64>() {
                    Ok(n) => n,
                    Err(_) => return Err(AsError::at(format!("convert.toNumber: cannot coerce '{}' to a number", s), span).into()),
                },
                _ => return Err(AsError::at(format!("convert.toNumber: cannot coerce {} to a number", crate::interp::type_name(&v)), span).into()),
            };
            Ok(Value::Number(n))
        }
        "toBool" => Ok(Value::Bool(arg(args, 0).is_truthy())),
        _ => Err(AsError::at(format!("std/convert has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }

    #[test]
    fn parse_number_ok_and_err() {
        let ok = call("parseNumber", &[s("3.5")], sp()).unwrap();
        assert_eq!(ok.to_string(), "[3.5, nil]");
        let err = call("parseNumber", &[s("abc")], sp()).unwrap();
        assert!(err.to_string().starts_with("[nil, {message:"));
    }

    #[test]
    fn parse_int_radix() {
        assert_eq!(call("parseInt", &[s("ff"), Value::Number(16.0)], sp()).unwrap().to_string(), "[255, nil]");
        assert_eq!(call("parseInt", &[s("101"), Value::Number(2.0)], sp()).unwrap().to_string(), "[5, nil]");
        assert_eq!(call("parseInt", &[s("42")], sp()).unwrap().to_string(), "[42, nil]");
    }

    #[test]
    fn parse_int_bad_radix_panics() {
        assert!(matches!(call("parseInt", &[s("1"), Value::Number(99.0)], sp()), Err(Control::Panic(_))));
    }

    #[test]
    fn coercions() {
        assert_eq!(call("toString", &[Value::Number(7.0)], sp()).unwrap(), s("7"));
        assert_eq!(call("toNumber", &[Value::Bool(true)], sp()).unwrap(), Value::Number(1.0));
        assert_eq!(call("toNumber", &[s(" 42 ")], sp()).unwrap(), Value::Number(42.0));
        assert_eq!(call("toBool", &[Value::Number(0.0)], sp()).unwrap(), Value::Bool(true));
        assert_eq!(call("toBool", &[Value::Nil], sp()).unwrap(), Value::Bool(false));
    }

    #[test]
    fn to_number_uncoercible_panics() {
        assert!(matches!(call("toNumber", &[s("xyz")], sp()), Err(Control::Panic(_))));
    }
}
