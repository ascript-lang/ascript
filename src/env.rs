//! Lexical scope chain. `Environment` is a cheap-to-clone handle to a scope;
//! child scopes link to their parent so name lookup walks outward. Single
//! threaded, so `Rc<RefCell<…>>` (never `Arc`/`Mutex`).

use crate::interp::DeferEntry;
use crate::value::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// A per-activation defer list (spec §5.1): the ordered list of [`DeferEntry`]s
/// registered by `defer` statements executing in a given call activation. Drained
/// LIFO at frame exit. Shared via `Rc` so `run_body` keeps a handle to drain it
/// while the `Stmt::Defer` evaluator (reached through the env chain) appends to it.
pub(crate) type DeferList = Rc<RefCell<Vec<DeferEntry>>>;

struct Binding {
    value: Value,
    mutable: bool,
}

struct Scope {
    vars: HashMap<String, Binding>,
    parent: Option<Environment>,
    /// DEFER §5.1: the defer list OWNED by this scope, if this scope is an
    /// activation boundary (a function call frame or the program/module root).
    /// `None` for ordinary lexical child scopes (blocks, loop bodies) and for
    /// `global()`/`child()` — those resolve their enclosing activation's list by
    /// walking parents to the nearest `Some` via [`Environment::defer_scope`].
    defers: Option<DeferList>,
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
        Environment(Rc::new(RefCell::new(Scope {
            vars: HashMap::new(),
            parent: None,
            defers: None,
        })))
    }

    /// Create a new child scope whose parent is `self`.
    pub fn child(&self) -> Self {
        Environment(Rc::new(RefCell::new(Scope {
            vars: HashMap::new(),
            parent: Some(self.clone()),
            defers: None,
        })))
    }

    /// DEFER §5.1: install a FRESH defer list on THIS scope, marking it an
    /// activation boundary, and return the shared handle. Called by `run_body`
    /// (per function call) and the program/module/REPL drivers (per top-level run)
    /// on the activation's own env. The returned `Rc` is what the caller drains at
    /// frame exit; `Stmt::Defer` appends to the SAME list via [`Self::defer_scope`].
    /// Idempotent-by-replacement: a second call replaces the list (never observed
    /// in practice — each activation installs exactly once on a fresh env).
    pub(crate) fn install_defer_scope(&self) -> DeferList {
        let list: DeferList = Rc::new(RefCell::new(Vec::new()));
        self.0.borrow_mut().defers = Some(Rc::clone(&list));
        list
    }

    /// DEFER §5.1: resolve the NEAREST enclosing activation's defer list by walking
    /// parents to the first scope carrying `Some`. Concurrency-sound: each live
    /// activation installs its OWN list on its OWN `call_env`, and a closure's
    /// definition env sits BEHIND the callee's call env in the chain, so this always
    /// resolves the CURRENTLY-EXECUTING activation's list regardless of how many
    /// other activations are concurrently suspended (unlike an `Interp`-level stack,
    /// whose `last()` is corrupted by interleaving). `None` only if no activation
    /// boundary was installed above this scope (a bare env with no driver).
    pub(crate) fn defer_scope(&self) -> Option<DeferList> {
        let scope = self.0.borrow();
        if let Some(list) = &scope.defers {
            return Some(Rc::clone(list));
        }
        match &scope.parent {
            Some(parent) => parent.defer_scope(),
            None => None,
        }
    }

    /// Define a binding in THIS scope. Errors if the name is already bound here
    /// (shadowing an outer scope is allowed; redefining in the same scope is not).
    pub fn define(&self, name: &str, value: Value, mutable: bool) -> Result<(), String> {
        let mut scope = self.0.borrow_mut();
        if scope.vars.contains_key(name) {
            return Err(format!("'{}' is already defined in this scope", name));
        }
        scope
            .vars
            .insert(name.to_string(), Binding { value, mutable });
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
        env.define("x", Value::float(5.0), true).unwrap();
        assert!(matches!(env.get("x").map(|v| v.into_kind()), Some(crate::value::OwnedKind::Float(n)) if n == 5.0));
    }

    #[test]
    fn redefining_in_same_scope_errors() {
        let env = Environment::global();
        env.define("x", Value::float(1.0), true).unwrap();
        assert!(env.define("x", Value::float(2.0), true).is_err());
    }

    #[test]
    fn child_reads_parent_but_can_shadow() {
        let parent = Environment::global();
        parent.define("x", Value::float(1.0), true).unwrap();
        let child = parent.child();
        assert!(matches!(child.get("x").map(|v| v.into_kind()), Some(crate::value::OwnedKind::Float(n)) if n == 1.0));
        child.define("x", Value::float(9.0), true).unwrap();
        assert!(matches!(child.get("x").map(|v| v.into_kind()), Some(crate::value::OwnedKind::Float(n)) if n == 9.0));
        assert!(matches!(parent.get("x").map(|v| v.into_kind()), Some(crate::value::OwnedKind::Float(n)) if n == 1.0));
    }

    #[test]
    fn assign_walks_outward_and_respects_mutability() {
        let parent = Environment::global();
        parent.define("m", Value::float(1.0), true).unwrap();
        parent.define("c", Value::float(2.0), false).unwrap();
        let child = parent.child();
        child.assign("m", Value::float(10.0)).unwrap();
        assert!(matches!(parent.get("m").map(|v| v.into_kind()), Some(crate::value::OwnedKind::Float(n)) if n == 10.0));
        assert_eq!(
            child.assign("c", Value::float(3.0)),
            Err(AssignError::Immutable)
        );
        assert_eq!(
            child.assign("nope", Value::nil()),
            Err(AssignError::Undefined)
        );
    }
}
