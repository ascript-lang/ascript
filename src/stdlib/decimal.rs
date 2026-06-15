//! `std/decimal` — exact decimal arithmetic (money / large integers).
//!
//! `Value::decimal_rc` wraps `rust_decimal::Decimal` (a 96-bit scaled integer).
//! Construction is always explicit — no grammar changes.
//!
//! **Rounding:** `round(d, places)` uses `MidpointRounding::MidpointAwayFromZero`
//! (also called "round half away from zero", the conventional school-math rule).
//! `1.5` → `2`, `2.5` → `3`, `−1.5` → `−2`.
//!
//! **Map key:** `MapKey::Decimal` is added so a Decimal can be a Map key.
//! Decimal and Number keys are **distinct** — number `1` and decimal `1` are
//! separate keys (see `value.rs` `mapkey_number_and_decimal_are_distinct`).
//!
//! **JSON serialization:** both the strict (`from_ascript`) and lossy
//! (`to_json_lossy`) serializers emit a valid JSON *number* for a Decimal. They
//! do so by reparsing the decimal's string through serde_json, which
//! re-canonicalizes the value — so trailing-zero **scale is NOT preserved**
//! (`decimal.from("1.50")` serializes to JSON `1.5`, not `1.50`). Use
//! `decimal.toString(d)` if you need to round-trip the exact scale as text.
//! A Decimal is always finite, so no null fallback is needed.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::{Value, ValueKind};
use rust_decimal::prelude::*;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("from", bi("decimal.from")),
        ("parse", bi("decimal.parse")),
        ("toString", bi("decimal.toString")),
        ("toNumber", bi("decimal.toNumber")),
        ("round", bi("decimal.round")),
        ("abs", bi("decimal.abs")),
        ("floor", bi("decimal.floor")),
        ("ceil", bi("decimal.ceil")),
        ("trunc", bi("decimal.trunc")),
    ]
}

// ---- internal helpers -------------------------------------------------------

/// Coerce a Value to Decimal for binary-op use.
/// - Decimal: unchanged.
/// - Number: `Decimal::from_f64`; non-finite → Tier-2 panic.
/// - Anything else: None (caller panics with a better message).
pub(crate) fn coerce_to_decimal(v: &Value, span: Span) -> Result<Option<Decimal>, Control> {
    match v.kind() {
        ValueKind::Decimal(d) => Ok(Some(**d)),
        // NUM §4: an `Int` converts EXACTLY (no f64 round-trip).
        ValueKind::Int(i) => Ok(Some(Decimal::from(i))),
        ValueKind::Float(n) => {
            if !n.is_finite() {
                return Err(
                    AsError::at("cannot convert non-finite number to decimal", span).into(),
                );
            }
            match Decimal::from_f64(n) {
                Some(d) => Ok(Some(d)),
                None => {
                    Err(AsError::at("cannot convert number to decimal (out of range)", span).into())
                }
            }
        }
        _ => Ok(None),
    }
}

