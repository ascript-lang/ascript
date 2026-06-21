//! `std/time` — wall-clock + monotonic time, async sleep, duration helpers.

use super::{arg, bi, want_number};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
#[cfg(test)]
use crate::value::ValueKind;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("now", bi("time.now")),
        ("monotonic", bi("time.monotonic")),
        ("sleep", bi("time.sleep")),
        ("millis", bi("time.millis")),
        ("seconds", bi("time.seconds")),
        ("minutes", bi("time.minutes")),
        ("hours", bi("time.hours")),
        ("interval", bi("time.interval")),
        ("debounce", bi("time.debounce")),
        ("throttle", bi("time.throttle")),
    ]
}

/// The real monotonic clock in ms since process start. Shared with the SP9 §3
/// determinism seam in `call_time`, which passes this as the `None`-mode fallback
/// for `time.monotonic` so the default path stays byte-identical to the arm below.
/// WASM §5.3.3: the raw monotonic source (the process-global `Instant` baseline)
/// moved to `platform::monotonic_ms` (native arm unchanged; wasm uses
/// `performance.now`).
pub(crate) fn real_monotonic_ms() -> f64 {
    crate::platform::monotonic_ms()
}

/// Synchronous time functions. `sleep` is handled async in `call_time` (mod.rs)
/// and must NOT be dispatched here.
pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("time.{}", f);
    match func {
        "now" => {
            // WASM §5.3.3: route through the platform clock (native = same `SystemTime`
            // body; wasm = `Date.now`). NOTE: this synchronous arm is the NON-det path
            // reached only when `call_time` did NOT pre-empt with the det seam.
            let ms = crate::platform::now_unix_ms();
            Ok(Value::float(ms))
        }
        "monotonic" => {
            let ms = real_monotonic_ms();
            Ok(Value::float(ms))
        }
        "millis" => Ok(Value::float(want_number(
            &arg(args, 0),
            span,
            &ctx("millis"),
        )?)),
        "seconds" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("seconds"))? * 1000.0,
        )),
        "minutes" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("minutes"))? * 60_000.0,
        )),
        "hours" => Ok(Value::float(
            want_number(&arg(args, 0), span, &ctx("hours"))? * 3_600_000.0,
        )),
        "sleep" => unreachable!("time.sleep is dispatched async in call_time"),
        "interval" | "debounce" | "throttle" => {
            unreachable!("time.{} is dispatched in call_time", func)
        }
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
        match v.kind() {
            ValueKind::Float(n) => assert!(n > 1.7e12, "now() = {n}, expected > 1.7e12"),
            _ => panic!("now() should return a number"),
        }
    }

    #[test]
    fn monotonic_is_non_negative_and_increases() {
        let a = match call("monotonic", &[], sp()).unwrap().kind() {
            ValueKind::Float(n) => n,
            _ => panic!("monotonic() should return a number"),
        };
        assert!(a >= 0.0, "monotonic() = {a}, expected >= 0");
        // busy loop to ensure measurable elapsed time
        let mut acc: u64 = 0;
        for i in 0..2_000_000u64 {
            acc = acc.wrapping_add(i);
        }
        std::hint::black_box(acc);
        let b = match call("monotonic", &[], sp()).unwrap().kind() {
            ValueKind::Float(n) => n,
            _ => panic!("monotonic() should return a number"),
        };
        assert!(b >= a, "monotonic must not go backwards: a={a}, b={b}");
    }

    #[test]
    fn duration_helpers() {
        assert_eq!(
            call("millis", &[Value::float(250.0)], sp()).unwrap(),
            Value::float(250.0)
        );
        assert_eq!(
            call("seconds", &[Value::float(2.0)], sp()).unwrap(),
            Value::float(2000.0)
        );
        assert_eq!(
            call("minutes", &[Value::float(1.0)], sp()).unwrap(),
            Value::float(60_000.0)
        );
        assert_eq!(
            call("hours", &[Value::float(1.0)], sp()).unwrap(),
            Value::float(3_600_000.0)
        );
    }

    #[test]
    fn unknown_function_errors() {
        assert!(call("nope", &[], sp()).is_err());
    }
}
