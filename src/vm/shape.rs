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
//!
//! ## SHAPE v2 changes (Task 2.1)
//!
//! - The registry now **owns** the canonical key list for every shape id
//!   (`keys: Vec<Rc<[Rc<str>]>>`), making it the layout authority for Phase 2.2.
//! - Transitions are stored as a two-level `FxHashMap<u32, FxHashMap<Box<str>, u32>>`
//!   so probes use a borrowed `&str` — zero allocation on the hot path.
//! - Two caps guard against pathological fan-out and very-wide objects:
//!   `SLAB_MAX_KEYS` (key-count cap) and `SHAPE_FANOUT_MAX` (transition fan-out cap).
//!   Both caps are far above anything the current corpus hits, so all existing
//!   callers stay behaviorally identical; the corpus differential remains 424/0.

use std::rc::Rc;

use rustc_hash::FxHashMap;

/// The empty layout (no keys). Every fresh object/instance starts here; the
/// tree-walker leaves all of its objects at this id.
pub const EMPTY_SHAPE: u32 = 0;

/// Maximum number of keys a shaped slab may hold (V8 fast-properties precedent).
/// A `shape_for`/`add_key` that would push a shape past this limit returns `None`.
pub const SLAB_MAX_KEYS: usize = 64;

/// Maximum number of distinct child transitions a single parent shape may have.
/// Exceeding this returns `None` (caller demotes to unshaped / shape 0).
/// An ALREADY-MINTED edge always resolves even after the cap is hit (memoized).
pub const SHAPE_FANOUT_MAX: usize = 128;

