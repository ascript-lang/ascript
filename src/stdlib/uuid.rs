//! `std/uuid` — UUID generation (v4 random, v7 time-ordered).

use super::bi;
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("v4", bi("uuid.v4")), ("v7", bi("uuid.v7"))]
}

pub fn call(
    interp: &crate::interp::Interp,
    func: &str,
    _args: &[Value],
    span: Span,
) -> Result<Value, Control> {
    match func {
        "v4" => Ok(Value::Str(v4(interp).to_string().into())),
        "v7" => Ok(Value::Str(v7(interp).to_string().into())),
        _ => Err(AsError::at(format!("std/uuid has no function '{}'", func), span).into()),
    }
}

/// A v4 UUID. SP9 §3: in deterministic mode the 16 random bytes come from the
/// per-`Interp` seeded PRNG (so `uuid.v4` is reproducible under `workflow`/replay),
/// with the version/variant nibbles set per RFC 4122; otherwise the real
/// `Uuid::new_v4()` — BYTE-IDENTICAL to pre-SP9.
fn v4(interp: &crate::interp::Interp) -> uuid::Uuid {
    let mut bytes = [0u8; 16];
    if interp.fill_seeded_bytes(&mut bytes) {
        // Set the version (4) and RFC 4122 variant bits, matching `Uuid::new_v4`.
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes)
    } else {
        uuid::Uuid::new_v4()
    }
}

/// A v7 (time-ordered) UUID. SP9 §3: in deterministic mode BOTH halves are
/// reproducible — the 48-bit unix-ms time prefix comes from the determinism
/// context's virtual clock (so the recorded/replayed timestamp matches) and the
/// 10 `rand_a`/`rand_b` tail bytes come from the per-`Interp` seeded PRNG; the
/// `Builder::from_unix_timestamp_millis` constructor sets the version (7) and
/// RFC 4122 variant bits. Otherwise the real `Uuid::now_v7()` (real clock + real
/// entropy) — BYTE-IDENTICAL to pre-SP9.
fn v7(interp: &crate::interp::Interp) -> uuid::Uuid {
    let mut tail = [0u8; 10];
    if interp.fill_seeded_bytes(&mut tail) {
        // Deterministic: draw the time prefix from the virtual clock (saturating
        // the ms-epoch into the v7 48-bit field; negative/huge clocks clamp to 0).
        let millis = interp.clock_now_ms();
        let millis = if millis.is_finite() && millis >= 0.0 {
            millis as u64
        } else {
            0
        };
        uuid::Builder::from_unix_timestamp_millis(millis, &tail).into_uuid()
    } else {
        uuid::Uuid::now_v7()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span {
        Span::new(0, 0)
    }

    /// Dispatch with a fresh non-deterministic `Interp` (the default real-RNG path).
    fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        let interp = crate::interp::Interp::new();
        super::call(&interp, func, args, span)
    }

    #[test]
    fn v4_v7_format() {
        let v4 = call("v4", &[], sp()).unwrap();
        if let Value::Str(s) = v4 {
            assert_eq!(s.len(), 36);
            assert_eq!(s.chars().filter(|&c| c == '-').count(), 4);
            assert_eq!(&s[14..15], "4"); // version nibble
        } else {
            panic!("expected string");
        }
        let a = call("v4", &[], sp()).unwrap();
        let b = call("v4", &[], sp()).unwrap();
        assert_ne!(a, b); // random → distinct
        let v7 = call("v7", &[], sp()).unwrap();
        if let Value::Str(s) = v7 {
            assert_eq!(&s[14..15], "7");
        } else {
            panic!();
        }
    }

    /// Dispatch under deterministic Record mode seeded by `seed`.
    ///
    /// Gated on `workflow` because `restore_determinism` is `#[cfg(feature =
    /// "workflow")]` (a partial `--features data` build without `workflow` must still
    /// compile this module's tests).
    #[cfg(feature = "workflow")]
    fn call_det(func: &str, seed: u64) -> Value {
        let interp = crate::interp::Interp::new();
        interp.restore_determinism(Some(crate::det::DeterminismContext::record(seed, 0.0)));
        super::call(&interp, func, &[], sp()).unwrap()
    }

    /// SP9 §3: under deterministic mode both the v7 time prefix (virtual clock) and
    /// the random tail (seeded PRNG) are reproducible, so two same-seed runs match —
    /// and differ across seeds. (Matches v4's determinism behavior.)
    #[cfg(feature = "workflow")]
    #[test]
    fn v7_reproducible_under_determinism() {
        assert_eq!(call_det("v7", 42), call_det("v7", 42));
        assert_ne!(call_det("v7", 42), call_det("v7", 7));
        // Still a well-formed v7 string.
        if let Value::Str(s) = call_det("v7", 42) {
            assert_eq!(s.len(), 36);
            assert_eq!(&s[14..15], "7");
        } else {
            panic!("expected string");
        }
    }

    /// And v4 likewise (kept here for symmetry with the v7 case).
    #[cfg(feature = "workflow")]
    #[test]
    fn v4_reproducible_under_determinism() {
        assert_eq!(call_det("v4", 42), call_det("v4", 42));
        assert_ne!(call_det("v4", 42), call_det("v4", 7));
    }

    /// Outside deterministic mode, v7 uses real entropy → two calls differ.
    #[test]
    fn v7_random_in_default_mode() {
        assert_ne!(call("v7", &[], sp()).unwrap(), call("v7", &[], sp()).unwrap());
    }
}
