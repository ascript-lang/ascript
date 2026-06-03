//! Object/instance SHAPES — hidden classes (V11-T2).
//!
//! A *shape* (a.k.a. hidden class) identifies the ordered key-LAYOUT of an
//! `Object`/`Instance` independent of the values stored under those keys. Two
//! objects built with the SAME insertion-ordered keys share one shape id; two
//! objects whose keys differ — including by ORDER — get distinct ids.
//!
//! Shapes are assigned via a **transition tree**: shape `0` is the empty layout;
//! adding a key `k` to a shape `s` follows (or creates) the edge `(s, k) → child`.
//! The tree dedups, so the same key sequence always lands on the same id, and a
//! shared prefix (`{a}` then `{a,b}` vs `{a}` then `{a,c}`) reuses the common
//! ancestor `{a}`. This lets V11-T3's inline caches store `(shape_id, value-index)`
//! and validate with a single `obj.shape == cached_shape` compare before reading
//! `values.get_index(idx)`.
//!
//! The registry is **per-VM** (isolate-friendly): it lives on the `Vm` behind a
//! `RefCell` and is only ever touched by VM code paths. The tree-walker never
//! consults it — its objects keep shape `0`.

use std::collections::HashMap;

/// The empty layout (no keys). Every fresh object/instance starts here; the
/// tree-walker leaves all of its objects at this id.
pub const EMPTY_SHAPE: u32 = 0;

/// Assigns shape ids to key-layouts via a transition tree. Memoized so the same
/// insertion-ordered key sequence always yields the same id.
pub struct ShapeRegistry {
    /// `(parent_shape, key) → child_shape`. The single source of truth; an entry
    /// exists iff that transition has been taken at least once.
    transitions: HashMap<(u32, Box<str>), u32>,
    /// The next shape id to hand out. Starts at 1 because `0` is the reserved
    /// empty shape.
    next_id: u32,
}

impl Default for ShapeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ShapeRegistry {
    /// A fresh registry holding only the empty shape (`0`).
    pub fn new() -> Self {
        ShapeRegistry {
            transitions: HashMap::new(),
            next_id: 1,
        }
    }

    /// Transition `shape` by adding `key`, returning the child shape id.
    ///
    /// Memoized: the first time a given `(shape, key)` edge is requested a NEW id
    /// is minted; every later request for that same edge returns the same id. So
    /// two objects that insert the same keys in the same order converge on one id.
    pub fn add_key(&mut self, shape: u32, key: &str) -> u32 {
        if let Some(&child) = self.transitions.get(&(shape, Box::from(key))) {
            return child;
        }
        let child = self.next_id;
        self.next_id += 1;
        self.transitions.insert((shape, Box::from(key)), child);
        child
    }

    /// The shape id for an ordered sequence of keys, walking from the empty shape.
    /// Used to derive an object-literal's final shape and a class's base shape.
    pub fn shape_for<'a, I>(&mut self, keys: I) -> u32
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut shape = EMPTY_SHAPE;
        for k in keys {
            shape = self.add_key(shape, k);
        }
        shape
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object_is_shape_zero() {
        let mut reg = ShapeRegistry::new();
        assert_eq!(reg.shape_for(std::iter::empty::<&str>()), EMPTY_SHAPE);
        assert_eq!(EMPTY_SHAPE, 0);
    }

    #[test]
    fn same_keys_same_shape() {
        let mut reg = ShapeRegistry::new();
        let a = reg.shape_for(["a", "b"]);
        let b = reg.shape_for(["a", "b"]);
        assert_eq!(a, b, "two objects with keys [a,b] must share a shape");
        assert_ne!(a, EMPTY_SHAPE);
    }

    #[test]
    fn key_order_matters() {
        let mut reg = ShapeRegistry::new();
        let ab = reg.shape_for(["a", "b"]);
        let ba = reg.shape_for(["b", "a"]);
        assert_ne!(ab, ba, "[a,b] and [b,a] are different layouts");
    }

    #[test]
    fn adding_a_key_transitions_to_a_child_shape() {
        let mut reg = ShapeRegistry::new();
        let a = reg.shape_for(["a"]);
        let ab = reg.add_key(a, "b");
        assert_ne!(a, ab, "adding a key must transition to a new shape");
        // The transition is memoized: re-adding from the same parent is stable.
        assert_eq!(reg.add_key(a, "b"), ab);
        // And `{a,b}` reached via shape_for equals the same child (shared prefix).
        assert_eq!(reg.shape_for(["a", "b"]), ab);
    }

    #[test]
    fn shared_prefix_reuses_ancestor() {
        let mut reg = ShapeRegistry::new();
        let a = reg.shape_for(["a"]);
        let ab = reg.shape_for(["a", "b"]);
        let ac = reg.shape_for(["a", "c"]);
        // Both branch off the SAME `{a}` parent, so they differ from each other
        // but each is a direct child of `a`.
        assert_ne!(ab, ac);
        assert_eq!(reg.add_key(a, "b"), ab);
        assert_eq!(reg.add_key(a, "c"), ac);
    }

    #[test]
    fn distinct_ids_are_monotonic() {
        let mut reg = ShapeRegistry::new();
        // Each genuinely-new edge mints the next id; the empty shape is excluded.
        let ids = [
            reg.add_key(EMPTY_SHAPE, "x"),
            reg.add_key(EMPTY_SHAPE, "y"),
            reg.add_key(EMPTY_SHAPE, "z"),
        ];
        assert_eq!(ids, [1, 2, 3]);
    }
}
