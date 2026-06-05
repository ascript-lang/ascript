//! The inferred-binding environment + narrowing overlay (SP10 §2 / §4).
//!
//! `Env` maps a [`BindingKey`] (derived from a `Resolution`) to its inferred or
//! declared [`CheckTy`], with push/pop scopes. The narrowing overlay is a separate
//! `HashMap<BindingKey, CheckTy>` pushed/popped at branch boundaries — it refines a
//! binding's type within a flow region WITHOUT mutating the underlying inferred
//! type. (Populated in T3/T4; T1 ships the structure.)

use crate::check::infer::ty::CheckTy;
use crate::syntax::resolve::types::Resolution;
use std::collections::HashMap;

/// A stable key for a binding, derived from its `Resolution`. A local/upvalue is
/// keyed by frame-relative slot within the current frame; a global by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BindingKey {
    Local(u32),
    Upvalue(u32),
    Global(String),
}

impl BindingKey {
    /// Derive a key from a resolved use. `Unresolved` → `None`.
    pub fn from_resolution(res: &Resolution) -> Option<BindingKey> {
        match res {
            Resolution::Local(s) => Some(BindingKey::Local(*s)),
            Resolution::Upvalue(s) => Some(BindingKey::Upvalue(*s)),
            Resolution::Global(n) => Some(BindingKey::Global(n.clone())),
            Resolution::Unresolved => None,
        }
    }
}

/// Inferred/declared binding types for the CURRENT frame, plus a narrowing overlay.
///
/// The base map is the binding's stable inferred/declared type. The overlay is a
/// stack of flow-scoped refinements; `lookup` consults the overlay (innermost
/// first) before the base map.
#[derive(Debug, Default)]
pub struct Env {
    base: HashMap<BindingKey, CheckTy>,
    overlay: Vec<HashMap<BindingKey, CheckTy>>,
}

impl Env {
    pub fn new() -> Env {
        Env::default()
    }

    /// Bind `key` to its inferred/declared type in the base environment.
    pub fn define(&mut self, key: BindingKey, ty: CheckTy) {
        self.base.insert(key, ty);
    }

    /// Look up a binding's CURRENT type: the innermost narrowing refinement if any,
    /// else its base type, else `None`.
    pub fn lookup(&self, key: &BindingKey) -> Option<CheckTy> {
        for frame in self.overlay.iter().rev() {
            if let Some(ty) = frame.get(key) {
                return Some(ty.clone());
            }
        }
        self.base.get(key).cloned()
    }

    /// The binding's BASE (un-narrowed) type, ignoring the overlay.
    pub fn base_type(&self, key: &BindingKey) -> Option<CheckTy> {
        self.base.get(key).cloned()
    }

    /// Push a fresh narrowing scope (a branch boundary).
    pub fn push_narrowing(&mut self) {
        self.overlay.push(HashMap::new());
    }

    /// Pop the innermost narrowing scope, returning it (for flow merging).
    pub fn pop_narrowing(&mut self) -> HashMap<BindingKey, CheckTy> {
        self.overlay.pop().unwrap_or_default()
    }

    /// Record a narrowing refinement in the innermost scope (no-op if no scope is
    /// open — a narrow at the top level is meaningless).
    pub fn narrow(&mut self, key: BindingKey, ty: CheckTy) {
        if let Some(frame) = self.overlay.last_mut() {
            frame.insert(key, ty);
        }
    }

    /// Apply a set of refinements directly into the innermost scope (used to install
    /// the negation of an early-return guard for the rest of a block).
    pub fn apply_narrowing(&mut self, refinements: HashMap<BindingKey, CheckTy>) {
        if let Some(frame) = self.overlay.last_mut() {
            frame.extend(refinements);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_then_overlay() {
        let mut env = Env::new();
        let k = BindingKey::Local(0);
        env.define(k.clone(), CheckTy::Number);
        assert_eq!(env.lookup(&k), Some(CheckTy::Number));

        env.push_narrowing();
        env.narrow(k.clone(), CheckTy::String);
        assert_eq!(env.lookup(&k), Some(CheckTy::String));
        assert_eq!(env.base_type(&k), Some(CheckTy::Number));

        env.pop_narrowing();
        assert_eq!(env.lookup(&k), Some(CheckTy::Number));
    }

    #[test]
    fn key_from_resolution() {
        assert_eq!(
            BindingKey::from_resolution(&Resolution::Local(3)),
            Some(BindingKey::Local(3))
        );
        assert_eq!(
            BindingKey::from_resolution(&Resolution::Global("g".into())),
            Some(BindingKey::Global("g".into()))
        );
        assert_eq!(BindingKey::from_resolution(&Resolution::Unresolved), None);
    }
}
