//! `std/math` — numeric functions and constants.

use super::{arg, bi, want_array, want_number};
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
        ("sign", bi("math.sign")),
        ("trunc", bi("math.trunc")),
        ("clamp", bi("math.clamp")),
        ("hypot", bi("math.hypot")),
        ("gcd", bi("math.gcd")),
        ("lcm", bi("math.lcm")),
        ("sum", bi("math.sum")),
        ("mean", bi("math.mean")),
        ("median", bi("math.median")),
        ("variance", bi("math.variance")),
        ("stddev", bi("math.stddev")),
        ("randomInt", bi("math.randomInt")),
        ("shuffle", bi("math.shuffle")),
        ("choice", bi("math.choice")),
    ]
}

/// Require `x` to be an integer-valued finite f64; returns it as i64 or panics.
fn want_int(x: f64, span: Span, ctx: &str) -> Result<i64, Control> {
    if x.fract() != 0.0 || !x.is_finite() {
        return Err(AsError::at(format!("{} requires finite integer values", ctx), span).into());
    }
    Ok(x as i64)
}

fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Collect an array argument into a Vec<f64>, panicking on non-number elements.
fn want_number_vec(v: &Value, span: Span, ctx: &str) -> Result<Vec<f64>, Control> {
    let a = want_array(v, span, ctx)?;
    let mut out = Vec::with_capacity(a.borrow().len());
    for el in a.borrow().iter() {
        out.push(want_number(el, span, ctx)?);
    }
    Ok(out)
}