/// Assigns shape ids to key-layouts via a transition tree. Memoized so the same
/// insertion-ordered key sequence always yields the same id.
///
/// ## Internal layout
///
/// ```text
/// transitions: parent_id → { key → child_id }
/// keys[id]:   the canonical ordered key list for that id
/// ```
///
/// `keys[0]` is always the empty slice (EMPTY_SHAPE). `keys` is dense and
/// id-indexed (id == index), so a `keys_of` lookup is a single slice dereference.
pub struct ShapeRegistry {
    /// `parent_shape → (key → child_shape)`. Two-level so a lookup probes with a
    /// borrowed `&str` (via `Borrow<str>` on `Box<str>` keys) — kills the
    /// per-probe `Box::from(key)` allocation of the v1 design.
    transitions: FxHashMap<u32, FxHashMap<Box<str>, u32>>,
    /// `shape_id → canonical ordered key list`. Dense, id-indexed.
    /// `keys[0]` = the empty slice (EMPTY_SHAPE). Shared via `Rc` so multiple
    /// callers calling `keys_of` for the same id get the exact same allocation.
    keys: Vec<Rc<[Rc<str>]>>,
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
            transitions: FxHashMap::default(),
            // Slot 0 = EMPTY_SHAPE → empty key list.
            keys: vec![Rc::from([])],
        }
    }

    /// Transition `shape` by adding `key`, returning `Some(child_shape_id)`.
    ///
    /// Returns `None` when either cap would be exceeded:
    /// - the new child's key list would exceed `SLAB_MAX_KEYS`, OR
    /// - the parent's fan-out would exceed `SHAPE_FANOUT_MAX`.
    ///
    /// An already-minted edge ALWAYS resolves (memoized), even after the parent
    /// hit its fan-out cap. Only inserting a NEW edge is subject to the caps.
    ///
    /// **Probe path allocates nothing**: the inner map's `Box<str>` keys implement
    /// `Borrow<str>`, so `.get(key)` where `key: &str` resolves with no allocation.
    pub fn add_key(&mut self, shape: u32, key: &str) -> Option<u32> {
        // 1. Probe with borrowed &str — no allocation on the hot path.
        if let Some(child) = self
            .transitions
            .get(&shape)
            .and_then(|inner| inner.get(key))
            .copied()
        {
            return Some(child);
        }

        // 2. Cap checks before minting a new edge.
        let parent_key_count = self.keys[shape as usize].len();
        if parent_key_count + 1 > SLAB_MAX_KEYS {
            return None; // key-count cap
        }
        let fan_out = self.transitions.get(&shape).map_or(0, |m| m.len());
        if fan_out >= SHAPE_FANOUT_MAX {
            return None; // fan-out cap
        }

        // 3. Mint a new shape id and build its canonical key list.
        let child_id = self.keys.len() as u32;
        let parent_keys = &self.keys[shape as usize];
        let mut new_keys: Vec<Rc<str>> = Vec::with_capacity(parent_keys.len() + 1);
        new_keys.extend_from_slice(parent_keys);
        new_keys.push(Rc::from(key));
        self.keys.push(Rc::from(new_keys.as_slice()));

        // 4. Insert the edge (only NOW allocate a Box<str> for the map key).
        self.transitions
            .entry(shape)
            .or_default()
            .insert(Box::from(key), child_id);

        Some(child_id)
    }

    /// The shape id for an ordered sequence of keys, walking from the empty shape.
    ///
    /// Returns `None` if any step along the chain hits a cap (key-count or fan-out).
    /// Used to derive an object-literal's final shape and a class's base shape.
    pub fn shape_for<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>) -> Option<u32> {
        let mut shape = EMPTY_SHAPE;
        for k in keys {
            shape = self.add_key(shape, k)?;
        }
        Some(shape)
    }

    /// The canonical ordered key list for `shape`. Returns an `Rc`-shared slice —
    /// all callers for the same id share one allocation (verified by `Rc::ptr_eq`).
    pub fn keys_of(&self, shape: u32) -> Rc<[Rc<str>]> {
        self.keys[shape as usize].clone()
    }

    /// WARM B §3.1 — PGO harvest: return the ordered key list for `shape` as a
    /// `Vec<String>`, or `None` if `shape` is not a registered id.
    ///
    /// This is the safe, `Option`-returning variant used by the PGO recorder. The
    /// production `keys_of` above panics on an out-of-range id (it is always called
    /// with a registry-minted id) — for untrusted/stale shape ids from a running Vm
    /// that may have demoted a slab, we return `None` instead.
    ///
    /// `EMPTY_SHAPE` (0) ⇒ `Some([])` — the empty key list is always registered.
    pub fn keys_of_pgo(&self, shape: u32) -> Option<Vec<String>> {
        let entry = self.keys.get(shape as usize)?;
        Some(entry.iter().map(|k| k.as_ref().to_owned()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── v2 tests (Task 2.1) ──────────────────────────────────────────────────

    // ── WARM B §3.1 — keys_of_pgo tests ────────────────────────────────────

    #[test]
    fn keys_of_pgo_reverses_interned_list() {
        let mut reg = ShapeRegistry::new();
        let abc = reg.shape_for(["a", "b", "c"]).unwrap();
        let keys = reg.keys_of_pgo(abc).unwrap();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn keys_of_pgo_empty_shape_returns_some_empty() {
        let reg = ShapeRegistry::new();
        let keys = reg.keys_of_pgo(EMPTY_SHAPE).unwrap();
        assert!(keys.is_empty(), "EMPTY_SHAPE must yield an empty list");
    }

    #[test]
    fn keys_of_pgo_unknown_id_returns_none() {
        let reg = ShapeRegistry::new();
        // Shape id 9999 was never registered.
        assert_eq!(reg.keys_of_pgo(9999), None);
    }

    // ── v2 tests (Task 2.1) ──────────────────────────────────────────────────

    #[test]
    fn keys_of_returns_canonical_shared_list() {
        let mut reg = ShapeRegistry::new();
        let ab = reg.shape_for(["a", "b"]).unwrap();
        let k1 = reg.keys_of(ab);
        let k2 = reg.keys_of(ab);
        assert!(Rc::ptr_eq(&k1, &k2), "one allocation per LAYOUT, shared");
        assert_eq!(&*k1[0], "a");
        assert_eq!(&*k1[1], "b");
        assert_eq!(reg.keys_of(EMPTY_SHAPE).len(), 0);
    }

    #[test]
    fn caps_refuse_instead_of_minting() {
        let mut reg = ShapeRegistry::new();
        // Fan-out cap: mint exactly SHAPE_FANOUT_MAX children from EMPTY_SHAPE.
        for i in 0..SHAPE_FANOUT_MAX {
            assert!(
                reg.add_key(EMPTY_SHAPE, &format!("k{i}")).is_some(),
                "edge {i} should mint"
            );
        }
        // The next NEW key from EMPTY_SHAPE is refused.
        assert_eq!(
            reg.add_key(EMPTY_SHAPE, "one_too_many"),
            None,
            "fan-out cap must refuse"
        );
        // An already-minted edge still resolves (memoized, never refused).
        assert!(
            reg.add_key(EMPTY_SHAPE, "k0").is_some(),
            "already-minted edge resolves even after cap"
        );

        // Key-count cap: shape_for a sequence longer than SLAB_MAX_KEYS must fail.
        let labels: Vec<String> = (0..=SLAB_MAX_KEYS).map(|i| format!("c{i}")).collect();
        assert!(
            reg.shape_for(labels.iter().map(|s| s.as_str())).is_none(),
            "key-count cap must refuse"
        );
    }

    // ── existing v1 tests (adapted: add .unwrap() where add_key / shape_for return Option) ──

    #[test]
    fn empty_object_is_shape_zero() {
        let mut reg = ShapeRegistry::new();
        assert_eq!(
            reg.shape_for(std::iter::empty::<&str>()).unwrap(),
            EMPTY_SHAPE
        );
        assert_eq!(EMPTY_SHAPE, 0);
    }

    #[test]
    fn same_keys_same_shape() {
        let mut reg = ShapeRegistry::new();
        let a = reg.shape_for(["a", "b"]).unwrap();
        let b = reg.shape_for(["a", "b"]).unwrap();
        assert_eq!(a, b, "two objects with keys [a,b] must share a shape");
        assert_ne!(a, EMPTY_SHAPE);
    }

    #[test]
    fn key_order_matters() {
        let mut reg = ShapeRegistry::new();
        let ab = reg.shape_for(["a", "b"]).unwrap();
        let ba = reg.shape_for(["b", "a"]).unwrap();
        assert_ne!(ab, ba, "[a,b] and [b,a] are different layouts");
    }

    #[test]
    fn adding_a_key_transitions_to_a_child_shape() {
        let mut reg = ShapeRegistry::new();
        let a = reg.shape_for(["a"]).unwrap();
        let ab = reg.add_key(a, "b").unwrap();
        assert_ne!(a, ab, "adding a key must transition to a new shape");
        // The transition is memoized: re-adding from the same parent is stable.
        assert_eq!(reg.add_key(a, "b").unwrap(), ab);
        // And `{a,b}` reached via shape_for equals the same child (shared prefix).
        assert_eq!(reg.shape_for(["a", "b"]).unwrap(), ab);
    }

    #[test]
    fn shared_prefix_reuses_ancestor() {
        let mut reg = ShapeRegistry::new();
        let a = reg.shape_for(["a"]).unwrap();
        let ab = reg.shape_for(["a", "b"]).unwrap();
        let ac = reg.shape_for(["a", "c"]).unwrap();
        // Both branch off the SAME `{a}` parent, so they differ from each other
        // but each is a direct child of `a`.
        assert_ne!(ab, ac);
        assert_eq!(reg.add_key(a, "b").unwrap(), ab);
        assert_eq!(reg.add_key(a, "c").unwrap(), ac);
    }

    #[test]
    fn distinct_ids_are_monotonic() {
        let mut reg = ShapeRegistry::new();
        // Each genuinely-new edge mints the next id; the empty shape is excluded.
        let ids = [
            reg.add_key(EMPTY_SHAPE, "x").unwrap(),
            reg.add_key(EMPTY_SHAPE, "y").unwrap(),
            reg.add_key(EMPTY_SHAPE, "z").unwrap(),
        ];
        assert_eq!(ids, [1, 2, 3]);
    }
}
