//! `std/uuid` — UUID generation (v4 random, v7 time-ordered).

use super::bi;
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![("v4", bi("uuid.v4")), ("v7", bi("uuid.v7"))]
}

pub fn call(func: &str, _args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        "v4" => Ok(Value::Str(uuid::Uuid::new_v4().to_string().into())),
        "v7" => Ok(Value::Str(uuid::Uuid::now_v7().to_string().into())),
        _ => Err(AsError::at(format!("std/uuid has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }

    #[test]
    fn v4_v7_format() {
        let v4 = call("v4", &[], sp()).unwrap();
        if let Value::Str(s) = v4 {
            assert_eq!(s.len(), 36);
            assert_eq!(s.chars().filter(|&c| c == '-').count(), 4);
            assert_eq!(&s[14..15], "4"); // version nibble
        } else { panic!("expected string"); }
        let a = call("v4", &[], sp()).unwrap();
        let b = call("v4", &[], sp()).unwrap();
        assert_ne!(a, b); // random → distinct
        let v7 = call("v7", &[], sp()).unwrap();
        if let Value::Str(s) = v7 { assert_eq!(&s[14..15], "7"); } else { panic!(); }
    }
}