// ---- module call ------------------------------------------------------------

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("decimal.{}", f);
    match func {
        // decimal.from(x) → decimal
        //   x = string  : parse exactly; invalid → Tier-2 panic
        //   x = number  : integer → exact; non-integer f64 → shortest-round-trip
        //   x = decimal : identity
        "from" => {
            let v = arg(args, 0);
            match v.kind() {
                ValueKind::Decimal(d) => Ok(Value::decimal_rc(d.clone())),
                // NUM §4: an `Int` converts EXACTLY.
                ValueKind::Int(i) => Ok(Value::decimal(Decimal::from(i))),
                ValueKind::Str(s) => Decimal::from_str(s.as_ref())
                    .map(Value::decimal)
                    .map_err(|_| {
                        AsError::at(
                            format!("decimal.from: invalid decimal string {:?}", s.as_ref()),
                            span,
                        )
                        .into()
                    }),
                ValueKind::Float(n) => {
                    if !n.is_finite() {
                        return Err(AsError::at(
                            "decimal.from: cannot convert non-finite number to decimal",
                            span,
                        )
                        .into());
                    }
                    // Integer-valued floats → exact integer Decimal (no fractional part).
                    // STRICT upper bound: `i64::MAX as f64` rounds UP to 2^63 (out of
                    // i64 range), so `<=` would admit 2^63 and `as i64` would saturate;
                    // `-(i64::MIN as f64)` == 2^63 and `<` excludes it. Out-of-i64-range
                    // integral floats fall through to the exact `from_f64` path below.
                    if n.fract() == 0.0 && n >= i64::MIN as f64 && n < -(i64::MIN as f64) {
                        Ok(Value::decimal(Decimal::from(n as i64)))
                    } else {
                        // Non-integer: use shortest round-trip via from_f64.
                        Decimal::from_f64(n)
                            .map(Value::decimal)
                            .ok_or_else(|| {
                                AsError::at(
                                    "decimal.from: cannot convert number to decimal (out of range)",
                                    span,
                                )
                                .into()
                            })
                    }
                }
                _ => Err(AsError::at(
                    format!(
                        "decimal.from expects a string, number, or decimal, got {}",
                        crate::interp::type_name(&v)
                    ),
                    span,
                )
                .into()),
            }
        }

        // decimal.parse(s) → [decimal, err]  (Tier-1 safe parse)
        "parse" => {
            let s = want_string(&arg(args, 0), span, &ctx("parse"))?;
            match Decimal::from_str(s.as_ref()) {
                Ok(d) => Ok(make_pair(Value::decimal(d), Value::nil())),
                Err(e) => Ok(make_pair(
                    Value::nil(),
                    make_error(Value::str(format!("invalid decimal: {}", e))),
                )),
            }
        }

        // decimal.toString(d) → string
        "toString" => {
            let d = want_decimal(&arg(args, 0), span, &ctx("toString"))?;
            Ok(Value::str(d.to_string()))
        }

        // decimal.toNumber(d) → number  (lossy f64 conversion)
        "toNumber" => {
            let d = want_decimal(&arg(args, 0), span, &ctx("toNumber"))?;
            Ok(Value::float(d.to_f64().unwrap_or(f64::NAN)))
        }

        // decimal.round(d, places=0) → decimal  (half-away-from-zero)
        "round" => {
            let d = want_decimal(&arg(args, 0), span, &ctx("round"))?;
            let places = match args.get(1) {
                Some(v) if matches!(v.kind(), ValueKind::Nil) => 0,
                None => 0,
                // NUM §4: accept BOTH numeric subtypes for `places`.
                Some(v) if v.is_number() => {
                    let n = v.as_f64().unwrap_or(f64::NAN);
                    if n.fract() != 0.0 || !n.is_finite() || !(0.0..=28.0).contains(&n) {
                        return Err(AsError::at(
                            "decimal.round: places must be a non-negative integer (0–28)",
                            span,
                        )
                        .into());
                    }
                    n as u32
                }
                _ => return Err(AsError::at("decimal.round: places must be a number", span).into()),
            };
            Ok(Value::decimal(d.round_dp_with_strategy(
                places,
                rust_decimal::RoundingStrategy::MidpointAwayFromZero,
            )))
        }

        // decimal.abs(d) → decimal
        "abs" => {
            let d = want_decimal(&arg(args, 0), span, &ctx("abs"))?;
            Ok(Value::decimal(d.abs()))
        }

        // decimal.floor(d) → decimal
        "floor" => {
            let d = want_decimal(&arg(args, 0), span, &ctx("floor"))?;
            Ok(Value::decimal(d.floor()))
        }

        // decimal.ceil(d) → decimal
        "ceil" => {
            let d = want_decimal(&arg(args, 0), span, &ctx("ceil"))?;
            Ok(Value::decimal(d.ceil()))
        }

        // decimal.trunc(d) → decimal
        "trunc" => {
            let d = want_decimal(&arg(args, 0), span, &ctx("trunc"))?;
            Ok(Value::decimal(d.trunc()))
        }

        _ => Err(AsError::at(format!("std/decimal has no function '{}'", func), span).into()),
    }
}

// ---- argument helper --------------------------------------------------------

