//! Runtime values. Currently six kinds (nil, bool, number, string, builtin,
//! function); the remaining heap kinds — arrays, objects, maps — arrive in
//! Milestone 4.

use crate::ast::Stmt;
use crate::env::Environment;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// A user-defined function with its captured (closure) environment.
pub struct Function {
    pub name: Option<String>,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub closure: Environment,
}

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    Str(Rc<str>),
    /// A native built-in function, dispatched by name in the interpreter.
    Builtin(Rc<str>),
    /// A user-defined function carrying its closure environment.
    Function(Rc<Function>),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<RefCell<IndexMap<String, Value>>>),
}

impl Value {
    /// Spec §4: only `nil` and `false` are falsy. Everything else
    /// (including `0` and `""`) is truthy.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            // Built-ins are equal iff they name the same function.
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            // Functions compare by identity.
            (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "Nil"),
            Value::Bool(b) => write!(f, "Bool({})", b),
            Value::Number(n) => write!(f, "Number({})", n),
            Value::Str(s) => write!(f, "Str({:?})", s),
            Value::Builtin(name) => write!(f, "Builtin({:?})", name),
            Value::Function(func) => {
                write!(f, "Function({})", func.name.as_deref().unwrap_or("<anonymous>"))
            }
            Value::Array(a) => write!(f, "Array(len {})", a.borrow().len()),
            Value::Object(o) => write!(f, "Object(len {})", o.borrow().len()),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_display(f, &mut Vec::new())
    }
}

impl Value {
    fn write_display(&self, f: &mut fmt::Formatter<'_>, seen: &mut Vec<usize>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{}", b),
            // Rust's f64 Display already prints 7.0 as "7" and 2.5 as "2.5".
            Value::Number(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{}", s),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::Function(func) => match &func.name {
                Some(n) => write!(f, "<function {}>", n),
                None => write!(f, "<function>"),
            },
            Value::Array(a) => {
                let ptr = Rc::as_ptr(a) as usize;
                if seen.contains(&ptr) {
                    return write!(f, "[...]");
                }
                seen.push(ptr);
                write!(f, "[")?;
                for (i, v) in a.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    v.write_element(f, seen)?;
                }
                write!(f, "]")?;
                seen.pop();
                Ok(())
            }
            Value::Object(o) => {
                let ptr = Rc::as_ptr(o) as usize;
                if seen.contains(&ptr) {
                    return write!(f, "{{...}}");
                }
                seen.push(ptr);
                write!(f, "{{")?;
                for (i, (k, v)) in o.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: ", k)?;
                    v.write_element(f, seen)?;
                }
                write!(f, "}}")?;
                seen.pop();
                Ok(())
            }
        }
    }

    /// Like `write_display`, but quotes bare strings (used for nested elements
    /// so `[1, "two"]` shows the quotes while top-level `print("x")` stays raw).
    fn write_element(&self, f: &mut fmt::Formatter<'_>, seen: &mut Vec<usize>) -> fmt::Result {
        match self {
            Value::Str(s) => write!(f, "{:?}", s),
            _ => self.write_display(f, seen),
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

    #[test]
    fn builtins_compare_by_name_and_are_truthy() {
        assert_eq!(Value::Builtin("print".into()), Value::Builtin("print".into()));
        assert_ne!(Value::Builtin("print".into()), Value::Builtin("len".into()));
        assert!(Value::Builtin("print".into()).is_truthy());
        assert_eq!(Value::Builtin("print".into()).to_string(), "<builtin print>");
    }

    #[test]
    fn arrays_compare_by_identity_and_display() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let a = Value::Array(Rc::new(RefCell::new(vec![Value::Number(1.0), Value::Str("two".into())])));
        assert_eq!(a.to_string(), "[1, \"two\"]");
        // identity: a clone of the SAME Rc is equal; a fresh array is not
        assert_eq!(a.clone(), a);
        let b = Value::Array(Rc::new(RefCell::new(vec![Value::Number(1.0)])));
        assert_ne!(a, b);
        assert!(a.is_truthy());
    }

    #[test]
    fn objects_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut m = IndexMap::new();
        m.insert("a".to_string(), Value::Number(1.0));
        m.insert("b".to_string(), Value::Str("x".into()));
        let o = Value::Object(Rc::new(RefCell::new(m)));
        assert_eq!(o.to_string(), "{a: 1, b: \"x\"}");
        assert_eq!(o.clone(), o);
        assert!(o.is_truthy());
    }
}
