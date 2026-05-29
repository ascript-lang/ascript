//! Runtime values. The skeleton supports four of the eight value kinds
//! from spec §4; the rest arrive in later milestones.

use std::fmt;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    Str(Rc<str>),
}

impl Value {
    /// Spec §4: only `nil` and `false` are falsy. Everything else
    /// (including `0` and `""`) is truthy.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{}", b),
            // Rust's f64 Display already prints 7.0 as "7" and 2.5 as "2.5".
            Value::Number(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{}", s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_values_like_a_script_language() {
        assert_eq!(Value::Number(7.0).to_string(), "7");
        assert_eq!(Value::Number(2.5).to_string(), "2.5");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Nil.to_string(), "nil");
        assert_eq!(Value::Str("hi".into()).to_string(), "hi");
    }

    #[test]
    fn truthiness_follows_spec() {
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Number(0.0).is_truthy());
        assert!(Value::Str("".into()).is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Nil.is_truthy());
    }

    #[test]
    fn equality_is_structural_and_cross_kind_is_false() {
        assert_eq!(Value::Number(1.0), Value::Number(1.0));
        assert_eq!(Value::Str("a".into()), Value::Str("a".into()));
        assert_ne!(Value::Number(1.0), Value::Str("1".into()));
        assert_ne!(Value::Bool(true), Value::Number(1.0));
    }
}
