//! `std/math` — numeric functions and constants.

use super::{arg, bi, want_array, want_number};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::{Value, ValueKind};
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
        ("pi", Value::float(std::f64::consts::PI)),
        ("e", Value::float(std::f64::consts::E)),
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
        // NUM §4: integer-only division helpers (int → int).
        ("floordiv", bi("math.floordiv")),
        ("divmod", bi("math.divmod")),
        ("ceildiv", bi("math.ceildiv")),
        // NUM §4: bit helpers on i64 (int → int).
        ("popcount", bi("math.popcount")),
        ("leading_zeros", bi("math.leading_zeros")),
        ("trailing_zeros", bi("math.trailing_zeros")),
        ("rotl", bi("math.rotl")),
        ("rotr", bi("math.rotr")),
    ]
}

/// Require `x` to be an integer-valued finite f64; returns it as i64 or panics.
fn want_int(x: f64, span: Span, ctx: &str) -> Result<i64, Control> {
    if x.fract() != 0.0 || !x.is_finite() {
        return Err(AsError::at(format!("{} requires finite integer values", ctx), span).into());
    }
    Ok(x as i64)
}

/// NUM §4: convert a finite f64 to an `i64`, rejecting non-finite (`inf`/`nan`)
/// and out-of-i64-range values with a clean Tier-2 panic — never silently
/// saturating. Mirrors the strict-bound pattern of the `int()` builtin
/// (`src/interp.rs`): `i64::MAX as f64` rounds UP to exactly `2^63` (out of
/// range), so the upper bound must be STRICT (`< -(i64::MIN as f64)`, i.e.
/// `< 2^63`); the lower bound is exact (`i64::MIN as f64 == -2^63`).
fn f64_to_int(x: f64, span: Span, ctx: &str) -> Result<i64, Control> {
    if !x.is_finite() {
        return Err(AsError::at(
            format!("{} cannot convert non-finite float to int", ctx),
            span,
        )
        .into());
    }
    if x >= i64::MIN as f64 && x < -(i64::MIN as f64) {
        Ok(x as i64)
    } else {
        Err(AsError::at(
            format!("{} result is out of range for int (i64)", ctx),
            span,
        )
        .into())
    }
}

/// NUM §4: `floor`/`ceil`/`round`/`trunc` return an `int`. An `int` input is
/// already integral and is returned unchanged; a `float` has `round_op` applied
/// (`.floor()`/`.ceil()`/`.round()`/`.trunc()`) and the integral result is
/// converted to a checked `int` (non-finite or out-of-range → clean panic).
fn round_to_int(
    v: &Value,
    round_op: fn(f64) -> f64,
    span: Span,
    ctx: &str,
) -> Result<Value, Control> {
    match v.kind() {
        ValueKind::Int(i) => Ok(Value::int(i)),
        ValueKind::Float(f) => Ok(Value::int(f64_to_int(round_op(f), span, ctx)?)),
        _ => Err(AsError::at(
            format!("{} expects a number, got {}", ctx, crate::interp::type_name(v)),
            span,
        )
        .into()),
    }
}

/// NUM §4: require the argument to be a strict `Value::int` (the int→int
/// helpers — `floordiv`/`divmod`/`ceildiv` and the bit ops — do not accept a
/// `float`, since they are exact integer operations). A `float` (or any other
/// kind) is a Tier-2 panic.
fn want_int_value(v: &Value, span: Span, ctx: &str) -> Result<i64, Control> {
    match v.kind() {
        ValueKind::Int(i) => Ok(i),
        _ => Err(AsError::at(
            format!("{} expects an int, got {}", ctx, crate::interp::type_name(v)),
            span,
        )
        .into()),
    }
}