pub fn call(
    interp: &crate::interp::Interp,
    func: &str,
    args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    let ctx = |f: &str| format!("math.{}", f);
    match func {
        "abs" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("abs"))?.abs(),
        )),
        "floor" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("floor"))?.floor(),
        )),
        "ceil" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("ceil"))?.ceil(),
        )),
        "round" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("round"))?.round(),
        )),
        "sqrt" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("sqrt"))?.sqrt(),
        )),
        "pow" => {
            let b = want_number(&arg(args, 0), span, &ctx("pow"))?;
            let e = want_number(&arg(args, 1), span, &ctx("pow"))?;
            Ok(Value::Number(b.powf(e)))
        }
        "min" | "max" => {
            if args.is_empty() {
                return Err(AsError::at(
                    format!("math.{} requires at least one argument", func),
                    span,
                )
                .into());
            }
            let nums: Result<Vec<f64>, Control> = args
                .iter()
                .map(|v| want_number(v, span, &ctx(func)))
                .collect();
            let nums = nums?;
            let acc = if func == "min" {
                nums.iter().copied().fold(f64::INFINITY, f64::min)
            } else {
                nums.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            };
            Ok(Value::Number(acc))
        }
        "random" => Ok(Value::Number(next_random(interp))),
        "sin" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("sin"))?.sin(),
        )),
        "cos" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("cos"))?.cos(),
        )),
        "tan" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("tan"))?.tan(),
        )),
        "asin" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("asin"))?.asin(),
        )),
        "acos" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("acos"))?.acos(),
        )),
        "atan" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("atan"))?.atan(),
        )),
        "atan2" => {
            let y = want_number(&arg(args, 0), span, &ctx("atan2"))?;
            let x = want_number(&arg(args, 1), span, &ctx("atan2"))?;
            Ok(Value::Number(y.atan2(x)))
        }
        "exp" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("exp"))?.exp(),
        )),
        "ln" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("ln"))?.ln(),
        )),
        "log2" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("log2"))?.log2(),
        )),
        "log10" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("log10"))?.log10(),
        )),
        "sign" => {
            let x = want_number(&arg(args, 0), span, &ctx("sign"))?;
            let r = if x.is_nan() {
                f64::NAN
            } else if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            };
            Ok(Value::Number(r))
        }
        "trunc" => Ok(Value::Number(
            want_number(&arg(args, 0), span, &ctx("trunc"))?.trunc(),
        )),
        "clamp" => {
            let x = want_number(&arg(args, 0), span, &ctx("clamp"))?;
            let lo = want_number(&arg(args, 1), span, &ctx("clamp"))?;
            let hi = want_number(&arg(args, 2), span, &ctx("clamp"))?;
            if lo > hi {
                return Err(AsError::at("math.clamp requires lo <= hi", span).into());
            }
            Ok(Value::Number(x.max(lo).min(hi)))
        }
        "hypot" => {
            let x = want_number(&arg(args, 0), span, &ctx("hypot"))?;
            let y = want_number(&arg(args, 1), span, &ctx("hypot"))?;
            Ok(Value::Number(x.hypot(y)))
        }
        "gcd" => {
            let a = want_int(
                want_number(&arg(args, 0), span, &ctx("gcd"))?,
                span,
                "math.gcd",
            )?;
            let b = want_int(
                want_number(&arg(args, 1), span, &ctx("gcd"))?,
                span,
                "math.gcd",
            )?;
            Ok(Value::Number(gcd_i64(a, b) as f64))
        }
        "lcm" => {
            let a = want_int(
                want_number(&arg(args, 0), span, &ctx("lcm"))?,
                span,
                "math.lcm",
            )?;
            let b = want_int(
                want_number(&arg(args, 1), span, &ctx("lcm"))?,
                span,
                "math.lcm",
            )?;
            let g = gcd_i64(a, b);
            let r = if g == 0 {
                0i64
            } else {
                let product = (a as i128 / g as i128) * b as i128;
                let abs = product.abs();
                if abs > i64::MAX as i128 {
                    return Err(AsError::at("math.lcm result overflows integer range", span).into());
                }
                abs as i64
            };
            Ok(Value::Number(r as f64))
        }
        "sum" => {
            let xs = want_number_vec(&arg(args, 0), span, &ctx("sum"))?;
            Ok(Value::Number(xs.iter().sum()))
        }
        "mean" => {
            let xs = want_number_vec(&arg(args, 0), span, &ctx("mean"))?;
            if xs.is_empty() {
                return Err(AsError::at("math.mean of empty array", span).into());
            }
            Ok(Value::Number(xs.iter().sum::<f64>() / xs.len() as f64))
        }
        "median" => {
            let mut xs = want_number_vec(&arg(args, 0), span, &ctx("median"))?;
            if xs.is_empty() {
                return Err(AsError::at("math.median of empty array", span).into());
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let m = xs.len() / 2;
            let med = if xs.len() % 2 == 1 {
                xs[m]
            } else {
                (xs[m - 1] + xs[m]) / 2.0
            };
            Ok(Value::Number(med))
        }
        "variance" | "stddev" => {
            let xs = want_number_vec(&arg(args, 0), span, &ctx(func))?;
            let sample = matches!(args.get(1), Some(v) if v.is_truthy());
            if xs.is_empty() {
                return Err(AsError::at(format!("math.{} of empty array", func), span).into());
            }
            if sample && xs.len() < 2 {
                return Err(AsError::at(
                    format!("math.{} (sample) requires at least 2 values", func),
                    span,
                )
                .into());
            }
            let mean = xs.iter().sum::<f64>() / xs.len() as f64;
            let ss: f64 = xs.iter().map(|x| (x - mean).powi(2)).sum();
            let denom = if sample {
                xs.len() as f64 - 1.0
            } else {
                xs.len() as f64
            };
            let var = ss / denom;
            Ok(Value::Number(if func == "stddev" {
                var.sqrt()
            } else {
                var
            }))
        }
        "randomInt" => {
            let min = want_int(
                want_number(&arg(args, 0), span, &ctx("randomInt"))?,
                span,
                "math.randomInt",
            )?;
            let max = want_int(
                want_number(&arg(args, 1), span, &ctx("randomInt"))?,
                span,
                "math.randomInt",
            )?;
            if min > max {
                return Err(AsError::at("math.randomInt requires min <= max", span).into());
            }
            let span_len = (max - min + 1) as f64;
            let v = min + (next_random(interp) * span_len).floor() as i64;
            Ok(Value::Number(v as f64))
        }
        "shuffle" => {
            let a = want_array(&arg(args, 0), span, &ctx("shuffle"))?;
            let mut items = a.borrow().clone();
            let len = items.len();
            for i in (1..len).rev() {
                let j = (next_random(interp) * (i as f64 + 1.0)).floor() as usize;
                items.swap(i, j.min(i));
            }
            Ok(Value::Array(crate::value::ArrayCell::new(items)))
        }
        "choice" => {
            let a = want_array(&arg(args, 0), span, &ctx("choice"))?;
            let b = a.borrow();
            if b.is_empty() {
                return Ok(Value::Nil);
            }
            let idx = (next_random(interp) * b.len() as f64).floor() as usize;
            Ok(b[idx.min(b.len() - 1)].clone())
        }
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

/// The next `[0,1)` random value. SP9 §3: in deterministic mode (an `interp` with a
/// determinism context) it draws from the per-`Interp` seeded PRNG (recorded /
/// replayed); otherwise it uses the thread-local xorshift — BYTE-IDENTICAL to the
/// pre-SP9 path (the conversion math is identical; only the seed source differs).
fn next_random(interp: &crate::interp::Interp) -> f64 {
    if let Some(v) = interp.next_seeded_f64() {
        return v;
    }
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

    /// Test helper: dispatch `math::call` with a fresh (non-deterministic) `Interp`,
    /// so the `random*` seam takes its thread-local default path. Mirrors the old
    /// `call(func, args, span)` signature the tests used before the SP9 `&Interp`
    /// parameter was added.
    fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        let interp = crate::interp::Interp::new();
        super::call(&interp, func, args, span)
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
        assert_eq!(
            call("abs", &[Value::Number(-3.0)], sp).unwrap(),
            Value::Number(3.0)
        );
        assert_eq!(
            call("floor", &[Value::Number(2.9)], sp).unwrap(),
            Value::Number(2.0)
        );
        assert_eq!(
            call("pow", &[Value::Number(2.0), Value::Number(10.0)], sp).unwrap(),
            Value::Number(1024.0)
        );
        assert_eq!(
            call(
                "max",
                &[Value::Number(1.0), Value::Number(9.0), Value::Number(4.0)],
                sp
            )
            .unwrap(),
            Value::Number(9.0)
        );
        assert_eq!(
            call(
                "min",
                &[Value::Number(1.0), Value::Number(9.0), Value::Number(4.0)],
                sp
            )
            .unwrap(),
            Value::Number(1.0)
        );
    }

    #[test]
    fn random_in_range() {
        let interp = crate::interp::Interp::new();
        for _ in 0..1000 {
            let r = next_random(&interp);
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

    #[test]
    fn math_stats() {
        let a = Value::Array(crate::value::ArrayCell::new(vec![
            n(1.0),
            n(2.0),
            n(3.0),
            n(4.0),
        ]));
        assert_eq!(
            call("sum", std::slice::from_ref(&a), sp()).unwrap(),
            n(10.0)
        );
        assert_eq!(
            call("mean", std::slice::from_ref(&a), sp()).unwrap(),
            n(2.5)
        );
        assert_eq!(
            call("median", std::slice::from_ref(&a), sp()).unwrap(),
            n(2.5)
        );
        assert_eq!(
            call("variance", std::slice::from_ref(&a), sp()).unwrap(),
            n(1.25)
        );
        let sv = call("variance", &[a.clone(), Value::Bool(true)], sp()).unwrap();
        assert!(matches!(sv, Value::Number(x) if (x - 5.0/3.0).abs() < 1e-12));
        // stddev returns sqrt(population variance)
        assert!(
            matches!(call("stddev", std::slice::from_ref(&a), sp()).unwrap(), Value::Number(x) if (x - 1.25f64.sqrt()).abs() < 1e-12)
        );
        let empty = Value::Array(crate::value::ArrayCell::new(vec![]));
        assert_eq!(
            call("sum", std::slice::from_ref(&empty), sp()).unwrap(),
            n(0.0)
        );
        assert!(matches!(
            call("mean", std::slice::from_ref(&empty), sp()),
            Err(Control::Panic(_))
        ));
        // median of empty array panics
        assert!(matches!(
            call("median", std::slice::from_ref(&empty), sp()),
            Err(Control::Panic(_))
        ));
        // sample variance needs >= 2 elements
        let one = Value::Array(crate::value::ArrayCell::new(vec![n(5.0)]));
        assert!(matches!(
            call("variance", &[one, Value::Bool(true)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn math_random_helpers() {
        for _ in 0..100 {
            let r = call("randomInt", &[n(1.0), n(6.0)], sp()).unwrap();
            if let Value::Number(x) = r {
                assert!((1.0..=6.0).contains(&x) && x.fract() == 0.0);
            } else {
                panic!()
            }
        }
        assert_eq!(call("randomInt", &[n(5.0), n(5.0)], sp()).unwrap(), n(5.0));
        assert!(matches!(
            call("randomInt", &[n(6.0), n(1.0)], sp()),
            Err(Control::Panic(_))
        ));
        let a = Value::Array(crate::value::ArrayCell::new(vec![
            n(1.0),
            n(2.0),
            n(3.0),
        ]));
        let sh = call("shuffle", std::slice::from_ref(&a), sp()).unwrap();
        if let Value::Array(v) = sh {
            assert_eq!(v.borrow().len(), 3);
        } else {
            panic!()
        }
        // shuffle is non-mutating: original unchanged length & content set
        if let Value::Array(orig) = &a {
            assert_eq!(orig.borrow().len(), 3);
        }
        // choice returns an element actually in the array
        let elem = call("choice", std::slice::from_ref(&a), sp()).unwrap();
        assert!([n(1.0), n(2.0), n(3.0)].contains(&elem));
        // shuffle preserves the multiset of elements (sorted equal to original)
        let sh2 = call("shuffle", std::slice::from_ref(&a), sp()).unwrap();
        if let Value::Array(v) = sh2 {
            let mut got: Vec<f64> = v
                .borrow()
                .iter()
                .map(|x| {
                    if let Value::Number(n) = x {
                        *n
                    } else {
                        f64::NAN
                    }
                })
                .collect();
            got.sort_by(|x, y| x.partial_cmp(y).unwrap());
            assert_eq!(got, vec![1.0, 2.0, 3.0]);
        } else {
            panic!()
        }
        let empty = Value::Array(crate::value::ArrayCell::new(vec![]));
        assert_eq!(
            call("choice", std::slice::from_ref(&empty), sp()).unwrap(),
            Value::Nil
        );
    }

    #[test]
    fn math_scalar_helpers() {
        assert_eq!(call("sign", &[n(-3.0)], sp()).unwrap(), n(-1.0));
        assert_eq!(call("sign", &[n(0.0)], sp()).unwrap(), n(0.0));
        assert!(
            matches!(call("sign", &[n(f64::NAN)], sp()).unwrap(), Value::Number(x) if x.is_nan())
        );
        assert_eq!(call("trunc", &[n(3.7)], sp()).unwrap(), n(3.0));
        assert_eq!(
            call("clamp", &[n(5.0), n(0.0), n(3.0)], sp()).unwrap(),
            n(3.0)
        );
        assert_eq!(call("hypot", &[n(3.0), n(4.0)], sp()).unwrap(), n(5.0));
        assert_eq!(call("gcd", &[n(12.0), n(8.0)], sp()).unwrap(), n(4.0));
        assert_eq!(call("lcm", &[n(4.0), n(6.0)], sp()).unwrap(), n(12.0));
        assert!(matches!(
            call("clamp", &[n(1.0), n(3.0), n(0.0)], sp()),
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            call("gcd", &[n(1.5), n(2.0)], sp()),
            Err(Control::Panic(_))
        ));
    }
}
