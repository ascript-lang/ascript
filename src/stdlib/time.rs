//! `std/time` — wall-clock + monotonic time, async sleep, duration helpers.

use super::{arg, bi, want_number};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("now", bi("time.now")),
        ("monotonic", bi("time.monotonic")),
        ("sleep", bi("time.sleep")),
        ("millis", bi("time.millis")),
        ("seconds", bi("time.seconds")),
        ("minutes", bi("time.minutes")),
        ("hours", bi("time.hours")),
    ]
}

// A process-global start instant for monotonic(), lazily initialized. Global
// (not thread_local) so readings are comparable across threads under a
// multi-thread runtime.
use std::sync::LazyLock;
static START: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);

/// Synchronous time functions. `sleep` is handled async in `call_time` (mod.rs)
/// and must NOT be dispatched here.
pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("time.{}", f);
    match func {
        "now" => {
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as f64)
                .unwrap_or(0.0);
            Ok(Value::Number(ms))
        }
        "monotonic" => {
            let ms = START.elapsed().as_secs_f64() * 1000.0;
            Ok(Value::Number(ms))
        }
        "millis" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("millis"))?)),
        "seconds" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("seconds"))? * 1000.0)),
        "minutes" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("minutes"))? * 60_000.0)),
        "hours" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("hours"))? * 3_600_000.0)),
        "sleep" => unreachable!("time.sleep is dispatched async in call_time"),
        _ => Err(AsError::at(format!("std/time has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn now_is_after_a_known_past_epoch() {
        let v = call("now", &[], sp()).unwrap();
        match v {
            Value::Number(n) => assert!(n > 1.7e12, "now() = {n}, expected > 1.7e12"),
            _ => panic!("now() should return a number"),
        }
    }

    #[test]
    fn monotonic_is_non_negative_and_increases() {
        let a = match call("monotonic", &[], sp()).unwrap() {
            Value::Number(n) => n,
            _ => panic!("monotonic() should return a number"),
        };
        assert!(a >= 0.0, "monotonic() = {a}, expected >= 0");
        // busy loop to ensure measurable elapsed time
        let mut acc: u64 = 0;
        for i in 0..2_000_000u64 {
            acc = acc.wrapping_add(i);
        }
        std::hint::black_box(acc);
        let b = match call("monotonic", &[], sp()).unwrap() {
            Value::Number(n) => n,
            _ => panic!("monotonic() should return a number"),
        };
        assert!(b >= a, "monotonic must not go backwards: a={a}, b={b}");
    }

    #[test]
    fn duration_helpers() {
        assert_eq!(call("millis", &[Value::Number(250.0)], sp()).unwrap(), Value::Number(250.0));
        assert_eq!(call("seconds", &[Value::Number(2.0)], sp()).unwrap(), Value::Number(2000.0));
        assert_eq!(call("minutes", &[Value::Number(1.0)], sp()).unwrap(), Value::Number(60_000.0));
        assert_eq!(call("hours", &[Value::Number(1.0)], sp()).unwrap(), Value::Number(3_600_000.0));
    }

    #[test]
    fn unknown_function_errors() {
        assert!(call("nope", &[], sp()).is_err());
    }
}
