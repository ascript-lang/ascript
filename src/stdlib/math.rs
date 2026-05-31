//! `std/math` — numeric functions and constants.

use super::{arg, bi, want_number};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use std::cell::Cell;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("abs", bi("math.abs")),
        ("floor", bi("math.floor")),
        ("ceil", bi("math.ceil")),
        ("round", bi("math.round")),
        ("sqrt", bi("math.sqrt")),
        ("pow", bi("math.pow")),
        ("min", bi("math.min")),
        ("max", bi("math.max")),
        ("random", bi("math.random")),
        ("sin", bi("math.sin")),
        ("cos", bi("math.cos")),
        ("tan", bi("math.tan")),
        ("asin", bi("math.asin")),
        ("acos", bi("math.acos")),
        ("atan", bi("math.atan")),
        ("atan2", bi("math.atan2")),
        ("exp", bi("math.exp")),
        ("ln", bi("math.ln")),
        ("log2", bi("math.log2")),
        ("log10", bi("math.log10")),
        ("pi", Value::Number(std::f64::consts::PI)),
        ("e", Value::Number(std::f64::consts::E)),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("math.{}", f);
    match func {
        "abs" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("abs"))?.abs())),
        "floor" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("floor"))?.floor())),
        "ceil" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("ceil"))?.ceil())),
        "round" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("round"))?.round())),
        "sqrt" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("sqrt"))?.sqrt())),
        "pow" => {
            let b = want_number(&arg(args, 0), span, &ctx("pow"))?;
            let e = want_number(&arg(args, 1), span, &ctx("pow"))?;
            Ok(Value::Number(b.powf(e)))
        }
        "min" | "max" => {
            if args.is_empty() {
                return Err(AsError::at(format!("math.{} requires at least one argument", func), span).into());
            }
            let nums: Result<Vec<f64>, Control> =
                args.iter().map(|v| want_number(v, span, &ctx(func))).collect();
            let nums = nums?;
            let acc = if func == "min" {
                nums.iter().copied().fold(f64::INFINITY, f64::min)
            } else {
                nums.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            };
            Ok(Value::Number(acc))
        }
        "random" => Ok(Value::Number(next_random())),
        "sin" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("sin"))?.sin())),
        "cos" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("cos"))?.cos())),
        "tan" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("tan"))?.tan())),
        "asin" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("asin"))?.asin())),
        "acos" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("acos"))?.acos())),
        "atan" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("atan"))?.atan())),
        "atan2" => {
            let y = want_number(&arg(args, 0), span, &ctx("atan2"))?;
            let x = want_number(&arg(args, 1), span, &ctx("atan2"))?;
            Ok(Value::Number(y.atan2(x)))
        }
        "exp" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("exp"))?.exp())),
        "ln" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("ln"))?.ln())),
        "log2" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("log2"))?.log2())),
        "log10" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("log10"))?.log10())),
        _ => Err(AsError::at(format!("std/math has no function '{}'", func), span).into()),
    }
}

// A tiny self-seeded xorshift64* PRNG (no external crate). Seeded once from the
// system clock + a stack address; advances thread-locally. Adequate for
// scripting `math.random()`; NOT cryptographic (see std/crypto, M13).
thread_local! {
    static RNG: Cell<u64> = Cell::new(seed());
}

fn seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    let local = 0u8;
    let addr = &local as *const u8 as u64;
    (nanos ^ addr).max(1)
}

fn next_random() -> f64 {
    RNG.with(|cell| {
        let mut x = cell.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        cell.set(x);
        let r = x.wrapping_mul(0x2545F4914F6CDD1D);
        (r >> 11) as f64 / (1u64 << 53) as f64
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(x: f64) -> Value {
        Value::Number(x)
    }

    fn sp() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn math_trig_exp() {
        assert_eq!(call("sin", &[n(0.0)], sp()).unwrap(), n(0.0));
        assert_eq!(call("cos", &[n(0.0)], sp()).unwrap(), n(1.0));
        assert_eq!(call("exp", &[n(0.0)], sp()).unwrap(), n(1.0));
        assert_eq!(call("ln", &[n(1.0)], sp()).unwrap(), n(0.0));
        assert_eq!(call("log2", &[n(8.0)], sp()).unwrap(), n(3.0));
        assert_eq!(call("log10", &[n(1000.0)], sp()).unwrap(), n(3.0));
        assert_eq!(call("atan2", &[n(0.0), n(1.0)], sp()).unwrap(), n(0.0));
    }

    #[test]
    fn basics() {
        let sp = Span::new(0, 0);
        assert_eq!(call("abs", &[Value::Number(-3.0)], sp).unwrap(), Value::Number(3.0));
        assert_eq!(call("floor", &[Value::Number(2.9)], sp).unwrap(), Value::Number(2.0));
        assert_eq!(call("pow", &[Value::Number(2.0), Value::Number(10.0)], sp).unwrap(), Value::Number(1024.0));
        assert_eq!(call("max", &[Value::Number(1.0), Value::Number(9.0), Value::Number(4.0)], sp).unwrap(), Value::Number(9.0));
        assert_eq!(call("min", &[Value::Number(1.0), Value::Number(9.0), Value::Number(4.0)], sp).unwrap(), Value::Number(1.0));
    }

    #[test]
    fn random_in_range() {
        for _ in 0..1000 {
            let r = next_random();
            assert!((0.0..1.0).contains(&r), "random out of range: {r}");
        }
    }

    #[test]
    fn type_misuse_panics() {
        let e = call("sqrt", &[Value::Str("x".into())], Span::new(0, 0));
        assert!(matches!(e, Err(Control::Panic(_))));
    }

    #[test]
    fn min_max_zero_args_panic() {
        let sp = Span::new(0, 0);
        assert!(matches!(call("min", &[], sp), Err(Control::Panic(_))));
        assert!(matches!(call("max", &[], sp), Err(Control::Panic(_))));
    }
}
