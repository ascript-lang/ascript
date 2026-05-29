//! Lexical scope chain. `Environment` is a cheap-to-clone handle to a scope;
//! child scopes link to their parent so name lookup walks outward. Single
//! threaded, so `Rc<RefCell<…>>` (never `Arc`/`Mutex`).

use crate::value::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

struct Binding {
    value: Value,
    mutable: bool,
}

struct Scope {
    vars: HashMap<String, Binding>,
    parent: Option<Environment>,
}

/// A handle to a lexical scope. Cloning shares the same underlying scope.
#[derive(Clone)]
pub struct Environment(Rc<RefCell<Scope>>);

/// Why an assignment failed.
#[derive(Debug, PartialEq)]
pub enum AssignError {
    Undefined,
    Immutable,
}

impl Environment {
    /// Create a new, empty global scope.
    pub fn global() -> Self {
        Environment(Rc::new(RefCell::new(Scope { vars: HashMap::new(), parent: None })))
    }

    /// Create a new child scope whose parent is `self`.
    pub fn child(&self) -> Self {
        Environment(Rc::new(RefCell::new(Scope {
            vars: HashMap::new(),
            parent: Some(self.clone()),
        })))
    }

    /// Define a binding in THIS scope. Errors if the name is already bound here
    /// (shadowing an outer scope is allowed; redefining in the same scope is not).
    pub fn define(&self, name: &str, value: Value, mutable: bool) -> Result<(), String> {
        let mut scope = self.0.borrow_mut();
        if scope.vars.contains_key(name) {
            return Err(format!("'{}' is already defined in this scope", name));
        }
        scope.vars.insert(name.to_string(), Binding { value, mutable });
        Ok(())
    }

    /// Look up a name, walking outward through parent scopes.
    pub fn get(&self, name: &str) -> Option<Value> {
        let scope = self.0.borrow();
        if let Some(binding) = scope.vars.get(name) {
            return Some(binding.value.clone());
        }
        match &scope.parent {
            Some(parent) => parent.get(name),
            None => None,
        }
    }

    /// Assign to an EXISTING binding, walking outward. Errors if not found
    /// (Undefined) or the binding is immutable (Immutable).
    pub fn assign(&self, name: &str, value: Value) -> Result<(), AssignError> {
        let mut scope = self.0.borrow_mut();
        if let Some(binding) = scope.vars.get_mut(name) {
            if !binding.mutable {
                return Err(AssignError::Immutable);
            }
            binding.value = value;
            return Ok(());
        }
        match &scope.parent {
            Some(parent) => parent.assign(name, value),
            None => Err(AssignError::Undefined),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defines_and_gets() {
        let env = Environment::global();
        env.define("x", Value::Number(5.0), true).unwrap();
        assert!(matches!(env.get("x"), Some(Value::Number(n)) if n == 5.0));
    }

    #[test]
    fn redefining_in_same_scope_errors() {
        let env = Environment::global();
        env.define("x", Value::Number(1.0), true).unwrap();
        assert!(env.define("x", Value::Number(2.0), true).is_err());
    }

    #[test]
    fn child_reads_parent_but_can_shadow() {
        let parent = Environment::global();
        parent.define("x", Value::Number(1.0), true).unwrap();
        let child = parent.child();
        assert!(matches!(child.get("x"), Some(Value::Number(n)) if n == 1.0));
        child.define("x", Value::Number(9.0), true).unwrap();
        assert!(matches!(child.get("x"), Some(Value::Number(n)) if n == 9.0));
        assert!(matches!(parent.get("x"), Some(Value::Number(n)) if n == 1.0));
    }

    #[test]
    fn assign_walks_outward_and_respects_mutability() {
        let parent = Environment::global();
        parent.define("m", Value::Number(1.0), true).unwrap();
        parent.define("c", Value::Number(2.0), false).unwrap();
        let child = parent.child();
        child.assign("m", Value::Number(10.0)).unwrap();
        assert!(matches!(parent.get("m"), Some(Value::Number(n)) if n == 10.0));
        assert_eq!(child.assign("c", Value::Number(3.0)), Err(AssignError::Immutable));
        assert_eq!(child.assign("nope", Value::Nil), Err(AssignError::Undefined));
    }
}