pub(crate) fn want_decimal(v: &Value, span: Span, ctx: &str) -> Result<Decimal, Control> {
    match v.kind() {
        ValueKind::Decimal(d) => Ok(**d),
        _ => Err(AsError::at(
            format!(
                "{} expects a decimal, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn d(s: &str) -> Value {
        Value::decimal(Decimal::from_str(s).unwrap())
    }

    // --- construction ---

    #[test]
    fn from_string_preserves_scale() {
        let v = call("from", &[Value::str("1.50")], sp()).unwrap();
        assert_eq!(v.to_string(), "1.50");
    }

    #[test]
    fn from_string_integer() {
        let v = call("from", &[Value::str("42")], sp()).unwrap();
        assert_eq!(v.to_string(), "42");
    }

    #[test]
    fn from_integer_number() {
        let v = call("from", &[Value::float(3.0)], sp()).unwrap();
        assert_eq!(v.to_string(), "3");
    }

    #[test]
    fn from_float_number_round_trips() {
        // decimal.from(1.1) must equal decimal.parse("1.1")
        let via_number = call("from", &[Value::float(1.1)], sp()).unwrap();
        let via_string = call("from", &[Value::str("1.1")], sp()).unwrap();
        assert_eq!(
            via_number, via_string,
            "from(1.1) should equal from(\"1.1\"): got {} vs {}",
            via_number, via_string
        );
    }

    #[test]
    fn from_invalid_string_panics() {
        let result = call("from", &[Value::str("xyz")], sp());
        assert!(
            matches!(result, Err(crate::interp::Control::Panic(_))),
            "expected Tier-2 panic for invalid string"
        );
    }

    #[test]
    fn from_non_finite_panics() {
        let result = call("from", &[Value::float(f64::INFINITY)], sp());
        assert!(matches!(result, Err(crate::interp::Control::Panic(_))));
        let result2 = call("from", &[Value::float(f64::NAN)], sp());
        assert!(matches!(result2, Err(crate::interp::Control::Panic(_))));
    }

    #[test]
    fn from_wrong_type_panics() {
        let result = call("from", &[Value::bool_(true)], sp());
        assert!(matches!(result, Err(crate::interp::Control::Panic(_))));
    }

    // --- parse ---

    #[test]
    fn parse_valid_returns_decimal() {
        let pair = call("parse", &[Value::str("1.5")], sp()).unwrap();
        // pair is [decimal, nil]
        let s = pair.to_string();
        assert!(s.starts_with("[1.5, nil]"), "got: {s}");
    }

    #[test]
    fn parse_invalid_returns_err() {
        let pair = call("parse", &[Value::str("x")], sp()).unwrap();
        let s = pair.to_string();
        assert!(s.starts_with("[nil, {message:"), "got: {s}");
    }

    // --- toString / toNumber ---

    #[test]
    fn to_string_preserves_scale() {
        let dec = d("1.50");
        let s = call("toString", &[dec], sp()).unwrap();
        assert_eq!(s, Value::str("1.50"));
    }

    #[test]
    fn to_number_is_lossy() {
        let dec = d("1.5");
        let n = call("toNumber", &[dec], sp()).unwrap();
        assert_eq!(n, Value::float(1.5));
    }

    // --- round ---

    #[test]
    fn round_default_zero_places() {
        assert_eq!(call("round", &[d("1.5")], sp()).unwrap(), d("2"));
        assert_eq!(call("round", &[d("2.5")], sp()).unwrap(), d("3"));
        // half-away-from-zero: −1.5 → −2
        assert_eq!(call("round", &[d("-1.5")], sp()).unwrap(), d("-2"));
    }

    #[test]
    fn round_with_places() {
        // round(1.456, 2) → 1.46
        assert_eq!(
            call("round", &[d("1.456"), Value::float(2.0)], sp()).unwrap(),
            d("1.46")
        );
    }

    // --- abs / floor / ceil / trunc ---

    #[test]
    fn abs_negative() {
        assert_eq!(call("abs", &[d("-3.7")], sp()).unwrap(), d("3.7"));
        assert_eq!(call("abs", &[d("3.7")], sp()).unwrap(), d("3.7"));
    }

    #[test]
    fn floor_positive_and_negative() {
        assert_eq!(call("floor", &[d("1.9")], sp()).unwrap(), d("1"));
        assert_eq!(call("floor", &[d("-1.1")], sp()).unwrap(), d("-2"));
    }

    #[test]
    fn ceil_positive_and_negative() {
        assert_eq!(call("ceil", &[d("1.1")], sp()).unwrap(), d("2"));
        assert_eq!(call("ceil", &[d("-1.9")], sp()).unwrap(), d("-1"));
    }

    #[test]
    fn trunc_drops_fractional() {
        assert_eq!(call("trunc", &[d("1.9")], sp()).unwrap(), d("1"));
        assert_eq!(call("trunc", &[d("-1.9")], sp()).unwrap(), d("-1"));
    }

    // --- operator overloading (tested via interp, see interp.rs tests) ---
}
