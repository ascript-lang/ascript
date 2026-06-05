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
        "v7" => Ok(Value::Str(uuid::Uuid::now_v7().to_string().into())),
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
}