/// NUM §4: floored integer division `a // b` (distinct from the truncating
/// language `/`). `b == 0` → clean panic; `a == i64::MIN, b == -1` overflows
/// (the single i64 division-overflow case) → clean panic.
fn floordiv_i64(a: i64, b: i64, span: Span, ctx: &str) -> Result<i64, Control> {
    if b == 0 {
        return Err(AsError::at(format!("{}: division by zero", ctx), span).into());
    }
    // The only overflowing i64 division is `i64::MIN / -1`; reject it cleanly
    // (a plain `/` or `%` would panic the host). `checked_div`/`checked_rem`
    // return `None` precisely here.
    let q = match a.checked_div(b) {
        Some(q) => q,
        None => return Err(AsError::at(format!("{}: integer overflow", ctx), span).into()),
    };
    let r = a % b; // safe: overflow already excluded above
    // Truncated quotient `q` is one too high when the (true) remainder is
    // non-zero and the divisor/remainder signs differ — adjust down to floor.
    if (r != 0) && ((r < 0) != (b < 0)) {
        Ok(q - 1)
    } else {
        Ok(q)
    }
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
        // NUM §4: `abs` is the only SUBTYPE-PRESERVING math fn: `abs(int)->int`
        // (checked — `abs(i64::MIN)` overflows and is a clean panic, never
        // wraps), `abs(float)->float`.
        "abs" => {
            let v = arg(args, 0);
            match v.kind() {
                ValueKind::Int(i) => match i.checked_abs() {
                    Some(a) => Ok(Value::int(a)),
                    None => Err(AsError::at(
                        "math.abs: integer overflow (abs of i64::MIN)",
                        span,
                    )
                    .into()),
                },
                ValueKind::Float(f) => Ok(Value::float(f.abs())),
                _ => Err(AsError::at(
                    format!("math.abs expects a number, got {}", crate::interp::type_name(&v)),
                    span,
                )
                .into()),
            }
        }
        // NUM §4: rounding fns return an `int`. An `int` input is already
        // integral and passes through unchanged.
        "floor" => round_to_int(&arg(args, 0), f64::floor, span, &ctx("floor")),
        "ceil" => round_to_int(&arg(args, 0), f64::ceil, span, &ctx("ceil")),
        "round" => round_to_int(&arg(args, 0), f64::round, span, &ctx("round")),
        "sqrt" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("sqrt"))?.sqrt(),
        )),
        "pow" => {
            let b = want_number(&arg(args, 0), span, &ctx("pow"))?;
            let e = want_number(&arg(args, 1), span, &ctx("pow"))?;
            Ok(Value::float(b.powf(e)))
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
            Ok(Value::float(acc))
        }
        "random" => Ok(Value::float(next_random(interp))),
        "sin" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("sin"))?.sin(),
        )),
        "cos" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("cos"))?.cos(),
        )),
        "tan" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("tan"))?.tan(),
        )),
        "asin" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("asin"))?.asin(),
        )),
        "acos" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("acos"))?.acos(),
        )),
        "atan" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("atan"))?.atan(),
        )),
        "atan2" => {
            let y = want_number(&arg(args, 0), span, &ctx("atan2"))?;
            let x = want_number(&arg(args, 1), span, &ctx("atan2"))?;
            Ok(Value::float(y.atan2(x)))
        }
        "exp" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("exp"))?.exp(),
        )),
        "ln" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("ln"))?.ln(),
        )),
        "log2" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("log2"))?.log2(),
        )),
        "log10" => Ok(Value::float(
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
            Ok(Value::float(r))
        }
        "trunc" => round_to_int(&arg(args, 0), f64::trunc, span, &ctx("trunc")),
        "clamp" => {
            let x = want_number(&arg(args, 0), span, &ctx("clamp"))?;
            let lo = want_number(&arg(args, 1), span, &ctx("clamp"))?;
            let hi = want_number(&arg(args, 2), span, &ctx("clamp"))?;
            if lo > hi {
                return Err(AsError::at("math.clamp requires lo <= hi", span).into());
            }
            Ok(Value::float(x.max(lo).min(hi)))
        }
        "hypot" => {
            let x = want_number(&arg(args, 0), span, &ctx("hypot"))?;
            let y = want_number(&arg(args, 1), span, &ctx("hypot"))?;
            Ok(Value::float(x.hypot(y)))
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
            Ok(Value::float(gcd_i64(a, b) as f64))
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
            Ok(Value::float(r as f64))
        }
        "sum" => {
            let xs = want_number_vec(&arg(args, 0), span, &ctx("sum"))?;
            Ok(Value::float(xs.iter().sum()))
        }
        "mean" => {
            let xs = want_number_vec(&arg(args, 0), span, &ctx("mean"))?;
            if xs.is_empty() {
                return Err(AsError::at("math.mean of empty array", span).into());
            }
            Ok(Value::float(xs.iter().sum::<f64>() / xs.len() as f64))
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
            Ok(Value::float(med))
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
            Ok(Value::float(if func == "stddev" {
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
            Ok(Value::float(v as f64))
        }
        "shuffle" => {
            let a = want_array(&arg(args, 0), span, &ctx("shuffle"))?;
            let mut items = a.borrow().clone();
            let len = items.len();
            for i in (1..len).rev() {
                let j = (next_random(interp) * (i as f64 + 1.0)).floor() as usize;
                items.swap(i, j.min(i));
            }
            Ok(Value::array_cell(crate::value::ArrayCell::new(items)))
        }
        "choice" => {
            let a = want_array(&arg(args, 0), span, &ctx("choice"))?;
            let b = a.borrow();
            if b.is_empty() {
                return Ok(Value::nil());
            }
            let idx = (next_random(interp) * b.len() as f64).floor() as usize;
            Ok(b[idx.min(b.len() - 1)].clone())
        }
        // NUM §4: int → int division helpers. All require strict `int` args;
        // `b == 0` is a clean Tier-2 panic.
        "floordiv" => {
            let a = want_int_value(&arg(args, 0), span, "math.floordiv")?;
            let b = want_int_value(&arg(args, 1), span, "math.floordiv")?;
            Ok(Value::int(floordiv_i64(a, b, span, "math.floordiv")?))
        }
        "ceildiv" => {
            let a = want_int_value(&arg(args, 0), span, "math.ceildiv")?;
            let b = want_int_value(&arg(args, 1), span, "math.ceildiv")?;
            // ceil(a/b) == -floor(a / -b) == floor((a + b - sign) / b); compute
            // via floor of the negated numerator to reuse the checked floor.
            let neg_floor = floordiv_i64(
                a.checked_neg().ok_or_else(|| -> Control {
                    AsError::at("math.ceildiv: integer overflow", span).into()
                })?,
                b,
                span,
                "math.ceildiv",
            )?;
            let q = neg_floor.checked_neg().ok_or_else(|| -> Control {
                AsError::at("math.ceildiv: integer overflow", span).into()
            })?;
            Ok(Value::int(q))
        }
        // `divmod(a, b) -> [q, r]` with q FLOORED and r the matching remainder,
        // satisfying `a == q*b + r` (r has the sign of the divisor, in `[0, |b|)`
        // toward the divisor's sign — the floored-division remainder).
        "divmod" => {
            let a = want_int_value(&arg(args, 0), span, "math.divmod")?;
            let b = want_int_value(&arg(args, 1), span, "math.divmod")?;
            let q = floordiv_i64(a, b, span, "math.divmod")?;
            // r = a - q*b, computed with checked ops so the identity holds
            // exactly with no host overflow (the floored q keeps |q*b| <= |a|,
            // so these cannot overflow, but stay defensive).
            let qb = q.checked_mul(b).ok_or_else(|| -> Control {
                AsError::at("math.divmod: integer overflow", span).into()
            })?;
            let r = a.checked_sub(qb).ok_or_else(|| -> Control {
                AsError::at("math.divmod: integer overflow", span).into()
            })?;
            Ok(Value::array_cell(crate::value::ArrayCell::new(vec![
                Value::int(q),
                Value::int(r),
            ])))
        }
        // NUM §4: bit helpers on the 64-bit i64 representation (int → int).
        "popcount" => {
            let x = want_int_value(&arg(args, 0), span, "math.popcount")?;
            Ok(Value::int(x.count_ones() as i64))
        }
        "leading_zeros" => {
            let x = want_int_value(&arg(args, 0), span, "math.leading_zeros")?;
            Ok(Value::int(x.leading_zeros() as i64))
        }
        "trailing_zeros" => {
            let x = want_int_value(&arg(args, 0), span, "math.trailing_zeros")?;
            Ok(Value::int(x.trailing_zeros() as i64))
        }
        // Rotations are modulo the 64-bit width (Rust `rotate_left`/`rotate_right`
        // already reduce the shift mod 64); the count is taken from the low bits
        // of the second int arg.
        "rotl" => {
            let x = want_int_value(&arg(args, 0), span, "math.rotl")?;
            let n = want_int_value(&arg(args, 1), span, "math.rotl")?;
            Ok(Value::int(x.rotate_left(n.rem_euclid(64) as u32)))
        }
        "rotr" => {
            let x = want_int_value(&arg(args, 0), span, "math.rotr")?;
            let n = want_int_value(&arg(args, 1), span, "math.rotr")?;
            Ok(Value::int(x.rotate_right(n.rem_euclid(64) as u32)))
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
        Value::float(x)
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
        // NUM §4: abs is subtype-preserving — float in, float out.
        assert_eq!(
            call("abs", &[Value::float(-3.0)], sp).unwrap(),
            Value::float(3.0)
        );
        // NUM §4: floor returns an int.
        assert_eq!(
            call("floor", &[Value::float(2.9)], sp).unwrap(),
            Value::int(2)
        );
        assert_eq!(
            call("pow", &[Value::float(2.0), Value::float(10.0)], sp).unwrap(),
            Value::float(1024.0)
        );
        assert_eq!(
            call(
                "max",
                &[Value::float(1.0), Value::float(9.0), Value::float(4.0)],
                sp
            )
            .unwrap(),
            Value::float(9.0)
        );
        assert_eq!(
            call(
                "min",
                &[Value::float(1.0), Value::float(9.0), Value::float(4.0)],
                sp
            )
            .unwrap(),
            Value::float(1.0)
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
        let e = call("sqrt", &[Value::str("x")], Span::new(0, 0));
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
        let a = Value::array_cell(crate::value::ArrayCell::new(vec![
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
        let sv = call("variance", &[a.clone(), Value::bool_(true)], sp()).unwrap();
        assert!(matches!(sv.kind(), ValueKind::Float(x) if (x - 5.0/3.0).abs() < 1e-12));
        // stddev returns sqrt(population variance)
        assert!(
            matches!(call("stddev", std::slice::from_ref(&a), sp()).unwrap().kind(), ValueKind::Float(x) if (x - 1.25f64.sqrt()).abs() < 1e-12)
        );
        let empty = Value::array_cell(crate::value::ArrayCell::new(vec![]));
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
        let one = Value::array_cell(crate::value::ArrayCell::new(vec![n(5.0)]));
        assert!(matches!(
            call("variance", &[one, Value::bool_(true)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn math_random_helpers() {
        for _ in 0..100 {
            let r = call("randomInt", &[n(1.0), n(6.0)], sp()).unwrap();
            if let ValueKind::Float(x) = r.kind() {
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
        let a = Value::array_cell(crate::value::ArrayCell::new(vec![
            n(1.0),
            n(2.0),
            n(3.0),
        ]));
        let sh = call("shuffle", std::slice::from_ref(&a), sp()).unwrap();
        if let ValueKind::Array(v) = sh.kind() {
            assert_eq!(v.borrow().len(), 3);
        } else {
            panic!()
        }
        // shuffle is non-mutating: original unchanged length & content set
        if let ValueKind::Array(orig) = a.kind() {
            assert_eq!(orig.borrow().len(), 3);
        }
        // choice returns an element actually in the array
        let elem = call("choice", std::slice::from_ref(&a), sp()).unwrap();
        assert!([n(1.0), n(2.0), n(3.0)].contains(&elem));
        // shuffle preserves the multiset of elements (sorted equal to original)
        let sh2 = call("shuffle", std::slice::from_ref(&a), sp()).unwrap();
        if let ValueKind::Array(v) = sh2.kind() {
            let mut got: Vec<f64> = v
                .borrow()
                .iter()
                .map(|x| {
                    if let ValueKind::Float(n) = x.kind() {
                        n
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
        let empty = Value::array_cell(crate::value::ArrayCell::new(vec![]));
        assert_eq!(
            call("choice", std::slice::from_ref(&empty), sp()).unwrap(),
            Value::nil()
        );
    }

    #[test]
    fn math_scalar_helpers() {
        assert_eq!(call("sign", &[n(-3.0)], sp()).unwrap(), n(-1.0));
        assert_eq!(call("sign", &[n(0.0)], sp()).unwrap(), n(0.0));
        assert!(
            matches!(call("sign", &[n(f64::NAN)], sp()).unwrap().kind(), ValueKind::Float(x) if x.is_nan())
        );
        // NUM §4: trunc returns an int.
        assert_eq!(call("trunc", &[n(3.7)], sp()).unwrap(), Value::int(3));
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

    // ---- NUM §4: typed returns -------------------------------------------

    fn i(x: i64) -> Value {
        Value::int(x)
    }

    #[test]
    fn rounding_returns_int() {
        // float inputs round to a true int.
        assert_eq!(call("floor", &[n(3.7)], sp()).unwrap(), i(3));
        assert_eq!(call("floor", &[n(-3.1)], sp()).unwrap(), i(-4));
        assert_eq!(call("ceil", &[n(3.1)], sp()).unwrap(), i(4));
        assert_eq!(call("ceil", &[n(-3.1)], sp()).unwrap(), i(-3));
        assert_eq!(call("round", &[n(2.5)], sp()).unwrap(), i(3));
        assert_eq!(call("round", &[n(2.4)], sp()).unwrap(), i(2));
        assert_eq!(call("trunc", &[n(3.9)], sp()).unwrap(), i(3));
        assert_eq!(call("trunc", &[n(-3.9)], sp()).unwrap(), i(-3));
    }

    #[test]
    fn rounding_passes_int_through_unchanged() {
        for f in ["floor", "ceil", "round", "trunc"] {
            assert_eq!(call(f, &[i(7)], sp()).unwrap(), i(7));
            assert_eq!(call(f, &[i(-7)], sp()).unwrap(), i(-7));
        }
    }

    #[test]
    fn rounding_non_finite_and_out_of_range_panic() {
        // inf/nan → clean panic, not a silent saturation.
        assert!(matches!(
            call("floor", &[n(f64::INFINITY)], sp()),
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            call("trunc", &[n(f64::NAN)], sp()),
            Err(Control::Panic(_))
        ));
        // 1e30 exceeds i64 range → clean panic, never wraps/saturates.
        assert!(matches!(
            call("floor", &[n(1e30)], sp()),
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            call("ceil", &[n(-1e30)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn abs_is_subtype_preserving() {
        assert_eq!(call("abs", &[i(-5)], sp()).unwrap(), i(5));
        assert_eq!(call("abs", &[i(5)], sp()).unwrap(), i(5));
        assert_eq!(call("abs", &[n(-2.5)], sp()).unwrap(), n(2.5));
        assert_eq!(call("abs", &[n(2.5)], sp()).unwrap(), n(2.5));
        // abs(i64::MIN) overflows → clean panic, never wraps.
        assert!(matches!(
            call("abs", &[i(i64::MIN)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn floordiv_floors_toward_neg_inf() {
        assert_eq!(call("floordiv", &[i(7), i(2)], sp()).unwrap(), i(3));
        assert_eq!(call("floordiv", &[i(-7), i(2)], sp()).unwrap(), i(-4));
        assert_eq!(call("floordiv", &[i(7), i(-2)], sp()).unwrap(), i(-4));
        assert_eq!(call("floordiv", &[i(-7), i(-2)], sp()).unwrap(), i(3));
        assert_eq!(call("floordiv", &[i(6), i(3)], sp()).unwrap(), i(2));
        // b == 0 → panic.
        assert!(matches!(
            call("floordiv", &[i(1), i(0)], sp()),
            Err(Control::Panic(_))
        ));
        // i64::MIN / -1 overflow → clean panic.
        assert!(matches!(
            call("floordiv", &[i(i64::MIN), i(-1)], sp()),
            Err(Control::Panic(_))
        ));
        // float arg → panic (int-only).
        assert!(matches!(
            call("floordiv", &[n(7.0), i(2)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn ceildiv_rounds_toward_pos_inf() {
        assert_eq!(call("ceildiv", &[i(7), i(2)], sp()).unwrap(), i(4));
        assert_eq!(call("ceildiv", &[i(-7), i(2)], sp()).unwrap(), i(-3));
        assert_eq!(call("ceildiv", &[i(6), i(3)], sp()).unwrap(), i(2));
        assert_eq!(call("ceildiv", &[i(7), i(-2)], sp()).unwrap(), i(-3));
        assert!(matches!(
            call("ceildiv", &[i(1), i(0)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn divmod_satisfies_identity() {
        // divmod returns [q, r] with q floored and a == q*b + r.
        for (a, b) in [(17i64, 5i64), (-17, 5), (17, -5), (-17, -5), (10, 2), (0, 7)] {
            let r = call("divmod", &[i(a), i(b)], sp()).unwrap();
            if let ValueKind::Array(arr) = r.kind() {
                let arr = arr.borrow();
                assert_eq!(arr.len(), 2);
                let q = if let ValueKind::Int(q) = arr[0].kind() { q } else { panic!() };
                let rem = if let ValueKind::Int(r) = arr[1].kind() { r } else { panic!() };
                assert_eq!(a, q * b + rem, "identity for divmod({a},{b})");
                // q is floored: matches floordiv.
                assert_eq!(Value::int(q), call("floordiv", &[i(a), i(b)], sp()).unwrap());
            } else {
                panic!("divmod did not return an array");
            }
        }
        assert!(matches!(
            call("divmod", &[i(1), i(0)], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn bit_helpers() {
        assert_eq!(call("popcount", &[i(255)], sp()).unwrap(), i(8));
        assert_eq!(call("popcount", &[i(0)], sp()).unwrap(), i(0));
        assert_eq!(call("popcount", &[i(-1)], sp()).unwrap(), i(64));
        assert_eq!(call("leading_zeros", &[i(1)], sp()).unwrap(), i(63));
        assert_eq!(call("leading_zeros", &[i(0)], sp()).unwrap(), i(64));
        assert_eq!(call("trailing_zeros", &[i(8)], sp()).unwrap(), i(3));
        assert_eq!(call("trailing_zeros", &[i(0)], sp()).unwrap(), i(64));
        // rotl/rotr on the 64-bit width.
        assert_eq!(call("rotl", &[i(1), i(1)], sp()).unwrap(), i(2));
        assert_eq!(call("rotl", &[i(1), i(64)], sp()).unwrap(), i(1)); // mod 64
        assert_eq!(
            call("rotl", &[i(1), i(63)], sp()).unwrap(),
            i(i64::MIN) // top bit set
        );
        assert_eq!(call("rotr", &[i(2), i(1)], sp()).unwrap(), i(1));
        assert_eq!(call("rotr", &[i(1), i(1)], sp()).unwrap(), i(i64::MIN));
        // float arg → panic (int-only).
        assert!(matches!(
            call("popcount", &[n(255.0)], sp()),
            Err(Control::Panic(_))
        ));
    }
}
