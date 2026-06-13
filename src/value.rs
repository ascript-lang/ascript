//! Runtime values. Kinds: nil, bool, number, decimal, string, builtin, function,
//! array, object, map, set, enum, enum-variant, class, instance, bound-method,
//! super-ref, future.

use crate::ast::Stmt;
use crate::env::Environment;
use gcmodule::{Cc, Trace, Tracer};
use indexmap::{IndexMap, IndexSet};
use rust_decimal::Decimal;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

/// The interior storage of `ObjectCell`. Two modes:
/// - `Slab`: shape-native, values indexed by position in the registry's canonical key list.
/// - `Dict`: dictionary fallback (IndexMap representation). Always shape 0.
pub enum ObjectStorage {
    /// Shape-native mode. `keys` is the registry's canonical `Rc<[Rc<str>]>` for this
    /// shape (shared, immutable — one allocation per LAYOUT, not per object);
    /// `values[i]` is the value of `keys[i]`. Invariant: `keys.len() == values.len()`.
    Slab {
        keys: Rc<[Rc<str>]>,
        values: Vec<Value>,
    },
    /// Dictionary fallback — insertion order preserved.
    /// Always `cell.shape == 0` (the EMPTY_SHAPE sentinel; ICs never cache shape-0).
    Dict(IndexMap<String, Value>),
}

// ── SHAPE Task 3.4 — shared accessor bodies over `&ObjectStorage` ─────────────
// Free functions so `ObjectCell` (storage behind a `RefCell`) and `Instance`
// (storage held directly inside the `Cc<RefCell<Instance>>`) share ONE copy of the
// slab/dict logic. `ObjectCell`'s methods delegate through its `RefCell`; the
// `Instance` accessors call these directly on `&self.fields` / `&mut self.fields`.
// Dict mode replicates today's IndexMap behavior EXACTLY (the migration must keep
// every mode byte-identical).

impl ObjectStorage {
    /// Number of entries.
    pub fn len(&self) -> usize {
        match self {
            ObjectStorage::Slab { values, .. } => values.len(),
            ObjectStorage::Dict(m) => m.len(),
        }
    }

    /// `true` when there are no entries.
    pub fn is_empty(&self) -> bool {
        match self {
            ObjectStorage::Slab { values, .. } => values.is_empty(),
            ObjectStorage::Dict(m) => m.is_empty(),
        }
    }

    /// `true` if the storage is in slab mode (shape-native).
    pub fn is_slab(&self) -> bool {
        matches!(self, ObjectStorage::Slab { .. })
    }

    /// Clone of the value stored at `key`, or `None`.
    pub fn get(&self, key: &str) -> Option<Value> {
        match self {
            ObjectStorage::Slab { keys, values } => {
                keys.iter().position(|k| k.as_ref() == key).map(|i| values[i].clone())
            }
            ObjectStorage::Dict(m) => m.get(key).cloned(),
        }
    }

    /// `true` if `key` is present.
    pub fn contains_key(&self, key: &str) -> bool {
        match self {
            ObjectStorage::Slab { keys, .. } => keys.iter().any(|k| k.as_ref() == key),
            ObjectStorage::Dict(m) => m.contains_key(key),
        }
    }

    /// Insertion-order position of `key`, or `None`.
    pub fn get_index_of(&self, key: &str) -> Option<usize> {
        match self {
            ObjectStorage::Slab { keys, .. } => keys.iter().position(|k| k.as_ref() == key),
            ObjectStorage::Dict(m) => m.get_index_of(key),
        }
    }

    /// Key + value at position `i` (cloned), or `None`.
    pub fn get_index(&self, i: usize) -> Option<(Rc<str>, Value)> {
        match self {
            ObjectStorage::Slab { keys, values } => {
                if i < values.len() {
                    Some((keys[i].clone(), values[i].clone()))
                } else {
                    None
                }
            }
            ObjectStorage::Dict(m) => m.get_index(i).map(|(k, v)| (Rc::from(k.as_str()), v.clone())),
        }
    }

    /// Value at position `i` (cloned), or `None`.
    pub fn value_at(&self, i: usize) -> Option<Value> {
        match self {
            ObjectStorage::Slab { values, .. } => values.get(i).cloned(),
            ObjectStorage::Dict(m) => m.get_index(i).map(|(_, v)| v.clone()),
        }
    }

    /// Overwrite the value at existing slot `i`. Returns `false` if `i >= len()`.
    pub fn set_value_at(&mut self, i: usize, v: Value) -> bool {
        match self {
            ObjectStorage::Slab { values, .. } => {
                if i < values.len() {
                    values[i] = v;
                    true
                } else {
                    false
                }
            }
            ObjectStorage::Dict(m) => {
                if let Some((_, slot)) = m.get_index_mut(i) {
                    *slot = v;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Materialise `(key, value)` pairs from a slab IN ORDER into a fresh `IndexMap`
    /// and replace `self` with `Dict`. No-op if already dict. The caller is
    /// responsible for resetting any associated `shape` to `0` (EMPTY_SHAPE).
    pub fn demote_to_dict(&mut self) {
        if let ObjectStorage::Slab { keys, values } = self {
            let mut map = IndexMap::with_capacity(keys.len());
            for (k, v) in keys.iter().zip(values.iter()) {
                map.insert(k.to_string(), v.clone());
            }
            *self = ObjectStorage::Dict(map);
        }
    }

    /// Insert or overwrite `key → v` (IndexMap semantics — existing key keeps its
    /// position, new key appends). On a slab with a NEW key (no registry at hand)
    /// this demotes to dict first; slab transitions are the VM's job via the Vm
    /// registry, NOT this accessor.
    pub fn insert(&mut self, key: &str, v: Value) {
        let existing_idx = self.get_index_of(key);
        if let Some(i) = existing_idx {
            self.set_value_at(i, v);
        } else {
            if self.is_slab() {
                self.demote_to_dict();
            }
            match self {
                ObjectStorage::Dict(m) => {
                    m.insert(key.to_string(), v);
                }
                ObjectStorage::Slab { .. } => unreachable!("just demoted"),
            }
        }
    }

    /// Remove `key`, preserving the relative order of the others. Slab → demote
    /// to dict first (caller resets shape), then `shift_remove`.
    pub fn shift_remove(&mut self, key: &str) -> Option<Value> {
        if self.is_slab() {
            self.demote_to_dict();
        }
        match self {
            ObjectStorage::Dict(m) => m.shift_remove(key),
            ObjectStorage::Slab { .. } => unreachable!("just demoted"),
        }
    }

    /// Snapshot all `(key, value)` pairs in insertion order.
    pub fn entries(&self) -> Vec<(Rc<str>, Value)> {
        match self {
            ObjectStorage::Slab { keys, values } => {
                keys.iter().zip(values.iter()).map(|(k, v)| (k.clone(), v.clone())).collect()
            }
            ObjectStorage::Dict(m) => {
                m.iter().map(|(k, v)| (Rc::from(k.as_str()), v.clone())).collect()
            }
        }
    }

    /// Call `f(key, value)` for every entry in insertion order (no allocation).
    pub fn for_each<F: FnMut(&str, &Value)>(&self, mut f: F) {
        match self {
            ObjectStorage::Slab { keys, values } => {
                for (k, v) in keys.iter().zip(values.iter()) {
                    f(k.as_ref(), v);
                }
            }
            ObjectStorage::Dict(m) => {
                for (k, v) in m.iter() {
                    f(k.as_str(), v);
                }
            }
        }
    }

    /// Snapshot the insertion-order key list as owned `String`s.
    pub fn keys_snapshot(&self) -> Vec<String> {
        match self {
            ObjectStorage::Slab { keys, .. } => keys.iter().map(|k| k.to_string()).collect(),
            ObjectStorage::Dict(m) => m.keys().cloned().collect(),
        }
    }

    /// Clone the whole entry map into a fresh `IndexMap`.
    pub fn to_index_map(&self) -> IndexMap<String, Value> {
        match self {
            ObjectStorage::Slab { keys, values } => {
                let mut m = IndexMap::with_capacity(keys.len());
                for (k, v) in keys.iter().zip(values.iter()) {
                    m.insert(k.to_string(), v.clone());
                }
                m
            }
            ObjectStorage::Dict(m) => m.clone(),
        }
    }
}

/// The heap payload behind `Value::Object`. Wraps an `ObjectStorage` (slab or dict)
/// together with a `shape` id (hidden classes) and a `frozen` flag.
///
/// `shape` defaults to `0` (EMPTY_SHAPE). The TREE-WALKER never reads or writes it
/// (its objects keep shape 0); only VM code paths assign shapes. The `borrow`/
/// `borrow_mut` helpers are kept for legacy call-sites; they panic on `Slab` mode
/// (the VM never calls them on slab objects — pre-Phase-3 all corpus objects are
/// dict-built so the panic is unreachable in practice).
pub struct ObjectCell {
    storage: RefCell<ObjectStorage>,
    pub shape: Cell<u32>,
    /// `object.freeze` flag (SP2 §4). Defaults `false`. A `Cell` (not `RefCell`)
    /// so a `&self` engine can set/read it without a borrow conflict and with no
    /// await-holding-borrow risk; it is `Copy` and adds no GC-traceable edge, so
    /// `Value::trace`/the cycle collector are unaffected.
    pub frozen: Cell<bool>,
}

impl ObjectCell {
    /// Wrap an `IndexMap` into a shared `ObjectCell` with shape `0` (EMPTY_SHAPE),
    /// unfrozen. **Signature unchanged** — all 167 call sites are unaffected.
    pub fn new(map: IndexMap<String, Value>) -> Cc<ObjectCell> {
        Cc::new(ObjectCell {
            storage: RefCell::new(ObjectStorage::Dict(map)),
            shape: Cell::new(0),
            frozen: Cell::new(false),
        })
    }

    /// Build a slab-mode `ObjectCell` with the given canonical key list and values.
    /// The caller (VM opcode or unit test) supplies the registry's `keys_of(shape)`.
    ///
    /// # Panics (debug)
    /// Panics in debug builds if `keys.len() != values.len()`.
    pub fn new_slab(keys: Rc<[Rc<str>]>, values: Vec<Value>, shape: u32) -> Cc<ObjectCell> {
        debug_assert_eq!(
            keys.len(),
            values.len(),
            "ObjectCell::new_slab: keys.len()={} != values.len()={}",
            keys.len(),
            values.len()
        );
        Cc::new(ObjectCell {
            storage: RefCell::new(ObjectStorage::Slab { keys, values }),
            shape: Cell::new(shape),
            frozen: Cell::new(false),
        })
    }

    /// Demote from slab mode to dict mode (one-way, order-preserving).
    /// Materialises `(key, value)` pairs from the slab IN ORDER into a fresh `IndexMap`,
    /// replaces storage with `Dict`, and sets `shape` to `0` (EMPTY_SHAPE).
    /// No-op if already in dict mode.
    pub fn demote_to_dict(&self) {
        self.storage.borrow_mut().demote_to_dict();
        // Shape 0 = EMPTY_SHAPE — ICs will miss forever on this object.
        self.shape.set(0);
    }

    /// VM-only: append a new value under the newly-minted `child_shape`, whose
    /// canonical key list is `child_keys` (caller already called `reg.add_key`).
    ///
    /// Returns `true` on success (we were in slab mode and the append was performed).
    /// Returns `false` if we are not in slab mode (caller should demote then insert).
    ///
    /// # Panics (debug)
    /// Panics if the resulting `values.len()` would not equal `child_keys.len()`.
    pub fn slab_append(&self, child_shape: u32, child_keys: Rc<[Rc<str>]>, v: Value) -> bool {
        let mut storage = self.storage.borrow_mut();
        if let ObjectStorage::Slab { keys, values } = &mut *storage {
            values.push(v);
            *keys = child_keys;
            debug_assert_eq!(
                keys.len(),
                values.len(),
                "slab_append invariant violated: keys={} values={}",
                keys.len(),
                values.len()
            );
            drop(storage);
            self.shape.set(child_shape);
            true
        } else {
            false
        }
    }

    /// Immutable borrow of the dict map (legacy shim for sites that existed before
    /// the accessor API). **Panics in slab mode** — the VM must not call this on a
    /// slab object; only tree-walker / stdlib dict-built paths use it.
    pub fn borrow(&self) -> Ref<'_, IndexMap<String, Value>> {
        Ref::map(self.storage.borrow(), |s| match s {
            ObjectStorage::Dict(m) => m,
            ObjectStorage::Slab { .. } => {
                panic!("ObjectCell::borrow() called on a slab-mode object — use accessors")
            }
        })
    }

    /// Mutable borrow of the dict map (legacy shim). **Panics in slab mode.**
    pub fn borrow_mut(&self) -> RefMut<'_, IndexMap<String, Value>> {
        RefMut::map(self.storage.borrow_mut(), |s| match s {
            ObjectStorage::Dict(m) => m,
            ObjectStorage::Slab { .. } => {
                panic!("ObjectCell::borrow_mut() called on a slab-mode object — use accessors")
            }
        })
    }

    /// Whether this object has been frozen by `object.freeze`.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    /// Mark this object frozen (one-way; idempotent).
    pub fn freeze(&self) {
        self.frozen.set(true);
    }

    // ── SHAPE Task 2.2 — mode-branching accessor bodies ─────────────────────
    // All accessors branch on `Slab` vs `Dict`. Signatures are UNCHANGED from
    // Task 1.1. Dict mode replicates today's behavior exactly (424/0 differential).

    /// `true` if the storage is in slab mode (shape-native). The VM uses this
    /// to decide whether to attempt a registry transition even when shape == 0
    /// (a freshly-built empty object literal is a slab at EMPTY_SHAPE). SHAPE Task 3.1.
    pub fn is_slab(&self) -> bool {
        self.storage.borrow().is_slab()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.storage.borrow().len()
    }

    /// `true` when the object has no entries.
    pub fn is_empty(&self) -> bool {
        self.storage.borrow().is_empty()
    }

    /// Return a clone of the value stored at `key`, or `None` if absent.
    /// (`Value::clone` is an `Rc`-bump, not a deep copy.)
    pub fn get(&self, key: &str) -> Option<Value> {
        self.storage.borrow().get(key)
    }

    /// `true` if `key` is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.storage.borrow().contains_key(key)
    }

    /// Index (insertion-order position) of `key`, or `None`.
    /// Used by the IC warmer to record the slot index at a `GET_PROP` site.
    pub fn get_index_of(&self, key: &str) -> Option<usize> {
        self.storage.borrow().get_index_of(key)
    }

    /// Key + value at insertion-order position `i` (cloned).
    /// Returns `None` when `i >= len()`.
    pub fn get_index(&self, i: usize) -> Option<(Rc<str>, Value)> {
        self.storage.borrow().get_index(i)
    }

    /// Value at insertion-order position `i` — the IC read primitive.
    /// Returns `None` when `i >= len()` (the IC's out-of-range guard).
    pub fn value_at(&self, i: usize) -> Option<Value> {
        self.storage.borrow().value_at(i)
    }

    /// Overwrite the value at existing insertion-order slot `i`.
    /// Returns `false` when `i >= len()`.
    pub fn set_value_at(&self, i: usize, v: Value) -> bool {
        self.storage.borrow_mut().set_value_at(i, v)
    }

    /// Insert or overwrite `key → v`.
    /// **IndexMap semantics:** existing key → position kept, value updated.
    /// New key on Slab (no registry at hand) → demote to dict, then dict insert.
    /// New key on Dict → append.
    pub fn insert(&self, key: &str, v: Value) {
        // A new key on a slab demotes (→ shape 0); keep `self.shape` in sync.
        let was_slab = self.storage.borrow().is_slab();
        let had_key = self.storage.borrow().contains_key(key);
        self.storage.borrow_mut().insert(key, v);
        if was_slab && !had_key {
            self.shape.set(0);
        }
    }

    /// Remove `key` while preserving the relative order of all other entries.
    /// Slab → demote to dict first (sets shape 0, order-preserving), then shift_remove.
    pub fn shift_remove(&self, key: &str) -> Option<Value> {
        let was_slab = self.storage.borrow().is_slab();
        let removed = self.storage.borrow_mut().shift_remove(key);
        if was_slab {
            self.shape.set(0);
        }
        removed
    }

    /// Snapshot all `(key, value)` pairs in insertion order.
    /// Intended for sites that must hold the data across an `.await` point or
    /// that alias the object during iteration.
    pub fn entries(&self) -> Vec<(Rc<str>, Value)> {
        self.storage.borrow().entries()
    }

    /// Call `f(key, value)` for every entry in insertion order.
    /// Zero allocation — `f` receives references, not clones.
    pub fn for_each<F: FnMut(&str, &Value)>(&self, f: F) {
        self.storage.borrow().for_each(f);
    }

    /// Like [`for_each`] but the visitor returns `Result`; iteration stops on the
    /// first `Err` and that error is propagated.
    pub fn try_for_each<E, F: FnMut(&str, &Value) -> Result<(), E>>(
        &self,
        mut f: F,
    ) -> Result<(), E> {
        match &*self.storage.borrow() {
            ObjectStorage::Slab { keys, values } => {
                for (k, v) in keys.iter().zip(values.iter()) {
                    f(k.as_ref(), v)?;
                }
            }
            ObjectStorage::Dict(m) => {
                for (k, v) in m.iter() {
                    f(k.as_str(), v)?;
                }
            }
        }
        Ok(())
    }

    /// Order-insensitive equality — same length AND every key's value is equal.
    /// Works across mode pairs (slab vs dict).
    /// Replicates `IndexMap::eq` (which is itself order-insensitive), used for
    /// the named-enum-payload structural comparison.
    pub fn content_eq(&self, other: &ObjectCell) -> bool {
        let a = self.storage.borrow();
        let b = other.storage.borrow();
        let len_a = match &*a {
            ObjectStorage::Slab { values, .. } => values.len(),
            ObjectStorage::Dict(m) => m.len(),
        };
        let len_b = match &*b {
            ObjectStorage::Slab { values, .. } => values.len(),
            ObjectStorage::Dict(m) => m.len(),
        };
        if len_a != len_b {
            return false;
        }
        // For every key in `a`, check `b` has the same value.
        match &*a {
            ObjectStorage::Slab { keys, values } => {
                for (k, v) in keys.iter().zip(values.iter()) {
                    let bv = match &*b {
                        ObjectStorage::Slab { keys: bkeys, values: bvals } => {
                            bkeys
                                .iter()
                                .position(|bk| bk.as_ref() == k.as_ref())
                                .map(|i| &bvals[i])
                        }
                        ObjectStorage::Dict(m) => m.get(k.as_ref()),
                    };
                    match bv {
                        Some(bv) if bv == v => {}
                        _ => return false,
                    }
                }
            }
            ObjectStorage::Dict(m) => {
                for (k, v) in m.iter() {
                    let bv = match &*b {
                        ObjectStorage::Slab { keys: bkeys, values: bvals } => {
                            bkeys
                                .iter()
                                .position(|bk| bk.as_ref() == k.as_str())
                                .map(|i| &bvals[i])
                        }
                        ObjectStorage::Dict(m2) => m2.get(k.as_str()),
                    };
                    match bv {
                        Some(bv) if bv == v => {}
                        _ => return false,
                    }
                }
            }
        }
        true
    }

    /// Snapshot the insertion-order key list as owned `String`s.
    pub fn keys_snapshot(&self) -> Vec<String> {
        self.storage.borrow().keys_snapshot()
    }

    /// Clone the slab's canonical key list `Rc` (shared per layout) when in slab
    /// mode, else `None` (dict mode). Cloning the `Rc` is a refcount bump, not a
    /// copy — two objects of the same shape return `Rc::ptr_eq`-equal handles.
    /// SHAPE Task 3.2 — used by the per-site cache tests to prove key sharing.
    pub fn slab_keys(&self) -> Option<Rc<[Rc<str>]>> {
        match &*self.storage.borrow() {
            ObjectStorage::Slab { keys, .. } => Some(keys.clone()),
            ObjectStorage::Dict(_) => None,
        }
    }

    /// Clone the whole entry map into a fresh `IndexMap`.
    /// Used by `object_like_fields` in `src/stdlib/object.rs`.
    pub fn to_index_map(&self) -> IndexMap<String, Value> {
        self.storage.borrow().to_index_map()
    }
}

/// GC tracing for `ObjectCell` (V13-T1 / SHAPE Task 2.2 + Task 2.3). Lives here
/// (co-located with the struct) so the private `storage` field is directly accessible.
///
/// # Invariant (§7)
/// - The slab is traced **exactly as the `IndexMap` values were**: every `Value` in
///   `values` is visited; nothing else.
/// - `keys: Rc<[Rc<str>]>` is acyclic immutable string data (the `MapKey` no-op-trace
///   rationale) owned by the `ShapeRegistry` on the `Vm` (a GC root). It holds NO
///   `Value`/`Cc`, so it is **NOT traced**.
/// - `Cell<u32>` (`shape`) and `Cell<bool>` (`frozen`) are scalar, non-edge fields —
///   NOT traced.
/// - Native resource handles never enter object storage, so the native-handle
///   no-trace rule is untouched.
impl Trace for ObjectCell {
    fn trace(&self, tracer: &mut Tracer) {
        // `try_borrow` discipline: skip if already mutably borrowed (mirrors gcmodule's
        // `RefCell` blanket impl — an outstanding borrow implies a live reference, so
        // it is safe to skip; the collector will revisit on the next cycle).
        if let Ok(storage) = self.storage.try_borrow() {
            match &*storage {
                ObjectStorage::Slab { keys: _, values } => {
                    // keys: Rc<[Rc<str>]> — acyclic, no GC edges — NOT traced.
                    // values: Vec<Value> — trace every element (identical to the Dict path).
                    for v in values.iter() {
                        v.trace(tracer);
                    }
                }
                ObjectStorage::Dict(m) => {
                    // Equivalent to gc::trace_index_map (private there; inlined here).
                    // String keys are acyclic; their trace() call is a no-op.
                    for (k, v) in m.iter() {
                        k.trace(tracer);
                        v.trace(tracer);
                    }
                }
            }
        }
    }

    fn is_type_tracked() -> bool {
        true
    }
}

/// The heap payload behind `Value::Array` (SP2 §4 / decision D3). Wraps the
/// element `Vec<Value>` together with an `object.freeze` flag. The wrapper exists
/// ONLY to carry the `frozen` flag beside the vector — exactly mirroring the
/// V11-T2 `ObjectCell` migration — so the `borrow()`/`borrow_mut()` shims keep the
/// vast majority of array access sites textually unchanged. `frozen` is a
/// `Cell<bool>` (`Copy`, no-op `Trace`): it adds no GC-traceable edge, so
/// `Value::trace` is unaffected.
pub struct ArrayCell {
    pub vec: RefCell<Vec<Value>>,
    pub frozen: Cell<bool>,
}

impl ArrayCell {
    /// Wrap a `Vec<Value>` into a shared, `Cc`-managed `ArrayCell` (unfrozen).
    pub fn new(vec: Vec<Value>) -> Cc<ArrayCell> {
        Cc::new(ArrayCell {
            vec: RefCell::new(vec),
            frozen: Cell::new(false),
        })
    }

    /// Immutable borrow of the element vector (drop-in for the old
    /// `Cc<RefCell<Vec<Value>>>`).
    pub fn borrow(&self) -> Ref<'_, Vec<Value>> {
        self.vec.borrow()
    }

    /// Mutable borrow of the element vector (drop-in for the old
    /// `Cc<RefCell<Vec<Value>>>`).
    pub fn borrow_mut(&self) -> RefMut<'_, Vec<Value>> {
        self.vec.borrow_mut()
    }

    /// Whether this array has been frozen by `object.freeze`.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    /// Mark this array frozen (one-way; idempotent).
    pub fn freeze(&self) {
        self.frozen.set(true);
    }
}

/// The heap payload behind `Value::Map`. A thin newtype around the entry
/// `RefCell<IndexMap<…>>` whose only purpose is to carry a hand-written
/// [`gcmodule::Trace`] impl: `IndexMap` is a foreign type, so we cannot give it
/// (nor `RefCell<IndexMap>`) a blanket `Trace` impl (orphan rule). Wrapping it in
/// this local newtype lets `Cc<MapCell>` satisfy `T: Trace` while the cycle
/// collector still reaches the contained `Value`s. `Deref`s to the inner
/// `RefCell` so every `m.borrow()`/`m.borrow_mut()` access site is unchanged.
pub struct MapCell {
    pub map: RefCell<IndexMap<MapKey, Value>>,
    /// `object.freeze` flag (SP2 §4). Defaults `false`. See [`ObjectCell::frozen`].
    pub frozen: Cell<bool>,
}

impl MapCell {
    /// Wrap an `IndexMap` into a shared, `Cc`-managed `MapCell` (unfrozen).
    pub fn new(map: IndexMap<MapKey, Value>) -> Cc<MapCell> {
        Cc::new(MapCell {
            map: RefCell::new(map),
            frozen: Cell::new(false),
        })
    }

    /// Whether this map has been frozen by `object.freeze`.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    /// Mark this map frozen (one-way; idempotent).
    pub fn freeze(&self) {
        self.frozen.set(true);
    }
}

impl std::ops::Deref for MapCell {
    type Target = RefCell<IndexMap<MapKey, Value>>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

/// The heap payload behind `Value::Set`. See [`MapCell`] — same story, a local
/// newtype over `RefCell<IndexSet<…>>` so it can carry a `Trace` impl (foreign
/// `IndexSet` cannot) and `Cc<SetCell>` satisfies `T: Trace`.
pub struct SetCell {
    pub set: RefCell<IndexSet<MapKey>>,
    /// `object.freeze` flag (SP2 §4). Defaults `false`. See [`ObjectCell::frozen`].
    pub frozen: Cell<bool>,
}

impl SetCell {
    /// Wrap an `IndexSet` into a shared, `Cc`-managed `SetCell` (unfrozen).
    pub fn new(set: IndexSet<MapKey>) -> Cc<SetCell> {
        Cc::new(SetCell {
            set: RefCell::new(set),
            frozen: Cell::new(false),
        })
    }

    /// Whether this set has been frozen by `object.freeze`.
    pub fn is_frozen(&self) -> bool {
        self.frozen.get()
    }

    /// Mark this set frozen (one-way; idempotent).
    pub fn freeze(&self) {
        self.frozen.set(true);
    }
}

impl std::ops::Deref for SetCell {
    type Target = RefCell<IndexSet<MapKey>>;
    fn deref(&self) -> &Self::Target {
        &self.set
    }
}

/// A hashable map key. Maps key on `nil`/`bool`/`number`/`decimal`/`string`
/// (spec §11.2 + decimal extension). Number and Decimal are distinct key kinds.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Nil,
    Bool(bool),
    /// Exact integer key (NUM §3.3). An integral, finite, in-i64-range `Float`
    /// FOLDS into this variant so `Int(1)` and `Float(1.0)` are the SAME key.
    Int(i64),
    Num(u64), // canonicalized f64 bits (−0.0→+0.0, all NaNs→one canonical NaN)
    Str(Rc<str>),
    /// Exact decimal key. Distinct from `Num`/`Int` — `Decimal("0.1")` ≠ `Num(0.1f64)`,
    /// `Decimal("1")` ≠ `Int(1)` (Decimal is exact and opt-in; never folded).
    Decimal(Decimal),
}

impl MapKey {
    /// Convert a value to a key, or `None` if its kind is not hashable.
    pub fn from_value(v: &Value) -> Option<MapKey> {
        match v {
            Value::Nil => Some(MapKey::Nil),
            Value::Bool(b) => Some(MapKey::Bool(*b)),
            Value::Int(i) => Some(MapKey::Int(*i)),
            Value::Float(n) => {
                // NUM §3.3: an integral, finite, in-i64-range float folds to the same
                // key as the equal `int` (so `map[1]` and `map[1.0]` collide). Every
                // other float (fractional, ±inf, NaN) keeps its canonical-bits key —
                // NaN stays a single canonical bit pattern (storable, but never equal
                // to a non-NaN under the evaluator's `==`).
                if n.fract() == 0.0
                    && n.is_finite()
                    && *n >= i64::MIN as f64
                    // STRICT upper bound: `i64::MAX as f64` rounds UP to 2^63 (out of
                    // i64 range), so `<=` would admit 2^63 and `as i64` would saturate
                    // to i64::MAX — wrongly colliding 2^63 with `int` i64::MAX as a key.
                    // `-(i64::MIN as f64)` is exactly 2^63; `<` excludes it (no i64 ≥ 2^63).
                    && *n < -(i64::MIN as f64)
                {
                    Some(MapKey::Int(*n as i64))
                } else {
                    // Only fractional or non-finite floats reach here (±0.0 folded to
                    // `Int(0)` above). NaN canonicalizes to one bit pattern.
                    let canon = if n.is_nan() {
                        f64::NAN.to_bits()
                    } else {
                        n.to_bits()
                    };
                    Some(MapKey::Num(canon))
                }
            }
            Value::Str(s) => Some(MapKey::Str(s.clone())),
            // VAL Task 2: `MapKey::Decimal` still folds BY VALUE (the inner
            // `Decimal`), so exact key equality is preserved — `**d` decodes the
            // boxed value out of the `Rc` (the box is invisible to keying).
            Value::Decimal(d) => Some(MapKey::Decimal(**d)),
            _ => None,
        }
    }

    /// Recover the value form of a key (for `keys`/`entries`/display/contracts).
    pub fn to_value(&self) -> Value {
        match self {
            MapKey::Nil => Value::Nil,
            MapKey::Bool(b) => Value::Bool(*b),
            MapKey::Int(i) => Value::Int(*i),
            MapKey::Num(bits) => Value::Float(f64::from_bits(*bits)),
            MapKey::Str(s) => Value::Str(s.clone()),
            MapKey::Decimal(d) => Value::Decimal(Rc::new(*d)),
        }
    }
}

/// `object.freeze` (SP2 §4): if `v` is a FROZEN mutable container, return the
/// kind name for the panic message (`"array"|"object"|"map"|"set"|"instance"`);
/// otherwise `None`. Non-frozen containers and all non-container values are
/// `None` (mutation of an unfrozen container is allowed; non-containers are never
/// frozen). Used by `check_not_frozen` at every mutation site on both engines.
pub fn frozen_kind(v: &Value) -> Option<&'static str> {
    match v {
        Value::Array(a) if a.is_frozen() => Some("array"),
        Value::Object(o) if o.is_frozen() => Some("object"),
        Value::Map(m) if m.is_frozen() => Some("map"),
        Value::Set(s) if s.is_frozen() => Some("set"),
        Value::Instance(i) if i.borrow().frozen.get() => Some("instance"),
        // SRV §3.8: a frozen `Shared` reports its underlying CONTAINER kind so the
        // shipped `cannot mutate a frozen {kind}` message applies (a frozen-shared
        // object → "object", array → "array", …). A frozen scalar / regex /
        // enum-variant is not a mutable container → `None`.
        Value::Shared(n) => n.mutable_container_kind(),
        _ => None,
    }
}

/// `object.freeze` (SP2 §4): shallow-freeze a mutable container in place. A no-op
/// for any non-container value (JS `Object.freeze` ergonomics). Idempotent /
/// one-way (no unfreeze). The caller returns `v` unchanged for chaining.
pub fn freeze_value(v: &Value) {
    match v {
        Value::Array(a) => a.freeze(),
        Value::Object(o) => o.freeze(),
        Value::Map(m) => m.freeze(),
        Value::Set(s) => s.freeze(),
        Value::Instance(i) => i.borrow().frozen.set(true),
        _ => {}
    }
}

/// `object.isFrozen` (SP2 §4): whether `v` is a frozen container. `false` for any
/// non-container value.
pub fn is_frozen_value(v: &Value) -> bool {
    match v {
        Value::Array(a) => a.is_frozen(),
        Value::Object(o) => o.is_frozen(),
        Value::Map(m) => m.is_frozen(),
        Value::Set(s) => s.is_frozen(),
        Value::Instance(i) => i.borrow().frozen.get(),
        // SRV §3.5: a `Shared` is frozen by construction.
        Value::Shared(_) => true,
        _ => false,
    }
}

pub struct EnumDef {
    pub name: String,
    pub variants: IndexMap<String, Value>, // each is a Value::EnumVariant
    /// ADT: per-variant payload schema (field names + declared types). A unit /
    /// scalar-backed variant has an EMPTY `VariantSchema.fields`; a payload variant
    /// (positional or named) carries its declared field list. The full ordered
    /// variant list is `variants.keys()` (== `variant_schemas.keys()`).
    pub variant_schemas: IndexMap<String, VariantSchema>,
}

/// ADT §5.1: the declared payload schema of one enum variant. An empty `fields`
/// vector means a unit / scalar-backed variant (no payload). A field's `name` is
/// `Some` for a named-field variant (`Circle(radius: float)`), `None` for a
/// positional one (`Pair(int, int)`). Field types use the NUM model.
#[derive(Clone)]
pub struct VariantSchema {
    pub fields: Vec<(Option<Rc<str>>, crate::ast::Type)>,
}

impl VariantSchema {
    /// A payload (non-unit) variant has at least one declared field.
    pub fn has_payload(&self) -> bool {
        !self.fields.is_empty()
    }

    /// `true` iff the fields are named (`Circle(radius: float)`). An empty schema
    /// (unit) is considered positional/none. Uniformity is guaranteed at parse time
    /// (all-named XOR all-positional), so checking the first field suffices.
    pub fn is_named(&self) -> bool {
        self.fields.first().map(|(n, _)| n.is_some()).unwrap_or(false)
    }
}

pub struct EnumVariant {
    pub enum_name: String,
    pub name: String,
    pub value: Value, // backing scalar (unit/scalar-backed variant), or Nil
    /// ADT §5.1: `None` for a unit variant OR an unsaturated constructor; `Some`
    /// for a CONSTRUCTED payload variant. The cycle-capable part of the value lives
    /// here (a recursive enum payload can form a cycle), so `Trace` reaches it.
    pub payload: Option<Payload>,
    /// ADT §5.1: `true` iff this is an unsaturated payload-variant CONSTRUCTOR
    /// (`Shape.Circle` referenced but not yet called). Calling it validates the
    /// payload and yields a constructed variant (`payload: Some, ctor: false`).
    pub ctor: bool,
    /// ADT: a back-reference to the owning `EnumDef`, populated ONLY on a constructor
    /// value RETURNED to user code (so a first-class `let mk = Shape.Circle` can
    /// validate the payload when called). The INTERNED map entry has `def: None`, so
    /// `EnumDef → variants → (interned ctor)` never forms an `Rc` cycle. A unit /
    /// constructed variant also has `def: None`. The constructor stays cheap (one
    /// extra `Rc` clone, only on the constructor read path).
    pub def: Option<Rc<EnumDef>>,
}

/// ADT §5.1: a constructed variant's payload data. The cycle-capable containers are
/// held behind a `Cc` (the cycle collector ONLY tracks `Cc` nodes — gcmodule's
/// `Rc<T>: Trace` is acyclic/no-op, so the `Rc<EnumVariant>` wrapper can never be a
/// cycle node; the payload's `Cc<ArrayCell>`/`Cc<ObjectCell>` IS). Positional reuses
/// `ArrayCell` (so `.value` returns a stable `Value::Array` handle — ADT §3.4);
/// named reuses `ObjectCell` (field-access sugar + stable `.value` Object share one
/// representation). Both are traced by the collector exactly as a free Array/Object.
pub enum Payload {
    Positional(Cc<ArrayCell>),
    Named(Cc<ObjectCell>),
}

pub struct Method {
    pub params: Vec<crate::ast::Param>,
    pub ret: Option<crate::ast::Type>,
    pub body: Vec<Stmt>,
    pub is_async: bool,
    pub is_generator: bool,
    /// `worker fn` / `static worker fn` — Spec A: dispatched to a pooled isolate,
    /// returns `future<T>`. Tree-walker reads this on the static-method call path.
    pub is_worker: bool,
}

#[derive(Clone)]
pub struct FieldSchema {
    pub ty: crate::ast::Type,
    pub default: Option<crate::ast::Expr>,
}

pub struct Class {
    pub name: String,
    pub superclass: Option<Rc<Class>>,
    pub fields: IndexMap<String, FieldSchema>,
    pub methods: IndexMap<String, Rc<Method>>,
    /// `static fn` / `static async fn` / `static fn*` members (SP1 §3). A SEPARATE
    /// namespace from instance `methods` — an instance method and a static method
    /// may share a name (`c.x()` vs `C.x()`). Called as `C.name(args)` with no
    /// receiver; inherited up the superclass chain like instance methods.
    pub static_methods: IndexMap<String, Rc<Method>>,
    pub def_env: Environment,
    /// Workers Spec B: this class was declared `worker class`. A `worker class` is
    /// spawned into a dedicated isolate via `ClassName.spawn(args)` (returns
    /// `future<handle>`); a bare `ClassName(args)` still builds a LOCAL instance.
    /// Set from the AST/CST `is_worker` flag on both engines.
    pub is_worker: bool,
}

pub struct Instance {
    pub class: Rc<Class>,
    /// The instance's fields, in shape-native [`ObjectStorage`] (SHAPE Task 3.4).
    /// The VM builds a `Slab` via precise registry transitions (mirroring objects);
    /// the tree-walker / `.from` / worker-airlock-rebuild build a `Dict` (shape 0).
    /// Accessed via the `Instance` accessor methods, NEVER as a raw map.
    pub fields: ObjectStorage,
    /// The instance's key-layout id (V11-T2 hidden classes). Defaults to `0`
    /// (unset); the tree-walker leaves it at `0`, the VM assigns the class's base
    /// shape (and transitions it if a field is added). `Cell` so a `&self` VM
    /// method can update it without a mutable instance borrow.
    pub shape_id: Cell<u32>,
    /// `object.freeze` flag (SP2 §4). Defaults `false`. `Cell` so a `&self`
    /// engine can set/read it without a mutable instance borrow; see
    /// [`ObjectCell::frozen`].
    pub frozen: Cell<bool>,
}

impl Instance {
    /// Build a `Dict`-mode instance (shape 0) from an `IndexMap` of fields. Used by
    /// the tree-walker `construct`, `validate_into` (`.from`), worker deserialize,
    /// and `object.deep_clone` — every NON-VM construction path. SHAPE Task 3.4.
    pub fn from_dict(class: Rc<Class>, fields: IndexMap<String, Value>) -> Instance {
        Instance {
            class,
            fields: ObjectStorage::Dict(fields),
            shape_id: Cell::new(0),
            frozen: Cell::new(false),
        }
    }

    /// Build an EMPTY `Slab`-mode instance at EMPTY_SHAPE (shape 0). The VM grows it
    /// in declared-field order via precise registry transitions. SHAPE Task 3.4.
    pub fn new_empty_slab(class: Rc<Class>) -> Instance {
        Instance {
            class,
            fields: ObjectStorage::Slab {
                keys: Rc::from([]),
                values: Vec::new(),
            },
            shape_id: Cell::new(0),
            frozen: Cell::new(false),
        }
    }

    // ── Field accessors (mirror `ObjectCell`'s; delegate to the shared
    //    `ObjectStorage` free-function bodies). SHAPE Task 3.4. ──────────────────

    /// Number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// `true` when the instance has no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// `true` if the field storage is in slab mode.
    pub fn is_slab(&self) -> bool {
        self.fields.is_slab()
    }

    /// Clone of the value stored under `key`, or `None`.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.fields.get(key)
    }

    /// `true` if `key` is a field on this instance.
    pub fn contains_key(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }

    /// Insertion-order position of `key`, or `None` (the field-IC warmer).
    pub fn get_index_of(&self, key: &str) -> Option<usize> {
        self.fields.get_index_of(key)
    }

    /// Key + value at insertion-order position `i` (cloned), or `None`.
    pub fn get_index(&self, i: usize) -> Option<(Rc<str>, Value)> {
        self.fields.get_index(i)
    }

    /// Value at insertion-order position `i` (cloned) — the field-IC read primitive.
    pub fn value_at(&self, i: usize) -> Option<Value> {
        self.fields.value_at(i)
    }

    /// Overwrite the value at existing slot `i`. Returns `false` if `i >= len()`.
    /// Requires `&mut self` (the `Instance` is behind a `RefCell`).
    pub fn set_value_at(&mut self, i: usize, v: Value) -> bool {
        self.fields.set_value_at(i, v)
    }

    /// Insert or overwrite `key → v` (IndexMap semantics). A NEW key on a slab (no
    /// registry at hand) demotes to dict and resets `shape_id` to 0; slab
    /// transitions are the Vm's job (`vm_instance_insert`). Existing-key writes keep
    /// the slot, shape unchanged.
    pub fn insert(&mut self, key: &str, v: Value) {
        let was_slab = self.fields.is_slab();
        let had_key = self.fields.contains_key(key);
        self.fields.insert(key, v);
        if was_slab && !had_key {
            self.shape_id.set(0);
        }
    }

    /// VM-only: append a new value under the newly-minted `child_shape` whose
    /// canonical key list is `child_keys` (the caller already called
    /// `reg.add_key`). Returns `true` on success (we were in slab mode); `false`
    /// otherwise (caller should demote then `insert`). Mirrors
    /// [`ObjectCell::slab_append`].
    pub fn slab_append(&mut self, child_shape: u32, child_keys: Rc<[Rc<str>]>, v: Value) -> bool {
        if let ObjectStorage::Slab { keys, values } = &mut self.fields {
            values.push(v);
            *keys = child_keys;
            debug_assert_eq!(
                keys.len(),
                values.len(),
                "Instance::slab_append invariant violated: keys={} values={}",
                keys.len(),
                values.len()
            );
            self.shape_id.set(child_shape);
            true
        } else {
            false
        }
    }

    /// Demote the field storage to dict mode (order-preserving) and reset the shape
    /// to 0. No-op if already dict. Mirrors [`ObjectCell::demote_to_dict`].
    pub fn demote_to_dict(&mut self) {
        self.fields.demote_to_dict();
        self.shape_id.set(0);
    }

    /// Snapshot all `(key, value)` pairs in insertion order.
    pub fn entries(&self) -> Vec<(Rc<str>, Value)> {
        self.fields.entries()
    }

    /// Call `f(key, value)` for every field in insertion order (no allocation).
    pub fn for_each<F: FnMut(&str, &Value)>(&self, f: F) {
        self.fields.for_each(f);
    }

    /// Snapshot the insertion-order key list as owned `String`s.
    pub fn keys_snapshot(&self) -> Vec<String> {
        self.fields.keys_snapshot()
    }

    /// Clone the whole field map into a fresh `IndexMap` (insertion order).
    pub fn to_index_map(&self) -> IndexMap<String, Value> {
        self.fields.to_index_map()
    }
}

pub struct BoundMethod {
    pub receiver: Value,
    pub method: Rc<Method>,
    pub defining_class: Rc<Class>,
    pub name: String,
}

pub struct SuperRef {
    pub receiver: Value,
    pub start: Option<Rc<Class>>,
}

/// IFACE §4: a structural interface — an immutable, acyclic conformance descriptor
/// naming a method set. An interface name resolves to a `Value::Interface(Rc<InterfaceDef>)`.
/// It is never a receiver, has no vtable, holds no `Value`/`Cc` edges, and its GC
/// `Trace` is a no-op (like `Regex`/`Native`). Identity-equal (`Rc::ptr_eq`).
pub struct InterfaceDef {
    pub name: String,
    /// This interface's OWN requirements (the body's `fn` signatures), keyed by name.
    pub own_methods: IndexMap<String, MethodReq>,
    /// The names of the interfaces this one `extends` (composition). Stored as NAMES,
    /// resolved LAZILY (interfaces forward-reference as late-bound module-globals) —
    /// NOT pre-flattened at declaration time (IFACE §4, C4).
    pub extends: Vec<String>,
    /// The module `Environment` this interface was declared in (mirrors `Class.def_env`).
    /// The lazy `flatten` resolves each `extends` NAME through it — late-bound, so a
    /// forward-referenced `extends B` resolves once `B` is defined. Cheap `Rc` clone;
    /// holds no cycle-capable `Value` the GC must trace into the descriptor.
    pub def_env: Environment,
    /// MEMOIZED flattened method set (own + every transitively-extended interface's),
    /// deduplicated by name. `None` until the first `conforms`/contract check; filled
    /// on first use via the engine's `flatten()` lazy builder, then reused. Never
    /// invalidated within a run (descriptors are load-time-immortal, IFACE §5.3).
    pub flat: RefCell<Option<Rc<IndexMap<String, MethodReq>>>>,
}

/// IFACE §4: a single required method on an interface — name keys it in the map, this
/// carries the call-shape. v1 is arity-only (type-erased, runtime-permissive); TYPE
/// later adds param/ret `CheckTy` slots here for the strict static check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodReq {
    /// The number of parameters the interface signature declares.
    pub arity: usize,
    /// Whether the requirement itself declares a rest param (`...xs`) — only then must
    /// the conforming method also be variadic (IFACE §5.1).
    pub has_rest: bool,
    // TYPE later adds param/ret CheckTy signatures here.
}

/// A compiled regular expression (spec §11.2). Immutable; identity equality.
/// Gated on the `data` feature because `regex::Regex` only exists with it.
#[cfg(feature = "data")]
pub struct RegexHandle {
    pub re: regex::Regex,
    pub source: String,
}

/// A native resource handle (sqlite connection/statement, process child/reader/writer,
/// and — in M14 — http bodies/sse/sockets). The non-Clone OS resource lives in the
/// interp's `resources` table keyed by `id`; this value is a cheap clonable handle.
pub struct NativeObject {
    pub id: u64,
    pub kind: NativeKind,
    /// Plain readable fields (e.g. a child's `pid`); methods are resolved separately.
    pub fields: indexmap::IndexMap<String, Value>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // Some variants are only constructed by feature-gated modules (sqlite/process).
pub enum NativeKind {
    SqliteConnection,
    SqliteStatement,
    ChildProcess,
    Reader,
    Writer,
    // M14 networking handles (registered only under feature `net`).
    TcpListener,
    TcpStream,
    HttpResponse,
    // A streaming HTTP response body reader (`resp.body` when `opts.stream:true`).
    // Follows the §11.4 reader idiom over a chunked byte stream.
    HttpBody,
    // A cancellation token for in-flight HTTP requests (`http.cancelToken()`).
    CancelHandle,
    // A first-class Server-Sent Events client stream (`http.sse(url, opts?)`).
    // `next()` yields parsed `{event,data,id,retry}` events; `lastEventId` is a
    // readable property; auto-reconnects on disconnect (see std/net/http).
    SseStream,
    // M14 std/http/server: a server handle holding registered routes + middleware
    // and (after `bind`) the live `TcpListener`. Methods: route/use/bind/serve/listen.
    HttpServer,
    // M14 std/http/server: the `next` callable handed to a middleware. Calling it
    // (as a `NativeMethod`) advances the middleware chain → matched route handler.
    HttpNext,
    // M14 std/net/ws: a connected WebSocket (client `connect` or server `accept`).
    // Methods: send/recv/close. Unifies the client/server stream types behind one
    // boxed Sink+Stream of `Message` (see net_ws::WsConnState).
    WsConnection,
    // M14 std/net/ws: an accept-based WebSocket server listener (binds a TcpListener;
    // `accept()` performs the handshake → WsConnection). Carries a `port` field.
    WsListener,
    // M15 std/tui: a terminal handle owning the back/flushed screen buffers, the
    // cursor position, and the active raw/alt-screen flags. Methods: size/clear/
    // moveCursor/enterRaw/leaveRaw/enterAltScreen/leaveAltScreen/showCursor/draw
    // (setCell/text/hline/vline/box/fill)/flush/pollEvent/readEvent/restore/close.
    // Registered only under feature `tui`.
    Terminal,
    // std/sync: a FIFO channel (VecDeque + Rc<Notify>). Not feature-gated.
    Channel,
    // std/sync: a counting semaphore (RefCell<usize> + Rc<Notify>). Not feature-gated.
    Semaphore,
    // std/time: a repeating timer handle. `.tick()` awaits the next tick.
    // Not feature-gated (tokio timers are always available).
    Interval,
    // std/time: a debounce wrapper (trailing-edge). Callable as `wrapper(args)`.
    DebounceWrapper,
    // std/time: a throttle wrapper (leading-edge). Callable as `wrapper(args)`.
    ThrottleWrapper,
    // std/sync: a token-bucket rate limiter. `.acquire()` awaits a token; the
    // bucket refills `count` tokens every `window_ms` milliseconds (monotonic
    // clock — no background task). Not feature-gated.
    RateLimiter,
    // std/net/udp: a bound UDP socket. Methods: send/recv/localAddr/close.
    // Registered only under feature `net`.
    UdpSocket,
    // std/stream: a lazy pull-based stream (a source + a chain of combinator
    // stages). Driven by terminals via `Interp::pull_next`. Not feature-gated.
    Stream,
    // SP5 §6 std/postgres: an async Postgres connection (feature `postgres`).
    // Methods: query/queryOne/exec/begin/commit/rollback/close.
    PostgresConnection,
    // SP5 §6 std/redis: an async Redis connection (feature `redis`).
    // Methods: command/get/set/del/incr/expire/exists/close.
    RedisConnection,
    // SP5 §7 std/lru: a bounded LRU cache (core). Methods: get/set/has/delete/
    // clear/len/keys.
    Lru,
    // SP5 §7 std/events: an event-emitter (core). Methods: on/once/off/emit/
    // listenerCount.
    Events,
    // SP12 std/telemetry: a tracing span handle. Methods: setAttribute/addEvent/
    // setStatus/end. Inert (no-op) before telemetry.init. Feature `telemetry`.
    #[cfg(feature = "telemetry")]
    TelemetrySpan,
    // SP12 std/telemetry: a metric instrument handle (counter/histogram/gauge).
    // Methods: add (counter), record (histogram), set (gauge). Feature `telemetry`.
    #[cfg(feature = "telemetry")]
    TelemetryInstrument,
    // SP12 std/telemetry: an INERT handle returned when telemetry is not
    // initialized — every method is a no-op. Feature `telemetry`.
    #[cfg(feature = "telemetry")]
    TelemetryNoop,
    // SP11 std/ai: a provider handle (`ai.provider(kind, config)`). Pure config in
    // `fields` (kind/baseUrl/apiKey/apiVersion/headers) — no OS resource. Method:
    // `.model(id)` → an AiModel handle. Feature `ai`.
    #[cfg(feature = "ai")]
    AiProvider,
    // SP11 std/ai: a model handle (`provider.model(id)`). Carries the resolved
    // provider config + model name in `fields`; consumed by ai.generate/stream/embed
    // as the `model:` argument. Feature `ai`.
    #[cfg(feature = "ai")]
    AiModel,
    // SP11 std/ai: a streaming chat handle (`ai.stream(...)`). Backed by an
    // `AiStream` resource; methods `next()`/`textOnly()`/`result()`, consumable by
    // `for await`. Feature `ai`.
    #[cfg(feature = "ai")]
    AiStream,
    // SP11 std/ai: a text-only streaming adapter (`stream.textOnly()`), yielding bare
    // text strings; shares the underlying `AiStream` resource. Feature `ai`.
    #[cfg(feature = "ai")]
    AiTextStream,
    // SP11 std/ai: a tool definition (`ai.tool({description, input, execute})`).
    // Carries description/input-schema/execute fn in `fields`; consumed by
    // ai.generate's `tools:` map. Feature `ai`.
    #[cfg(feature = "ai")]
    AiTool,
    // Workers Spec B §Task 5: a `worker class` ACTOR proxy handle. The actor
    // instance lives in a dedicated isolate; this handle's method calls become FIFO
    // mailbox messages over a `Send` channel. Backed by `ResourceState::WorkerActor`
    // (the outbound sender + the `IsolateHandle`, whose `Drop` tears the isolate
    // down). Not feature-gated — `worker` is core syntax. Readable field: the
    // declared class `name`.
    WorkerActor,
    // FFI campaign §3.4: an open shared library (`ffi.open` → `dlopen`). Backs a
    // `ResourceState::ForeignLib(libloading::Library)`; its `Drop` `dlclose`s
    // deterministically. Method: `.symbol`. GC-UNTRACED (a `Library` is an opaque OS
    // handle the collector cannot reason about — reclaimed only by `Drop`). The
    // variant name stays un-gated (kept in every exhaustive `NativeKind` match) even
    // when the `ffi` feature is off, so matches compile in both configs; only the
    // backing `ResourceState` body references `libloading`.
    ForeignLib,
    // FFI campaign §3.4: a `dlsym`'d symbol + its bound signature (argtypes/rettype +
    // the libffi CIF). Method: `.call`. Stores the resolved function address as a raw
    // `*mut c_void` and KEEPS THE OWNING `Library` ALIVE (a borrowed `Symbol<'lib>`
    // cannot be `'static`). GC-UNTRACED.
    ForeignSymbol,
    // FFI campaign §3.4: an opaque C pointer returned by a call (a `malloc` result, a
    // C "constructor" handle). Carries the raw `usize` address; passed back as a
    // `ffi.ptr`. NOT auto-freed (ownership is the C library's contract). GC-UNTRACED.
    ForeignPtr,
}

impl NativeKind {
    pub fn type_name(self) -> &'static str {
        match self {
            NativeKind::SqliteConnection => "connection",
            NativeKind::SqliteStatement => "statement",
            NativeKind::ChildProcess => "childProcess",
            NativeKind::Reader => "reader",
            NativeKind::Writer => "writer",
            NativeKind::TcpListener => "tcpListener",
            NativeKind::TcpStream => "tcpStream",
            NativeKind::HttpResponse => "httpResponse",
            NativeKind::HttpBody => "httpBody",
            NativeKind::CancelHandle => "cancelHandle",
            NativeKind::SseStream => "sseStream",
            NativeKind::HttpServer => "httpServer",
            NativeKind::HttpNext => "httpNext",
            NativeKind::WsConnection => "wsConnection",
            NativeKind::WsListener => "wsListener",
            NativeKind::Terminal => "terminal",
            NativeKind::Channel => "channel",
            NativeKind::Semaphore => "semaphore",
            NativeKind::Interval => "interval",
            NativeKind::DebounceWrapper => "debounce",
            NativeKind::ThrottleWrapper => "throttle",
            NativeKind::RateLimiter => "rateLimiter",
            NativeKind::UdpSocket => "udpSocket",
            NativeKind::Stream => "stream",
            NativeKind::PostgresConnection => "postgresConnection",
            NativeKind::RedisConnection => "redisConnection",
            NativeKind::Lru => "lru",
            NativeKind::Events => "emitter",
            #[cfg(feature = "telemetry")]
            NativeKind::TelemetrySpan => "span",
            #[cfg(feature = "telemetry")]
            NativeKind::TelemetryInstrument => "instrument",
            #[cfg(feature = "telemetry")]
            NativeKind::TelemetryNoop => "telemetryNoop",
            #[cfg(feature = "ai")]
            NativeKind::AiProvider => "aiProvider",
            #[cfg(feature = "ai")]
            NativeKind::AiModel => "aiModel",
            #[cfg(feature = "ai")]
            NativeKind::AiStream => "aiStream",
            #[cfg(feature = "ai")]
            NativeKind::AiTextStream => "aiTextStream",
            #[cfg(feature = "ai")]
            NativeKind::AiTool => "aiTool",
            NativeKind::WorkerActor => "workerActor",
            NativeKind::ForeignLib => "foreignLib",
            NativeKind::ForeignSymbol => "foreignSymbol",
            NativeKind::ForeignPtr => "foreignPtr",
        }
    }

    /// FFI §4 (BLOCKER 3): the capability that governs OPERATING an already-open
    /// handle of this kind, or `None` for a pure in-memory native that touches no OS
    /// resource. Consulted at the top of `Interp::call_native_method` so that
    /// dropping a capability HOLDS for handles opened before the drop (e.g.
    /// `socket.read()` / `listener.accept()` are denied after `caps.drop("net")`).
    ///
    /// - `Net`: every networking handle (TCP, UDP, HTTP body/server/response/SSE,
    ///   WebSocket, HTTP cancel/next) plus the network DB connections (postgres/redis).
    /// - `Process`: a child process and its stdio reader/writer.
    /// - `Fs`: a sqlite connection/statement (an open DB FILE handle).
    /// - `None`: pure in-memory natives (channel, semaphore, timers, rate limiter,
    ///   stream, lru, events, telemetry, ai-config, worker actor, terminal) — they
    ///   acquire no OS effect at method time, so gating them would over-deny.
    pub fn governing_cap(self) -> Option<crate::stdlib::caps::Cap> {
        use crate::stdlib::caps::Cap;
        match self {
            // Networking handles — operating them is live network I/O.
            NativeKind::TcpListener
            | NativeKind::TcpStream
            | NativeKind::HttpResponse
            | NativeKind::HttpBody
            | NativeKind::SseStream
            | NativeKind::HttpServer
            | NativeKind::WsConnection
            | NativeKind::WsListener
            | NativeKind::UdpSocket => Some(Cap::Net),
            NativeKind::PostgresConnection | NativeKind::RedisConnection => Some(Cap::Net),
            // A cancel token (`http.cancelToken()`) is a pure in-memory `Notify` —
            // cancelling acquires no network, so gating it would over-deny a cleanup
            // (you typically cancel an in-flight request even after dropping `net`).
            // The HTTP-next middleware advancer drives already-accepted server work;
            // the accept itself (HttpServer) is gated. Both stay ungated to avoid
            // over-deny on the default path's request-lifecycle handles.
            NativeKind::CancelHandle | NativeKind::HttpNext => None,
            // A child process + its stdio: operating them is subprocess control.
            NativeKind::ChildProcess | NativeKind::Reader | NativeKind::Writer => {
                Some(Cap::Process)
            }
            // An open DB file handle (sqlite): operating it is filesystem I/O.
            NativeKind::SqliteConnection | NativeKind::SqliteStatement => Some(Cap::Fs),
            // FFI §4 (BLOCKER 3): operating an OPEN foreign handle — `lib.symbol`,
            // `sym.call`, reading a `ForeignPtr` — is a native-call effect governed by
            // `ffi`. So `caps.drop("ffi")` HOLDS for libs/symbols opened before the
            // drop: a `lib.symbol`/`sym.call` after the drop is denied here, not just
            // the initial `ffi.open` at the dispatch gate.
            NativeKind::ForeignLib | NativeKind::ForeignSymbol | NativeKind::ForeignPtr => {
                Some(Cap::Ffi)
            }
            // Pure in-memory natives — no OS effect at method time → ungated.
            NativeKind::Terminal
            | NativeKind::Channel
            | NativeKind::Semaphore
            | NativeKind::Interval
            | NativeKind::DebounceWrapper
            | NativeKind::ThrottleWrapper
            | NativeKind::RateLimiter
            | NativeKind::Stream
            | NativeKind::Lru
            | NativeKind::Events
            | NativeKind::WorkerActor => None,
            // Telemetry spans/instruments BUFFER in memory; the only network egress is the
            // module-level `telemetry.flush`/`capture`/`init` exporters (gated at the
            // dispatch root → `Cap::Net`); a no-op span does nothing. So operating a
            // telemetry handle acquires no OS resource → ungated (over-gating a no-op span
            // would break defensive telemetry use).
            #[cfg(feature = "telemetry")]
            NativeKind::TelemetrySpan
            | NativeKind::TelemetryInstrument
            | NativeKind::TelemetryNoop => None,
            // An OPEN AI stream reads completions FROM THE NETWORK on each `.next()`
            // (`exec_chat_stream`), so operating one after `caps.drop("net")` must be
            // denied — the per-handle re-check that makes the drop HOLD (mirrors
            // TcpStream/HttpBody). `AiProvider`/`AiModel`/`AiTool` are pure config / tool
            // definitions with no network-doing handle methods (the network is the
            // module-level `ai.generate`/`ai.stream`, gated at the dispatch root).
            #[cfg(feature = "ai")]
            NativeKind::AiStream | NativeKind::AiTextStream => Some(Cap::Net),
            #[cfg(feature = "ai")]
            NativeKind::AiProvider | NativeKind::AiModel | NativeKind::AiTool => None,
        }
    }
}

/// A method bound to a native handle (e.g. `child.wait`), dispatched async.
pub struct NativeMethod {
    pub receiver: std::rc::Rc<NativeObject>,
    pub method: String,
}

/// Walk a class chain for a method, returning it plus the class that defined it.
pub fn find_method(class: &Rc<Class>, name: &str) -> Option<(Rc<Method>, Rc<Class>)> {
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        if let Some(m) = c.methods.get(name) {
            return Some((m.clone(), c.clone()));
        }
        cur = c.superclass.clone();
    }
    None
}

/// `x instanceof class` (SP2 §1): `true` iff `v` is a `Value::Instance` whose class
/// is `class` or a subclass of it. Walks the `superclass` chain by `Rc::as_ptr`
/// identity — the same identity `find_method`/`super` use. Any non-`Instance` `v`
/// (number, string, object, nil, enum, …) is `false`, never an error. Single source
/// of truth shared by the tree-walker (`apply_binop`) and the VM (`Op::InstanceOf`).
pub(crate) fn is_instance_of(v: &Value, class: &Rc<Class>) -> bool {
    let Value::Instance(inst) = v else {
        return false;
    };
    let target = Rc::as_ptr(class);
    let mut cur = Some(inst.borrow().class.clone());
    while let Some(c) = cur {
        if Rc::as_ptr(&c) == target {
            return true;
        }
        cur = c.superclass.clone();
    }
    false
}

/// Walk a class chain for a STATIC method (SP1 §3), returning it plus the class
/// that defined it. Mirrors `find_method` but over the `static_methods` namespace
/// so a subclass resolves an unknown static up its superclass chain.
pub fn find_static_method(class: &Rc<Class>, name: &str) -> Option<(Rc<Method>, Rc<Class>)> {
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        if let Some(m) = c.static_methods.get(name) {
            return Some((m.clone(), c.clone()));
        }
        cur = c.superclass.clone();
    }
    None
}

/// Merge the declared field schemas across a class's inheritance chain,
/// **base-class first** so a subclass declaration overrides a base one with the
/// same name. Each entry carries the class that declared it, so callers that
/// evaluate field defaults can use the *defining* class's `def_env`. Insertion
/// order is base-first, then subclass (a subclass override keeps the field's
/// original position, matching `IndexMap::insert` semantics).
pub fn merged_field_schema(class: &Rc<Class>) -> IndexMap<String, (FieldSchema, Rc<Class>)> {
    let mut chain = Vec::new();
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        cur = c.superclass.clone();
        chain.push(c);
    }
    let mut schema: IndexMap<String, (FieldSchema, Rc<Class>)> = IndexMap::new();
    for c in chain.into_iter().rev() {
        for (n, s) in &c.fields {
            schema.insert(n.clone(), (s.clone(), c.clone()));
        }
    }
    schema
}

/// A user-defined function with its captured (closure) environment.
pub struct Function {
    pub name: Option<String>,
    pub params: Vec<crate::ast::Param>,
    pub ret: Option<crate::ast::Type>,
    pub body: Vec<Stmt>,
    pub closure: Environment,
    pub is_async: bool,
    pub is_generator: bool,
    /// `worker fn` — Spec A: dispatched to a pooled isolate, returns `future<T>`.
    /// The tree-walker reads this in `call_function` to route to the worker pool.
    pub is_worker: bool,
}

/// SRV §3.3 — an immutable, `Send + Sync`, `Arc`-shared node. A frozen DAG of
/// these is built once by `shared.freeze` and read zero-copy by any isolate.
///
/// **Send-safety invariant (Gate 0):** every field is itself `Send + Sync`
/// (`Arc<str>`, `Arc<[u8]>`, `Arc<[SharedValue]>`, `Decimal`/`i64`/`f64`/`bool` —
/// all `Send`, and recursively `SharedValue = Arc<SharedNode>`; map/set keys use the
/// `Send` `SharedKey`, NOT the `Rc<str>`-bearing `MapKey`). There is **no `Rc`, no
/// `Cc`, no `RefCell`/`Cell`, no `Native`** anywhere in the graph. This makes
/// `Value::Shared(Arc<SharedNode>)` the runtime's FIRST `Send` value. A compile-time
/// `assert_send_sync::<SharedNode>` below makes a future edit that smuggles in a
/// non-`Send` field fail to compile.
///
/// The graph is an immutable, **acyclic** DAG by construction (`shared.freeze`
/// rejects input cycles), so plain `Arc` reference-counting reclaims it — no cycle
/// collector, and the GC traces it as a NO-OP (SRV §3.6).
pub enum SharedNode {
    Nil,
    Bool(bool),
    /// A 64-bit signed integer (NUM §3.1). Frozen from `Value::Int`.
    Int(i64),
    /// A 64-bit IEEE-754 float (NUM §3.1). Frozen from `Value::Float`.
    Float(f64),
    Decimal(Decimal),
    Str(Arc<str>),
    /// An immutable byte slice (vs the mutable `Rc<RefCell<Vec<u8>>>` of `Bytes`).
    Bytes(Arc<[u8]>),
    /// An immutable array slice; children are themselves shared sub-trees.
    Array(Arc<[SharedValue]>),
    /// An ordered, immutable object: insertion-ordered `key -> SharedValue`.
    Object(Arc<SharedMap>),
    /// A `SharedKey -> SharedValue` map, keys canonicalized at freeze time (per
    /// NUM's post-split `MapKey` rule).
    Map(Arc<SharedMapKeyed>),
    /// An insertion-ordered set of canonical `SharedKey`s.
    Set(Arc<SharedSet>),
    /// ADT: a frozen enum variant. `enum_name` + `name` identify it; `value` is the
    /// frozen payload (a unit variant freezes with `value: Nil`).
    EnumVariant {
        enum_name: Arc<str>,
        name: Arc<str>,
        value: SharedValue,
    },
    /// A frozen regex — only the source is retained (recompiled per-isolate on use,
    /// matching the airlock's regex story).
    Regex { source: Arc<str> },
    /// A frozen instance: class NAME + frozen fields. Reads expose fields;
    /// cross-isolate method dispatch is out of scope (SRV §3.8).
    Instance {
        class_name: Arc<str>,
        fields: Arc<SharedMap>,
    },
}

/// An `Arc`-shared, reference-counted frozen node (SRV §3.3).
pub type SharedValue = Arc<SharedNode>;

/// A `Send`-safe frozen map/set key (SRV §3.3). Mirrors [`MapKey`] EXACTLY in
/// canonicalization (NUM's post-split rule: an integral in-range `Float` folds to
/// `Int`, all NaNs unify, −0.0→+0.0), but uses `Arc<str>` for the string case so
/// the whole frozen graph stays `Send + Sync` (a `MapKey`'s `Rc<str>` is `!Send`).
/// Converts to/from `MapKey` at the freeze / read boundary.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum SharedKey {
    Nil,
    Bool(bool),
    Int(i64),
    Num(u64),
    Str(Arc<str>),
    Decimal(Decimal),
}

impl SharedKey {
    /// Freeze a `MapKey` (drawn from a `Map`/`Set`'s already-canonical keys) into a
    /// `Send` `SharedKey`. The `MapKey` is already canonical, so this is a faithful
    /// re-tagging (only the string `Rc`→`Arc`).
    pub fn from_map_key(k: &MapKey) -> SharedKey {
        match k {
            MapKey::Nil => SharedKey::Nil,
            MapKey::Bool(b) => SharedKey::Bool(*b),
            MapKey::Int(i) => SharedKey::Int(*i),
            MapKey::Num(bits) => SharedKey::Num(*bits),
            MapKey::Str(s) => SharedKey::Str(Arc::from(&**s)),
            MapKey::Decimal(d) => SharedKey::Decimal(*d),
        }
    }

    /// Recover a `MapKey` (for reads / membership tests against a live `Map`).
    pub fn to_map_key(&self) -> MapKey {
        match self {
            SharedKey::Nil => MapKey::Nil,
            SharedKey::Bool(b) => MapKey::Bool(*b),
            SharedKey::Int(i) => MapKey::Int(*i),
            SharedKey::Num(bits) => MapKey::Num(*bits),
            SharedKey::Str(s) => MapKey::Str(Rc::from(&**s)),
            SharedKey::Decimal(d) => MapKey::Decimal(*d),
        }
    }

    /// The value form of a frozen key (for display / `keys()`).
    pub fn to_value(&self) -> Value {
        self.to_map_key().to_value()
    }
}

/// An ordered, immutable string-keyed map (Object / Instance fields). A `Vec` of
/// pairs preserves insertion order; lookups are linear, acceptable for the
/// read-only frozen surface (objects are small; large keyed lookups use `Map`).
pub type SharedMap = Vec<(Arc<str>, SharedValue)>;

/// An ordered, immutable `SharedKey`-keyed map (frozen `Map`).
pub type SharedMapKeyed = Vec<(SharedKey, SharedValue)>;

/// An ordered, immutable set of canonical `SharedKey`s (frozen `Set`).
pub type SharedSet = Vec<SharedKey>;

impl SharedNode {
    /// The underlying container/scalar kind name a frozen node reports — a frozen
    /// array `type_name`s `"array"`, a frozen object `"object"`, etc. (SRV §3.5: a
    /// `Shared` reads as the data it froze).
    pub fn kind_name(&self) -> &'static str {
        match self {
            SharedNode::Nil => "nil",
            SharedNode::Bool(_) => "bool",
            SharedNode::Int(_) => "int",
            SharedNode::Float(_) => "float",
            SharedNode::Decimal(_) => "decimal",
            SharedNode::Str(_) => "string",
            SharedNode::Bytes(_) => "bytes",
            SharedNode::Array(_) => "array",
            SharedNode::Object(_) => "object",
            SharedNode::Map(_) => "map",
            SharedNode::Set(_) => "set",
            SharedNode::EnumVariant { .. } => "enum variant",
            SharedNode::Regex { .. } => "regex",
            SharedNode::Instance { .. } => "instance",
        }
    }

    /// Render a frozen node the way its underlying kind prints (SRV §3.5). The
    /// frozen DAG is acyclic by construction, so no cycle guard is needed.
    fn write_display(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SharedNode::Nil => write!(f, "nil"),
            SharedNode::Bool(b) => write!(f, "{}", b),
            SharedNode::Int(i) => write!(f, "{}", i),
            SharedNode::Float(n) => write!(f, "{}", format_float(*n)),
            SharedNode::Decimal(d) => write!(f, "{}", d),
            SharedNode::Str(s) => write!(f, "{}", s),
            SharedNode::Bytes(b) => write!(f, "<bytes len {}>", b.len()),
            SharedNode::Array(a) => {
                write!(f, "[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    v.write_element(f)?;
                }
                write!(f, "]")
            }
            SharedNode::Object(o) => {
                write!(f, "{{")?;
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: ", k)?;
                    v.write_element(f)?;
                }
                write!(f, "}}")
            }
            SharedNode::Map(m) => {
                write!(f, "map {{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", k.to_value())?;
                    write!(f, ": ")?;
                    v.write_element(f)?;
                }
                write!(f, "}}")
            }
            SharedNode::Set(s) => {
                write!(f, "set {{")?;
                for (i, k) in s.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", k.to_value())?;
                }
                write!(f, "}}")
            }
            SharedNode::EnumVariant {
                enum_name, name, ..
            } => write!(f, "{}.{}", enum_name, name),
            SharedNode::Regex { source } => write!(f, "<regex {}>", source),
            SharedNode::Instance { class_name, .. } => write!(f, "<{} instance>", class_name),
        }
    }

    /// Quote bare strings for nested elements (mirrors `Value::write_element`).
    fn write_element(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SharedNode::Str(s) => write!(f, "{:?}", s),
            _ => self.write_display(f),
        }
    }

    /// The container kind a frozen MUTATION targets (`frozen_kind`): only the
    /// mutable container kinds report a name (so `cannot mutate a frozen {kind}`
    /// applies), mirroring `frozen_kind` for the `!Send` `object.freeze` story.
    /// Scalars/regex/enum-variant are not mutable containers → `None`.
    pub(crate) fn mutable_container_kind(&self) -> Option<&'static str> {
        match self {
            SharedNode::Array(_) => Some("array"),
            SharedNode::Object(_) => Some("object"),
            SharedNode::Map(_) => Some("map"),
            SharedNode::Set(_) => Some("set"),
            SharedNode::Instance { .. } => Some("instance"),
            _ => None,
        }
    }
}

// SRV §3.4 (Gate 0): a compile-time proof that the frozen graph is `Send + Sync`.
// If a future edit smuggles a non-`Send` field (an `Rc`, a `Cc`, a `RefCell`) into
// `SharedNode`, THIS fails to compile — the structural Send-safety guarantee. The
// NEGATIVE counterpart `assert_not_impl_any!(Value: Send)` lives in `gc.rs`.
const _: fn() = || {
    fn is_send_sync<T: Send + Sync>() {}
    is_send_sync::<SharedNode>();
};

/// VAL Task 1 — the boxed payload of `Value::GeneratorMethod`. Carries the
/// generator handle plus the bound method name (`"next"`/`"close"`). Boxing the
/// two fields behind one `Rc` collapses a 24-byte two-field variant to a single
/// word, removing the floor that pinned `Value` at 32 bytes.
pub struct GeneratorMethodData {
    pub handle: Rc<crate::coro::GeneratorHandle>,
    pub name: &'static str,
}

/// VAL Task 1 — the boxed payload of `Value::ClassMethod`. Carries the class plus
/// the static/`from` method name. Boxing collapses the other 24-byte two-field
/// variant to a single word (see `GeneratorMethodData`).
pub struct ClassMethodData {
    pub class: Rc<Class>,
    pub name: Rc<str>,
}

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    /// A 64-bit signed integer (NUM §3.1). The exact-arithmetic default subtype of
    /// `number`; literals without a fractional part or exponent lex to `Int`.
    Int(i64),
    /// A 64-bit IEEE-754 float (NUM §3.1). The fractional subtype of `number`;
    /// literals with a `.` or exponent lex to `Float`. (Formerly `Number(f64)`.)
    Float(f64),
    /// Exact decimal arithmetic (96-bit scaled integer via `rust_decimal`).
    /// `Hash + Eq + Ord` via the inner type.
    /// Participates in operator overloading with `Int`/`Float` via coercion.
    ///
    /// VAL Task 2: boxed behind `Rc<Decimal>`. The inner `Decimal` is 16 bytes
    /// (`Copy`), one of the two 16-byte inline payloads (`Str(Rc<str>)` is the
    /// other) that block niche-packing to the 16-byte floor. Behind a 1-word `Rc`,
    /// `Decimal` no longer contributes to the enum width. The boxing is INVISIBLE:
    /// every op decodes (`**d`) before operating, so a boxed `Decimal` is
    /// byte-identical to the old inline one (same `type_name`, exact arithmetic,
    /// Map-key fold). (The enum reaches 16 only once `Str` is ALSO thinned —
    /// Stage 3 / Task 9; see `value_size_is_documented`.)
    Decimal(Rc<Decimal>),
    Str(Rc<str>),
    /// A native built-in function, dispatched by name in the interpreter.
    Builtin(Rc<str>),
    /// A user-defined function carrying its closure environment.
    Function(Rc<Function>),
    /// A bytecode-VM closure: a function prototype plus its captured upvalue
    /// cells. Behaves like `Function` to the user (same `type()`/display);
    /// identity equality. Produced by the VM (V4+); inert in the tree-walker.
    Closure(Cc<crate::vm::value_ext::Closure>),
    Array(Cc<ArrayCell>),
    Object(Cc<ObjectCell>),
    // IndexMap (not HashMap) is deliberate: insertion order is required for
    // deterministic keys/values/entries/display and to match `Object`.
    Map(Cc<MapCell>),
    /// An insertion-ordered hash set of hashable values (spec §11.2).
    /// Elements use the same `MapKey` type as Map keys.
    /// Identity equality (like Array/Map/Bytes).
    Set(Cc<SetCell>),
    /// A mutable byte buffer (spec §11.2). Identity equality, like Array/Map.
    Bytes(Rc<RefCell<Vec<u8>>>),
    /// A compiled regular expression (spec §11.2). Identity equality.
    #[cfg(feature = "data")]
    Regex(Rc<RegexHandle>),
    /// A native resource handle (spec §11.2/§11.4). Always compiled; only the
    /// feature-gated modules (sqlite/process) construct one. Identity equality.
    Native(Rc<NativeObject>),
    /// A method bound to a native handle, dispatched by the async `call_native_method`.
    NativeMethod(Rc<NativeMethod>),
    Enum(Rc<EnumDef>),
    EnumVariant(Rc<EnumVariant>),
    Class(Rc<Class>),
    /// IFACE §4: a structural interface — an immutable, acyclic conformance descriptor
    /// (`Rc<InterfaceDef>`) naming a method set. Identity-equal like `Class`; the RHS
    /// of `instanceof Reader`, the resolved target of a `Reader` annotation. No vtable,
    /// no GC edges (no-op `Trace`).
    Interface(Rc<InterfaceDef>),
    Instance(Cc<RefCell<Instance>>),
    BoundMethod(Rc<BoundMethod>),
    Super(Rc<SuperRef>),
    /// A pending or completed async computation (spec §7, M17 Phase 2). Produced
    /// by calling a script `async fn` and driven by `await`. Identity equality.
    Future(crate::task::SharedFuture),
    /// A running script generator (spec §7, M17 Phase 4). Produced by calling a
    /// `fn*` / `async fn*`; consumed by `for await` or `gen.next(v)`. Holds the
    /// rendezvous channel to the spawned body task. Identity equality.
    Generator(Rc<crate::coro::GeneratorHandle>),
    /// A method bound to a generator handle (e.g. `gen.next`), dispatched by the
    /// async `call_generator_method`. Generators have no `NativeObject`, so they
    /// can't reuse `NativeMethod`; this is the parallel binding for them.
    ///
    /// VAL Task 1: boxed into a single `Rc<GeneratorMethodData>` (one word) — the
    /// two-field form was a 24-byte payload that pinned the whole enum at 32 bytes.
    /// These bindings are rare/cold, so the extra indirection is negligible.
    GeneratorMethod(Rc<GeneratorMethodData>),
    /// A class associated function bound to its class: either the built-in typed
    /// parser `User.from` or a USER static method `User.create` (SP1 §3). The name
    /// is an `Rc<str>` (not `&'static`) so it can carry an arbitrary user static
    /// name; `call_value` resolves it against `static_methods` (chain-walked),
    /// then the built-in `from`.
    ///
    /// VAL Task 1: boxed into a single `Rc<ClassMethodData>` (one word) — see the
    /// `GeneratorMethod` note above; this was the other 24-byte pinning variant.
    ClassMethod(Rc<ClassMethodData>),
    /// SRV §3.2 — an immutable, `Arc`-backed frozen value (`shared.freeze`). The
    /// runtime's FIRST and ONLY `Send`-carrying variant (the union as a whole stays
    /// `!Send` — see the `assert_not_impl_any!(Value: Send)` guard in `gc.rs`).
    /// Reads dispatch like the underlying kind (SRV §3.5); mutation is a Tier-2
    /// panic (SRV §3.8); crosses the worker airlock by `Arc` clone (zero copy).
    Shared(Arc<SharedNode>),
}

// VAL Task 0 / spec §6 — the `!Send`/`!Sync` lock, module-level (compile-time)
// next to the `Value` definition so it fails the build, not just a test run, if a
// future edit (VAL's own NaN-box, SRV's `Arc` leaf, or any variant-adder) ever
// makes `Value` `Send` or `Sync`. That would break the
// `#[tokio::main(flavor = "current_thread")]` + `LocalSet` invariant the whole
// runtime rests on (CLAUDE.md §"The interpreter"). A deliberate future decision to
// make `Value` `Send` must DELETE this assert, surfacing the choice. (The SRV-era
// `assert_not_impl_any!(Value: Send)` in `gc.rs` is a test-body sibling — kept; this
// is the broader compile-time `Send + Sync` guard the VAL spec asks for.)
static_assertions::assert_not_impl_any!(Value: Send, Sync);

impl Value {
    /// NUM §3.3 (BREAKING): the resolved falsy set is `nil`, `false`, `Int(0)`,
    /// a `Float` that is `0.0`/`-0.0`/`NaN`, a `Decimal` equal to zero, and the
    /// empty string `""`. EVERYTHING else is truthy — including non-empty strings
    /// and ALL collections/objects/instances even when empty.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            // `0.0 == -0.0` is `true`, so the `!= 0.0` test covers both signed zeros;
            // NaN is excluded explicitly (`!is_nan()`) → `0.0`/`-0.0`/`NaN` all falsy.
            Value::Float(f) => *f != 0.0 && !f.is_nan(),
            Value::Decimal(d) => **d != Decimal::ZERO,
            Value::Str(s) => !s.is_empty(),
            _ => true,
        }
    }

    /// NUM: central numeric extraction. Returns the `f64` value of any number kind
    /// (`Int` is widened via `i as f64`, `Float` returned as-is). `None` for every
    /// non-number. This is the single helper every "accepts a number" site should
    /// route through so `Int` is first-class everywhere a number was accepted.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// `true` for any number kind (`Int` or `Float`).
    pub fn is_number(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Float(_))
    }

    /// `true` only for `Value::Int`. Used by range lowering to decide whether a
    /// range yields an `Int` sequence (both bounds + step `Int`) or a `Float` one.
    pub fn is_int_value(&self) -> bool {
        matches!(self, Value::Int(_))
    }

    /// NUM: exact integer extraction for integral contexts (indexing, range bounds,
    /// counts, repeat). `Int(i)` yields `i` directly. A `Float` yields `Some` ONLY
    /// when it is finite and integral and within `i64` range; a non-integral or
    /// out-of-range `Float` yields `None` (callers turn that into a Tier-2 panic
    /// such as `array index must be an int, got float`). Non-numbers yield `None`.
    pub fn as_int_exact(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) => {
                if f.is_finite()
                    && f.fract() == 0.0
                    && *f >= i64::MIN as f64
                    // STRICT upper bound: `i64::MAX as f64` rounds UP to 2^63 (out of
                    // i64 range); `-(i64::MIN as f64)` == 2^63 and `<` excludes it so
                    // 2^63 is rejected instead of silently saturating via `as i64`.
                    && *f < -(i64::MIN as f64)
                {
                    Some(*f as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    // ── VAL Task 0: thin zero-cost accessor helpers ─────────────────────────
    // These insulate the rest of the codebase from the physical `Value`
    // encoding. They are `#[inline]` wrappers over the CURRENT enum so that
    // later VAL stages (a niche-shrunk enum, then a NaN-box) change ONLY
    // `value.rs` — call sites read `Value::int(n)` / `v.as_int()` / etc. and
    // never pattern-match the encoding directly. Mirrors NUM's mechanical
    // accessor discipline (`as_f64`/`as_int_exact` above).

    /// Construct an `Int` value (NUM's exact-integer subtype).
    #[inline]
    pub fn int(n: i64) -> Value {
        Value::Int(n)
    }

    /// Construct a `Float` value (NUM's IEEE-754 subtype).
    #[inline]
    pub fn float(n: f64) -> Value {
        Value::Float(n)
    }

    /// Construct an `Object` value from an insertion-ordered key map.
    #[inline]
    pub fn object(map: IndexMap<String, Value>) -> Value {
        Value::Object(ObjectCell::new(map))
    }

    /// Extract the `i64` of an `Int` value EXACTLY — `None` for every other kind
    /// (including `Float`; use `as_int_exact` for the integral-float coercion).
    #[inline]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Extract the `f64` of a `Float` value EXACTLY — `None` for every other kind
    /// (including `Int`; use `as_f64` for the number-widening view).
    #[inline]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// View the underlying `ObjectCell` of an `Object` value, `None` otherwise.
    #[inline]
    pub fn as_object(&self) -> Option<Cc<ObjectCell>> {
        match self {
            Value::Object(o) => Some(o.clone()),
            _ => None,
        }
    }
}

/// Exact `int`-vs-`float` equality (NUM §3.3): `true` iff `i` and `f` denote the
/// same mathematical value. Avoids the lossy `i as f64` round-trip — a non-finite
/// or non-integral `f`, or one outside i64 range, is never equal to any `int`; an
/// integral in-range `f` equals `i` iff `f as i64 == i`.
fn int_eq_float(i: i64, f: f64) -> bool {
    f.is_finite()
        && f.fract() == 0.0
        && f >= i64::MIN as f64
        // STRICT upper bound: `i64::MAX as f64` rounds UP to 2^63 (out of i64 range),
        // so `<=` would admit 2^63 and `f as i64` would saturate to i64::MAX, making
        // `2^63 == i64::MAX` wrongly true. `-(i64::MIN as f64)` == 2^63; `<` excludes it.
        && f < -(i64::MIN as f64)
        && f as i64 == i
}

/// Exact `int`-vs-`float` ordering (NUM §3.3): returns `Some(Ordering)` for the
/// mathematical comparison of `i` and `f`, or `None` iff `f` is `NaN` (which is
/// unordered, exactly like IEEE-754). The comparison is **exact** — it never casts
/// `i as f64` (which would lose precision past 2^53). Strategy: if `f` is integral
/// and within i64 range, compare as integers; otherwise compare `i as f64` vs `f`
/// — but bias by the fractional part / out-of-range magnitude so no precision is
/// lost at the boundary.
pub(crate) fn int_cmp_float(i: i64, f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if f.is_nan() {
        return None;
    }
    if f == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if f == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    // `f` is finite. If it is below the i64 range, every i64 is greater; above the
    // range, every i64 is smaller. The bounds `i64::MIN as f64` (= -2^63, exact)
    // and `i64::MAX as f64` (= 2^63, since 2^63-1 rounds up) frame the range:
    // `f < -2^63` ⇒ i > f; `f >= 2^63` ⇒ i < f (no i64 reaches 2^63).
    if f < i64::MIN as f64 {
        return Some(Ordering::Greater);
    }
    if f >= -(i64::MIN as f64) {
        // -(i64::MIN as f64) == 2^63; no i64 is >= 2^63.
        return Some(Ordering::Less);
    }
    // Now `-2^63 <= f < 2^63`, so `f.trunc()` fits in i64 exactly.
    let trunc = f.trunc() as i64;
    match i.cmp(&trunc) {
        // Same integer part: the fraction decides. `f.fract()` is in (-1, 1); a
        // positive fraction makes `f` larger than its truncation, a negative one
        // smaller. `i == trunc` so compare against the fraction's sign.
        Ordering::Equal => {
            let frac = f.fract();
            if frac > 0.0 {
                Some(Ordering::Less) // i == trunc < f
            } else if frac < 0.0 {
                Some(Ordering::Greater) // i == trunc > f
            } else {
                Some(Ordering::Equal)
            }
        }
        other => Some(other),
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            // Cross-subtype numeric equality is EXACT (NUM §3.3): an `int` equals a
            // `float` iff they are mathematically equal — no lossy `i as f64` cast
            // (which would make `2**53+1 == float(2**53)`). Symmetric.
            (Value::Int(i), Value::Float(f)) | (Value::Float(f), Value::Int(i)) => {
                int_eq_float(*i, *f)
            }
            // Decimal: same-type value equality by the Decimal's own PartialEq.
            // Cross-type Number↔Decimal equality is handled in the evaluator's
            // Eq/Ne path, not here.
            (Value::Decimal(a), Value::Decimal(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            // Built-ins are equal iff they name the same function.
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            // Functions compare by identity.
            (Value::Function(a), Value::Function(b)) => Rc::ptr_eq(a, b),
            (Value::Closure(a), Value::Closure(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Array(a), Value::Array(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Object(a), Value::Object(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Map(a), Value::Map(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Set(a), Value::Set(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::Bytes(a), Value::Bytes(b)) => Rc::ptr_eq(a, b),
            #[cfg(feature = "data")]
            (Value::Regex(a), Value::Regex(b)) => Rc::ptr_eq(a, b),
            // Native handles and bound native methods compare by identity.
            (Value::Native(a), Value::Native(b)) => Rc::ptr_eq(a, b),
            (Value::NativeMethod(a), Value::NativeMethod(b)) => Rc::ptr_eq(a, b),
            // Enums and their (interned) variants compare by identity.
            (Value::Enum(a), Value::Enum(b)) => Rc::ptr_eq(a, b),
            // ADT §5.2: unit / constructor variants compare by interned IDENTITY
            // (byte-identical to pre-ADT). A CONSTRUCTED payload variant compares
            // STRUCTURALLY: same enum, same variant name, payloads equal element-wise
            // (positional) or key-wise (named, via the existing Object `PartialEq`).
            (Value::EnumVariant(a), Value::EnumVariant(b)) => {
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                match (&a.payload, &b.payload) {
                    // At least one is a payload variant → structural compare. (A
                    // payload variant is never `==` a unit/constructor of the same
                    // name: a unit's `payload` is `None`, so the arms below short out.)
                    (Some(pa), Some(pb)) => {
                        a.enum_name == b.enum_name
                            && a.name == b.name
                            && match (pa, pb) {
                                (Payload::Positional(xa), Payload::Positional(xb)) => {
                                    *xa.borrow() == *xb.borrow()
                                }
                                (Payload::Named(oa), Payload::Named(ob)) => {
                                    oa.content_eq(ob)
                                }
                                _ => false,
                            }
                    }
                    // Both unit/constructor but distinct `Rc`s → not equal (interned,
                    // so identity is the only equality; a re-interning failure across
                    // a worker boundary is handled by §6 re-interning, not here).
                    _ => false,
                }
            }
            // Classes/instances/bound-methods/super compare by identity.
            (Value::Class(a), Value::Class(b)) => Rc::ptr_eq(a, b),
            // Interfaces compare by identity (immutable descriptors, IFACE §4).
            (Value::Interface(a), Value::Interface(b)) => Rc::ptr_eq(a, b),
            (Value::Instance(a), Value::Instance(b)) => crate::gc::cc_ptr_eq(a, b),
            (Value::BoundMethod(a), Value::BoundMethod(b)) => Rc::ptr_eq(a, b),
            (Value::Super(a), Value::Super(b)) => Rc::ptr_eq(a, b),
            // Futures compare by identity (same completion cell).
            (Value::Future(a), Value::Future(b)) => a.ptr_eq(b),
            // Generators compare by identity (same body channel).
            (Value::Generator(a), Value::Generator(b)) => Rc::ptr_eq(a, b),
            (Value::GeneratorMethod(a), Value::GeneratorMethod(b)) => {
                Rc::ptr_eq(&a.handle, &b.handle) && a.name == b.name
            }
            (Value::ClassMethod(a), Value::ClassMethod(b)) => {
                Rc::ptr_eq(&a.class, &b.class) && a.name == b.name
            }
            // SRV §3.5: a frozen `Shared` compares by `Arc` IDENTITY (like every
            // other container's identity-equality). Two `Shared`s wrapping the SAME
            // `Arc` are equal (idempotent `freeze` returns the same `Arc`); distinct
            // `Arc`s are NOT equal even if structurally identical; a `Shared` never
            // equals a non-frozen container (a distinct value kind → `_`).
            (Value::Shared(a), Value::Shared(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "Nil"),
            Value::Bool(b) => write!(f, "Bool({})", b),
            Value::Int(i) => write!(f, "Int({})", i),
            Value::Float(n) => write!(f, "Float({})", n),
            Value::Decimal(d) => write!(f, "Decimal({})", d),
            Value::Str(s) => write!(f, "Str({:?})", s),
            Value::Builtin(name) => write!(f, "Builtin({:?})", name),
            Value::Function(func) => {
                write!(
                    f,
                    "Function({})",
                    func.name.as_deref().unwrap_or("<anonymous>")
                )
            }
            Value::Closure(_) => write!(f, "Closure(<anonymous>)"),
            Value::Array(a) => write!(f, "Array(len {})", a.borrow().len()),
            Value::Object(o) => write!(f, "Object(len {})", o.len()),
            Value::Map(m) => write!(f, "Map(len {})", m.borrow().len()),
            Value::Set(s) => write!(f, "Set(len {})", s.borrow().len()),
            Value::Bytes(b) => write!(f, "Bytes(len {})", b.borrow().len()),
            #[cfg(feature = "data")]
            Value::Regex(r) => write!(f, "Regex({:?})", r.source),
            Value::Native(n) => write!(f, "Native({} #{})", n.kind.type_name(), n.id),
            Value::NativeMethod(m) => write!(
                f,
                "NativeMethod({}.{})",
                m.receiver.kind.type_name(),
                m.method
            ),
            Value::Enum(e) => write!(f, "Enum({})", e.name),
            Value::EnumVariant(v) => match &v.payload {
                None => write!(f, "EnumVariant({}.{})", v.enum_name, v.name),
                Some(_) => write!(f, "EnumVariant({}.{}(..))", v.enum_name, v.name),
            },
            Value::Class(c) => write!(f, "Class({})", c.name),
            Value::Interface(i) => write!(f, "Interface({})", i.name),
            Value::Instance(i) => write!(f, "Instance({})", i.borrow().class.name),
            Value::BoundMethod(b) => write!(f, "BoundMethod({})", b.name),
            Value::Super(_) => write!(f, "Super"),
            Value::Future(_) => write!(f, "Future"),
            Value::Generator(_) => write!(f, "Generator"),
            Value::GeneratorMethod(g) => write!(f, "GeneratorMethod({})", g.name),
            Value::ClassMethod(c) => write!(f, "ClassMethod({}.{})", c.class.name, c.name),
            Value::Shared(n) => write!(f, "Shared({})", n.kind_name()),
        }
    }
}

/// NUM §4: render a `float` (`f64`) the way AScript prints/`str()`s it. Unlike
/// Rust's `f64` Display (which prints `7.0` as `"7"`), a `float` ALWAYS shows at
/// least one fractional digit so it is visually distinguishable from an `int`
/// (the Python/Swift convention): `5.0`, `1500.0`, `-0.0`. `inf`/`-inf`/`nan`
/// pass through Rust's Display unchanged. This is the single shared spelling so
/// the tree-walker and the VM (and every str()/print/template path that routes
/// through `Value::Float` Display) agree byte-for-byte.
pub fn format_float(n: f64) -> String {
    if n.is_finite() {
        if n.fract() == 0.0 {
            // Integral finite float: append `.0`. `{}` on `-0.0` yields `-0`, so
            // the `.0` suffix gives `-0.0` / `0.0` / `7.0` uniformly.
            format!("{n}.0")
        } else {
            format!("{n}")
        }
    } else {
        // inf / -inf / NaN: unchanged ("inf", "-inf", "NaN").
        format!("{n}")
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
            Value::Int(i) => write!(f, "{}", i),
            // NUM §4: a `float` always shows a decimal (`5.0`, not `5`) so it is
            // distinguishable from an `int`. See `format_float`.
            Value::Float(n) => write!(f, "{}", format_float(*n)),
            // Decimal: print the canonical string (scale preserved, e.g. "1.50").
            Value::Decimal(d) => write!(f, "{}", d),
            Value::Str(s) => write!(f, "{}", s),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::Function(func) => match &func.name {
                Some(n) => write!(f, "<function {}>", n),
                None => write!(f, "<function>"),
            },
            // A VM closure has no name on its proto, so it displays exactly like
            // an anonymous `Function`. (Same concept to the user.)
            Value::Closure(_) => write!(f, "<function>"),
            Value::Array(a) => {
                let ptr = crate::gc::cc_addr(a);
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
                let ptr = crate::gc::cc_addr(o);
                if seen.contains(&ptr) {
                    return write!(f, "{{...}}");
                }
                seen.push(ptr);
                write!(f, "{{")?;
                let entries = o.entries();
                for (i, (k, v)) in entries.iter().enumerate() {
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
            Value::Map(m) => {
                let ptr = crate::gc::cc_addr(m);
                if seen.contains(&ptr) {
                    return write!(f, "map {{...}}");
                }
                seen.push(ptr);
                write!(f, "map {{")?;
                for (i, (k, v)) in m.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    k.to_value().write_element(f, seen)?;
                    write!(f, ": ")?;
                    v.write_element(f, seen)?;
                }
                write!(f, "}}")?;
                seen.pop();
                Ok(())
            }
            Value::Set(s) => {
                let ptr = crate::gc::cc_addr(s);
                if seen.contains(&ptr) {
                    return write!(f, "set {{...}}");
                }
                seen.push(ptr);
                write!(f, "set {{")?;
                for (i, k) in s.borrow().iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    k.to_value().write_element(f, seen)?;
                }
                write!(f, "}}")?;
                seen.pop();
                Ok(())
            }
            Value::Bytes(b) => write!(f, "<bytes len {}>", b.borrow().len()),
            #[cfg(feature = "data")]
            Value::Regex(r) => write!(f, "<regex {}>", r.source),
            Value::Native(n) => write!(f, "<native {} #{}>", n.kind.type_name(), n.id),
            Value::NativeMethod(m) => write!(f, "<native method {}>", m.method),
            Value::Enum(e) => write!(f, "<enum {}>", e.name),
            Value::EnumVariant(v) => match &v.payload {
                // Unit / scalar-backed / constructor: byte-identical to pre-ADT.
                None => write!(f, "{}.{}", v.enum_name, v.name),
                // ADT: a constructed payload variant renders as `Enum.Variant(a, b)`
                // (positional) or `Enum.Variant(name: v, ...)` (named). Cycle-guarded
                // via the shared `seen` set (a recursive payload can self-reference).
                Some(Payload::Positional(a)) => {
                    let ptr = crate::gc::cc_addr(a);
                    if seen.contains(&ptr) {
                        return write!(f, "{}.{}(...)", v.enum_name, v.name);
                    }
                    seen.push(ptr);
                    write!(f, "{}.{}(", v.enum_name, v.name)?;
                    for (i, it) in a.borrow().iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        it.write_element(f, seen)?;
                    }
                    write!(f, ")")?;
                    seen.pop();
                    Ok(())
                }
                Some(Payload::Named(o)) => {
                    let ptr = crate::gc::cc_addr(o);
                    if seen.contains(&ptr) {
                        return write!(f, "{}.{}(...)", v.enum_name, v.name);
                    }
                    seen.push(ptr);
                    write!(f, "{}.{}(", v.enum_name, v.name)?;
                    let entries = o.entries();
                    for (i, (k, val)) in entries.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}: ", k)?;
                        val.write_element(f, seen)?;
                    }
                    write!(f, ")")?;
                    seen.pop();
                    Ok(())
                }
            },
            Value::Class(c) => write!(f, "<class {}>", c.name),
            Value::Interface(i) => write!(f, "<interface {}>", i.name),
            Value::Instance(i) => write!(f, "<{} instance>", i.borrow().class.name),
            Value::BoundMethod(b) => write!(f, "<method {}>", b.name),
            Value::Super(_) => write!(f, "<super>"),
            Value::Future(_) => write!(f, "<future>"),
            Value::Generator(_) => write!(f, "<generator>"),
            Value::GeneratorMethod(g) => write!(f, "<generator method {}>", g.name),
            Value::ClassMethod(c) => write!(f, "<class method {}.{}>", c.class.name, c.name),
            // SRV §3.5: a frozen `Shared` prints like the value it froze (a frozen
            // object as `{...}`, a frozen array as `[...]`, a scalar bare).
            Value::Shared(n) => n.write_display(f),
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

    // Task 0.1 regression: `i64::MAX as f64` rounds UP to 2^63 (out of i64 range),
    // so a `<=` upper bound wrongly admitted 2^63 across equality, MapKey folding,
    // and `as_int_exact`. The strict `< -(i64::MIN as f64)` bound rejects it.
    #[test]
    fn float_two_pow_63_is_not_i64_max() {
        let two63 = 9223372036854775808.0_f64; // 2^63, NOT representable as i64
        assert!(!int_eq_float(i64::MAX, two63));
        assert_eq!(Value::Float(two63).as_int_exact(), None);
        // `MapKey` has no `Debug`, so compare for inequality with `==` directly.
        assert!(
            MapKey::from_value(&Value::Float(two63))
                != MapKey::from_value(&Value::Int(i64::MAX)),
            "2^63 float must not share a map key with i64::MAX"
        );
        // The largest in-range integral float (2^63 − 2048) still folds correctly.
        let max_in_range = 9223372036854773760.0_f64;
        assert!(int_eq_float(9223372036854773760, max_in_range));
        assert_eq!(Value::Float(max_in_range).as_int_exact(), Some(9223372036854773760));
    }

    // VAL Task 0 — the MOVING SIZE TRIPWIRE. `Value` is the runtime tagged union
    // threaded through every fiber stack slot, frame slot, array element, and map
    // slot; its width is a load-bearing performance fact. This test pins the
    // measured baseline so a careless edit that widens a variant is caught
    // immediately, and each VAL stage updates the asserted constant as the enum
    // shrinks (32 → ≤24 → 16 → 8). The companion `value_size_print` is `ignore`d
    // (it just surfaces the number when run with `--nocapture`).
    #[test]
    fn value_size_is_documented() {
        // VAL Task 2: `Decimal` is now boxed behind `Rc<Decimal>` (one word, was a
        // 16-byte inline payload). This is a necessary, behavior-preserving step
        // toward the 16-byte niche floor — but the enum stays at **24** because
        // `Str(Rc<str>)` is STILL a 16-byte *fat* pointer (data ptr + length) and is
        // now the widest payload:
        //
        //   The inline scalar variants (`Int(i64)`/`Float(f64)`) take ANY bit
        //   pattern, so Rust cannot niche-elide the discriminant into a pointer
        //   variant's null niche — it must add an explicit tag word. The layout is
        //   therefore `round_up(widest_payload) + 8-byte tag`: with the 16-byte fat
        //   `Str`, that is 16 + 8 = **24**; with a single-word (thin) `Str` it is
        //   8 + 8 = **16**. (Scratch-verified at the real variant count: fat-`Str`
        //   enum = 24 even with NO scalar and even with few variants; thin-`Str`
        //   enum = 16. So the floor is set by the fat-pointer payload WIDTH, not by
        //   Decimal — which is exactly why boxing Decimal alone cannot reach 16.)
        //
        // Reaching 16 therefore requires thinning `Str` to a single-word pointer —
        // that is **Task 9 (small-string / thin-`Str`)**, OUTSIDE this unit
        // (Tasks 0–2). Boxing `Decimal` here removes the OTHER 16-byte inline payload
        // so that once `Str` is thinned the enum drops straight to 16. NOTE: the spec
        // §3.3 / plan Task-2 "fat-`Str` → 16" target is arithmetically wrong (it is
        // 24); corrected in the spec/plan. 8 bytes is reachable ONLY via the NaN-box
        // (a hand-tagged machine word, not a Rust enum), which is a separate, gated
        // stage.
        //
        // Size progression: 32 → 24 [Task 1: fat method bindings boxed] → 24
        // [Task 2: Decimal boxed, now fat-`Str`-limited] → 16 [thin-`Str`] → 8
        // [NaN-box, gated].
        assert_eq!(std::mem::size_of::<Value>(), 24);
    }

    #[test]
    #[ignore]
    fn value_size_print() {
        eprintln!("size_of::<Value>() = {}", std::mem::size_of::<Value>());
    }

    // VAL Task 0 — accessor round-trip: the thin, zero-cost constructor/extractor
    // helpers (`Value::int`/`float`/`object`, `as_int`/`as_float`/`as_object`)
    // insulate the rest of the tree from the physical encoding so later VAL stages
    // change ONLY `value.rs`. Round-trip identity must hold.
    #[test]
    fn accessor_round_trip() {
        assert_eq!(Value::int(5).as_int(), Some(5));
        assert_eq!(Value::int(-2_000_000).as_int(), Some(-2_000_000));
        assert_eq!(Value::float(3.5).as_float(), Some(3.5));
        // Cross-kind: an int is not a float and vice versa (no silent coercion).
        assert_eq!(Value::int(5).as_float(), None);
        assert_eq!(Value::float(3.5).as_int(), None);
        // Object round-trip preserves pointer identity.
        let obj = Value::object(IndexMap::new());
        let cell = obj.as_object().expect("object accessor");
        assert!(matches!(&obj, Value::Object(c) if crate::gc::cc_ptr_eq(c, &cell)));
    }

    // VAL Task 3 — the SMI↔boxed spill-BOUNDARY round-trip + Map-key fold (spec
    // §7.2), value-layer half. The boundary values straddle the NaN-box SMI budget
    // (`i48`, range `[−2^47, 2^47 − 1]`): under the Stage-1 niche-fallback layout
    // `Int` is a FULL inline `i64` — there is no SMI and no spill, so every value
    // below round-trips through the inline scalar word TRIVIALLY (this is the
    // assertion the plan/spec require to pass today; it becomes the LIVE SMI/spill
    // boundary if/when the Stage-2 NaN-box lands and the encoding gains a 48-bit
    // SMI). The engine-level (tree-walker == specialized-VM == generic-VM) half of
    // §7.2 — arithmetic carry, comparison, and Map-key fold across the boundary on
    // real `.as` programs — lives in `tests/vm_differential.rs`
    // (`smi_boundary_*`).
    #[test]
    fn smi_boundary_round_trip_and_mapkey_fold() {
        // The exact boundary values from spec §7.2: the i48 spill edges, plus a few
        // beyond the budget (which spill to a boxed `Int` under the NaN-box; stay a
        // full inline `i64` under the Stage-1 fallback).
        let boundary: [i64; 7] = [
            (1i64 << 47) - 1, // 2^47 − 1  (largest i48 SMI)
            1i64 << 47,       // 2^47      (first spill, positive)
            -(1i64 << 47),    // −2^47     (smallest i48 SMI)
            -(1i64 << 47) - 1, // −2^47 − 1 (first spill, negative)
            1i64 << 53,       // 2^53      (well beyond)
            i64::MAX,
            i64::MIN,
        ];
        for &n in &boundary {
            // Round-trip through the value encoding via the Task-0 accessors:
            // `decode(encode(n)) == n` (the §7.2 round-trip property). Under the
            // inline-`i64` Stage-1 layout this is an exact pass-through.
            assert_eq!(Value::int(n).as_int(), Some(n), "as_int round-trip for {n}");
            // Round-trip through the Map-key fold: an `Int` value folds to
            // `MapKey::Int(n)` and recovers the SAME value. (Under a future NaN-box,
            // an SMI `Int` and a boxed `Int` of equal value MUST fold to the same
            // key — there is only one logical `Int(n)`, so this property is what
            // pins that invariant. Today both encodings are the inline `i64`.)
            let key = MapKey::from_value(&Value::int(n)).expect("int is hashable");
            // `MapKey` is not `Debug`, so compare with a boolean assert.
            assert!(key == MapKey::Int(n), "MapKey fold for {n}");
            assert_eq!(key.to_value().as_int(), Some(n), "MapKey recover for {n}");
        }
        // Cross-boundary fold equality: two `Int`s of the same value (regardless of
        // any future SMI/boxed encoding split) are the SAME Map key. Constructed via
        // two independent paths to mimic an "SMI vs boxed of equal value" pairing.
        let lo = (1i64 << 47) - 1;
        for &n in &[lo, lo + 1, -(1i64 << 47), i64::MAX, i64::MIN] {
            let a = MapKey::from_value(&Value::int(n)).unwrap();
            let b = MapKey::from_value(&Value::Int(n)).unwrap();
            assert!(a == b, "equal-valued Ints must fold to the SAME Map key ({n})");
        }
    }

    // ADT Task 1 helpers — construct variant values directly at the value layer.
    fn unit_variant(en: &str, name: &str, backing: Value) -> Value {
        Value::EnumVariant(Rc::new(EnumVariant {
            enum_name: en.to_string(),
            name: name.to_string(),
            value: backing,
            payload: None,
            ctor: false,
        def: None,
        }))
    }
    fn pos_variant(en: &str, name: &str, items: Vec<Value>) -> Value {
        Value::EnumVariant(Rc::new(EnumVariant {
            enum_name: en.to_string(),
            name: name.to_string(),
            value: Value::Nil,
            payload: Some(Payload::Positional(ArrayCell::new(items))),
            ctor: false,
        def: None,
        }))
    }
    fn named_variant(en: &str, name: &str, fields: Vec<(&str, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in fields {
            m.insert(k.to_string(), v);
        }
        Value::EnumVariant(Rc::new(EnumVariant {
            enum_name: en.to_string(),
            name: name.to_string(),
            value: Value::Nil,
            payload: Some(Payload::Named(ObjectCell::new(m))),
            ctor: false,
        def: None,
        }))
    }

    #[test]
    fn adt_unit_variant_is_byte_identical_to_pre_adt() {
        // A `payload: None, ctor: false` unit variant: `.value` is the backing scalar
        // (or Nil), it is truthy, and two DISTINCT `Rc`s of the same name are NOT
        // equal (identity equality, as pre-ADT — interning makes real uses equal).
        let red = unit_variant("Color", "Red", Value::Nil);
        let green = unit_variant("Color", "Green", Value::Int(2));
        assert!(red.is_truthy());
        assert!(green.is_truthy());
        // Distinct allocations of the same unit variant are NOT `==` (identity).
        let red2 = unit_variant("Color", "Red", Value::Nil);
        assert_ne!(red, red2);
        // But cloning the SAME `Rc` is equal (the interned-use case).
        assert_eq!(red.clone(), red);
    }

    #[test]
    fn adt_constructed_variants_compare_structurally() {
        // Positional: `Pair(3, 4) == Pair(3, 4)`, `!= Pair(3, 5)`.
        let p1 = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(4)]);
        let p2 = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(4)]);
        let p3 = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(5)]);
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
        // Named: `Circle(radius: 2.0) == Circle(radius: 2.0)`, `!= Circle(radius: 3.0)`.
        let c1 = named_variant("Shape", "Circle", vec![("radius", Value::Float(2.0))]);
        let c2 = named_variant("Shape", "Circle", vec![("radius", Value::Float(2.0))]);
        let c3 = named_variant("Shape", "Circle", vec![("radius", Value::Float(3.0))]);
        assert_eq!(c1, c2);
        assert_ne!(c1, c3);
        // A payload variant is never equal to a unit variant of the same name.
        let unit_circle = unit_variant("Shape", "Circle", Value::Nil);
        assert_ne!(c1, unit_circle);
        // Different variant names with equal payload are not equal.
        let other = pos_variant("Shape", "Other", vec![Value::Int(3), Value::Int(4)]);
        assert_ne!(p1, other);
        // Constructed payload variants are truthy.
        assert!(p1.is_truthy());
        assert!(c1.is_truthy());
    }

    #[test]
    fn adt_constructed_variant_display() {
        let pair = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(4)]);
        assert_eq!(pair.to_string(), "Shape.Pair(3, 4)");
        let circle = named_variant("Shape", "Circle", vec![("radius", Value::Float(2.0))]);
        assert_eq!(circle.to_string(), "Shape.Circle(radius: 2.0)");
        // Unit variant display is unchanged: `Enum.Variant`.
        let red = unit_variant("Color", "Red", Value::Nil);
        assert_eq!(red.to_string(), "Color.Red");
        // Nested string payload quotes the inner string (write_element).
        let str_v = pos_variant("Json", "Str", vec![Value::Str("hi".into())]);
        assert_eq!(str_v.to_string(), "Json.Str(\"hi\")");
    }

    #[test]
    fn adt_payload_variant_is_not_a_map_key() {
        // Payload variants are identity-style containers (like Array/Map): NOT
        // hashable as a `MapKey`. Unit variants were never hashable either (today's
        // behavior is preserved — both return `None`).
        let pair = pos_variant("Shape", "Pair", vec![Value::Int(3), Value::Int(4)]);
        assert!(MapKey::from_value(&pair).is_none());
        let red = unit_variant("Color", "Red", Value::Nil);
        assert!(MapKey::from_value(&red).is_none());
    }

    #[test]
    fn adt_type_name_unchanged_for_payload_variant() {
        // The runtime `type_name` for any EnumVariant stays "enum variant" (the
        // wildcard arm). Asserted at the interp layer; here we assert the value-layer
        // Debug differentiates payload vs unit (used in panics/tests only).
        let red = unit_variant("Color", "Red", Value::Nil);
        let pair = pos_variant("Shape", "Pair", vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(format!("{:?}", red), "EnumVariant(Color.Red)");
        assert_eq!(format!("{:?}", pair), "EnumVariant(Shape.Pair(..))");
    }

    #[test]
    fn displays_values_like_a_script_language() {
        // NUM §4: a `float` always renders with at least one fractional digit so it
        // is visually distinguishable from an `int` (Python/Swift convention).
        assert_eq!(Value::Float(7.0).to_string(), "7.0");
        assert_eq!(Value::Float(2.5).to_string(), "2.5");
        assert_eq!(Value::Float(1500.0).to_string(), "1500.0");
        assert_eq!(Value::Float(-0.0).to_string(), "-0.0");
        assert_eq!(Value::Float(0.0).to_string(), "0.0");
        assert_eq!(Value::Float(f64::INFINITY).to_string(), "inf");
        assert_eq!(Value::Float(f64::NEG_INFINITY).to_string(), "-inf");
        assert_eq!(Value::Float(f64::NAN).to_string(), "NaN");
        // `int` keeps NO decimal.
        assert_eq!(Value::Int(5).to_string(), "5");
        assert_eq!(Value::Int(-7).to_string(), "-7");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Nil.to_string(), "nil");
        assert_eq!(Value::Str("hi".into()).to_string(), "hi");
    }

    #[test]
    fn float_in_collections_keeps_decimal() {
        let arr = Value::Array(crate::value::ArrayCell::new(vec![
            Value::Float(1.0),
            Value::Float(2.0),
        ]));
        assert_eq!(arr.to_string(), "[1.0, 2.0]");
    }

    #[test]
    fn truthiness_follows_spec() {
        // NUM: falsy = nil, false, 0 (int), 0.0/-0.0/NaN (float), 0 decimal, "" (string).
        // Everything else — incl. non-empty strings and all collections even when empty — is truthy.
        assert!(Value::Bool(true).is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Nil.is_truthy());
        assert!(!Value::Int(0).is_truthy());
        assert!(Value::Int(1).is_truthy());
        assert!(!Value::Float(0.0).is_truthy());
        assert!(!Value::Float(-0.0).is_truthy());
        assert!(!Value::Float(f64::NAN).is_truthy());
        assert!(Value::Float(0.5).is_truthy());
        assert!(!Value::Str("".into()).is_truthy());
        assert!(Value::Str("x".into()).is_truthy());
    }

    #[test]
    fn equality_is_structural_and_cross_kind_is_false() {
        assert_eq!(Value::Float(1.0), Value::Float(1.0));
        assert_eq!(Value::Str("a".into()), Value::Str("a".into()));
        assert_ne!(Value::Float(1.0), Value::Str("1".into()));
        assert_ne!(Value::Bool(true), Value::Float(1.0));
    }

    #[test]
    fn builtins_compare_by_name_and_are_truthy() {
        assert_eq!(
            Value::Builtin("print".into()),
            Value::Builtin("print".into())
        );
        assert_ne!(Value::Builtin("print".into()), Value::Builtin("len".into()));
        assert!(Value::Builtin("print".into()).is_truthy());
        assert_eq!(
            Value::Builtin("print".into()).to_string(),
            "<builtin print>"
        );
    }

    #[test]
    fn arrays_compare_by_identity_and_display() {
        

        let a = Value::Array(crate::value::ArrayCell::new(vec![
            Value::Float(1.0),
            Value::Str("two".into()),
        ]));
        assert_eq!(a.to_string(), "[1.0, \"two\"]");
        // identity: a clone of the SAME Rc is equal; a fresh array is not
        assert_eq!(a.clone(), a);
        let b = Value::Array(crate::value::ArrayCell::new(vec![Value::Float(1.0)]));
        assert_ne!(a, b);
        assert!(a.is_truthy());
    }

    #[test]
    fn maps_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert(MapKey::Str("a".into()), Value::Float(1.0));
        m.insert(MapKey::Num(0.0f64.to_bits()), Value::Str("zero".into()));
        let map = Value::Map(crate::value::MapCell::new(m));
        assert_eq!(map.to_string(), "map {\"a\": 1.0, 0.0: \"zero\"}");
        assert_eq!(map.clone(), map);
        assert!(map.is_truthy());
        assert!(MapKey::from_value(&Value::Float(0.0)).is_some());
        assert!(
            MapKey::from_value(&Value::Array(crate::value::ArrayCell::new(vec![]))).is_none()
        );
    }

    #[test]
    fn mapkey_number_and_decimal_are_distinct() {
        use rust_decimal::Decimal;
        // Number 1 and Decimal 1 must produce DIFFERENT map keys, so they index
        // distinct slots in a Map/Set. This pins the MapKey::Decimal claim directly.
        // (MapKey intentionally has no Debug derive, so compare via bool to avoid
        // requiring it in assert_eq!/assert_ne!.)
        let num_key = MapKey::from_value(&Value::Float(1.0)).expect("number is hashable");
        let dec_key =
            MapKey::from_value(&Value::Decimal(Rc::new(Decimal::from(1)))).expect("decimal is hashable");
        assert!(
            num_key != dec_key,
            "number 1 and decimal 1 must be distinct map keys"
        );
        // Two equal Decimals produce the same key (round-trips through to_value).
        let a = MapKey::from_value(&Value::Decimal(Rc::new(Decimal::from(1))));
        let b = MapKey::from_value(&Value::Decimal(Rc::new(Decimal::from(1))));
        assert!(a == b);
        assert_eq!(dec_key.to_value(), Value::Decimal(Rc::new(Decimal::from(1))));
    }

    // ---- IFACE Task 1: Value::Interface descriptor ----

    fn iface(name: &str) -> Rc<InterfaceDef> {
        Rc::new(InterfaceDef {
            name: name.to_string(),
            own_methods: IndexMap::new(),
            extends: Vec::new(),
            def_env: crate::interp::global_env(),
            flat: RefCell::new(None),
        })
    }

    #[test]
    fn iface_value_basics() {
        let r = iface("Reader");
        let v = Value::Interface(r.clone());
        // type_name → "interface"
        assert_eq!(crate::interp::type_name(&v), "interface");
        // truthy (a descriptor is truthy)
        assert!(v.is_truthy());
        // Display → "<interface Reader>" (mirrors "<class Foo>")
        assert_eq!(format!("{}", v), "<interface Reader>");
        // same Rc → equal (identity)
        assert_eq!(v.clone(), v);
        assert_eq!(Value::Interface(r.clone()), Value::Interface(r));
        // two distinct Rcs of the same name → NOT equal (identity, not structural)
        assert_ne!(Value::Interface(iface("Reader")), Value::Interface(iface("Reader")));
    }

    // ---- NUM Task 1: int subtype, truthiness, MapKey fold, cross-subtype eq ----

    #[test]
    fn num_type_names_distinguish_int_and_float() {
        assert_eq!(crate::interp::type_name(&Value::Int(5)), "int");
        assert_eq!(crate::interp::type_name(&Value::Float(5.0)), "float");
        // Decimal is its own subtype, unchanged.
        assert_eq!(
            crate::interp::type_name(&Value::Decimal(Rc::new(Decimal::from(1)))),
            "decimal"
        );
    }

    #[test]
    fn num_int_cmp_float_is_exact_at_boundaries() {
        use std::cmp::Ordering;
        // Trivial integral cases.
        assert_eq!(int_cmp_float(2, 2.5), Some(Ordering::Less));
        assert_eq!(int_cmp_float(3, 2.5), Some(Ordering::Greater));
        assert_eq!(int_cmp_float(2, 2.0), Some(Ordering::Equal));
        // NaN is unordered.
        assert_eq!(int_cmp_float(1, f64::NAN), None);
        // Infinities.
        assert_eq!(int_cmp_float(i64::MAX, f64::INFINITY), Some(Ordering::Less));
        assert_eq!(
            int_cmp_float(i64::MIN, f64::NEG_INFINITY),
            Some(Ordering::Greater)
        );
        // 2^53 boundary: 2^53+1 (exact i64) vs 2^53.0 — the int is strictly
        // greater, despite (2^53+1) as f64 rounding back to 2^53.
        let two53_plus1 = (1i64 << 53) + 1;
        let two53_f = (1u64 << 53) as f64;
        assert_eq!(int_cmp_float(two53_plus1, two53_f), Some(Ordering::Greater));
        assert!(int_eq_float(1i64 << 53, two53_f)); // exactly equal at 2^53
        assert!(!int_eq_float(two53_plus1, two53_f)); // 2^53+1 != 2^53.0
        // Far out-of-range floats: every i64 is below 1e300 and above -1e300.
        assert_eq!(int_cmp_float(i64::MAX, 1e300), Some(Ordering::Less));
        assert_eq!(int_cmp_float(i64::MIN, -1e300), Some(Ordering::Greater));
        // Negative fractional near an int.
        assert_eq!(int_cmp_float(-3, -3.5), Some(Ordering::Greater));
        assert_eq!(int_cmp_float(-4, -3.5), Some(Ordering::Less));
    }

    #[test]
    fn num_int_displays_without_a_decimal_point() {
        assert_eq!(Value::Int(5).to_string(), "5");
        assert_eq!(Value::Int(-42).to_string(), "-42");
        assert_eq!(Value::Int(0).to_string(), "0");
        // Debug carries the subtype tag.
        assert_eq!(format!("{:?}", Value::Int(7)), "Int(7)");
        assert_eq!(format!("{:?}", Value::Float(7.0)), "Float(7)");
    }

    #[test]
    fn num_truthiness_resolved_falsy_set() {
        // Falsy: nil, false, Int(0), 0.0/-0.0/NaN, 0m, "".
        assert!(!Value::Nil.is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Int(0).is_truthy());
        assert!(!Value::Float(0.0).is_truthy());
        assert!(!Value::Float(-0.0).is_truthy());
        assert!(!Value::Float(f64::NAN).is_truthy());
        assert!(!Value::Decimal(Rc::new(Decimal::ZERO)).is_truthy());
        assert!(!Value::Str("".into()).is_truthy());
        // Truthy: any non-zero number, non-empty string, EVERY collection even empty.
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Int(1).is_truthy());
        assert!(Value::Int(-1).is_truthy());
        assert!(Value::Float(0.5).is_truthy());
        assert!(Value::Float(f64::INFINITY).is_truthy());
        assert!(Value::Decimal(Rc::new(Decimal::from(1))).is_truthy());
        assert!(Value::Str("x".into()).is_truthy());
        assert!(Value::Array(crate::value::ArrayCell::new(vec![])).is_truthy());
        {
            use indexmap::IndexMap;
            assert!(Value::Map(crate::value::MapCell::new(IndexMap::new())).is_truthy());
            assert!(Value::Object(crate::value::ObjectCell::new(IndexMap::new())).is_truthy());
        }
    }

    #[test]
    fn num_mapkey_folds_integral_float_to_int() {
        // §3.3: an integral, in-range float is the SAME map key as the equal int.
        let from_int = MapKey::from_value(&Value::Int(1)).expect("int is hashable");
        let from_float = MapKey::from_value(&Value::Float(1.0)).expect("float is hashable");
        assert!(from_int == from_float, "Int(1) and Float(1.0) must share a key");
        // -0.0 folds to Int(0) and equals Int(0)/0.0.
        let neg_zero = MapKey::from_value(&Value::Float(-0.0)).expect("float is hashable");
        let pos_zero = MapKey::from_value(&Value::Float(0.0)).expect("float is hashable");
        let int_zero = MapKey::from_value(&Value::Int(0)).expect("int is hashable");
        assert!(neg_zero == pos_zero && pos_zero == int_zero);
        // A fractional float is a distinct (non-Int) key.
        let frac = MapKey::from_value(&Value::Float(1.5)).expect("float is hashable");
        assert!(frac != from_int);
        // Round-trips: Int key -> Value::Int.
        assert_eq!(from_int.to_value(), Value::Int(1));
    }

    #[test]
    fn num_mapkey_nan_carveout() {
        // §3.3: NaN is excluded from the "a==b ⟺ same key" claim. NaN keys
        // canonicalize to ONE storable key, but never equal a non-NaN key, and a
        // NaN float is NOT folded to any Int.
        let nan1 = MapKey::from_value(&Value::Float(f64::NAN)).expect("nan is hashable");
        let nan2 = MapKey::from_value(&Value::Float(f64::NAN)).expect("nan is hashable");
        // Two NaN keys canonicalize identically (storable/retrievable as one key).
        assert!(nan1 == nan2);
        // A NaN key never collides with any integer key (incl. 0).
        let zero = MapKey::from_value(&Value::Int(0)).expect("int is hashable");
        assert!(nan1 != zero);
        // The canonical NaN key is a `Num` (float) key, not an `Int` fold.
        assert!(matches!(nan1, MapKey::Num(_)));
    }

    #[test]
    fn num_cross_subtype_equality_is_exact() {
        // Int(1) == Float(1.0), symmetric.
        assert_eq!(Value::Int(1), Value::Float(1.0));
        assert_eq!(Value::Float(1.0), Value::Int(1));
        assert_eq!(Value::Int(0), Value::Float(-0.0));
        // Non-integral float is never equal to an int.
        assert_ne!(Value::Int(2), Value::Float(2.5));
        assert_ne!(Value::Float(2.5), Value::Int(2));
        // Exact (not lossy): 2^53+1 as int does NOT equal float(2^53) which rounds.
        let big = (1i64 << 53) + 1;
        assert_ne!(Value::Int(big), Value::Float(big as f64));
        // NaN/inf floats never equal any int.
        assert_ne!(Value::Int(0), Value::Float(f64::NAN));
        assert_ne!(Value::Int(0), Value::Float(f64::INFINITY));
        // Same-subtype equality still holds.
        assert_eq!(Value::Int(7), Value::Int(7));
        assert_ne!(Value::Int(7), Value::Int(8));
    }

    #[test]
    fn closure_behaves_like_an_anonymous_function() {
        use crate::vm::chunk::{Chunk, FnProto};
        use crate::vm::value_ext::Closure;

        let proto = Rc::new(FnProto {
            chunk: Chunk::new(),
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
            debug_name: None,
        });
        let a = Closure::new(proto);
        let cv = Value::Closure(a.clone());

        // Display mirrors an anonymous Function exactly.
        assert_eq!(cv.to_string(), "<function>");
        assert_eq!(Value::Function(anon_function()).to_string(), "<function>");

        // type() reports "function", like a Function.
        assert_eq!(crate::interp::type_name(&cv), "function");
        assert_eq!(
            crate::interp::type_name(&Value::Function(anon_function())),
            "function"
        );

        // Pointer identity: same Rc is equal; a distinct closure is not.
        assert_eq!(Value::Closure(a.clone()), Value::Closure(a.clone()));
        let b = Closure::new(Rc::new(FnProto {
            chunk: Chunk::new(),
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            is_worker: false,
            owning_class: None,
            params: Vec::new(),
            ret: None,
            local_names: Vec::new(),
            debug_name: None,
        }));
        assert_ne!(Value::Closure(a), Value::Closure(b));

        // Not a valid map key (mirrors Function).
        assert!(MapKey::from_value(&cv).is_none());

        // Truthy, like any callable.
        assert!(cv.is_truthy());
    }

    fn anon_function() -> Rc<Function> {
        Rc::new(Function {
            name: None,
            params: vec![],
            ret: None,
            body: vec![],
            closure: Environment::global(),
            is_async: false,
            is_generator: false,
            is_worker: false,
        })
    }

    #[test]
    fn objects_display_and_compare_by_identity() {
        use indexmap::IndexMap;
        let mut m = IndexMap::new();
        m.insert("a".to_string(), Value::Float(1.0));
        m.insert("b".to_string(), Value::Str("x".into()));
        let o = Value::Object(ObjectCell::new(m));
        assert_eq!(o.to_string(), "{a: 1.0, b: \"x\"}");
        assert_eq!(o.clone(), o);
        assert!(o.is_truthy());
    }

    // ---- SRV: the shared read-only heap (Task 1 — core value layer) ----

    fn shared_obj() -> Arc<SharedNode> {
        Arc::new(SharedNode::Object(Arc::new(vec![
            ("region".into(), Arc::new(SharedNode::Str("us".into()))),
            (
                "limits".into(),
                Arc::new(SharedNode::Array(Arc::from(vec![
                    Arc::new(SharedNode::Int(10)),
                    Arc::new(SharedNode::Int(100)),
                ]))),
            ),
        ])))
    }

    #[test]
    fn shared_node_is_send_sync() {
        fn is_send_sync<T: Send + Sync>() {}
        is_send_sync::<SharedNode>();
        is_send_sync::<Arc<SharedNode>>();
    }

    #[test]
    fn shared_frozen_helpers_report_underlying_kind() {
        let obj = Value::Shared(shared_obj());
        assert_eq!(frozen_kind(&obj), Some("object"));
        let arr = Value::Shared(Arc::new(SharedNode::Array(Arc::from(vec![Arc::new(
            SharedNode::Int(1),
        )]))));
        assert_eq!(frozen_kind(&arr), Some("array"));
        let map = Value::Shared(Arc::new(SharedNode::Map(Arc::new(vec![(
            SharedKey::Str("k".into()),
            Arc::new(SharedNode::Int(1)),
        )]))));
        assert_eq!(frozen_kind(&map), Some("map"));
        // A frozen SCALAR is not a mutable container → not a frozen-mutation target.
        let scalar = Value::Shared(Arc::new(SharedNode::Int(5)));
        assert_eq!(frozen_kind(&scalar), None);
        // But every Shared is frozen; freeze_value of it is a no-op.
        assert!(is_frozen_value(&obj));
        assert!(is_frozen_value(&scalar));
        freeze_value(&obj); // no panic, no change
        assert!(is_frozen_value(&obj));
    }

    #[test]
    fn shared_type_name_is_underlying_kind() {
        assert_eq!(
            crate::interp::type_name(&Value::Shared(shared_obj())),
            "object"
        );
        assert_eq!(
            crate::interp::type_name(&Value::Shared(Arc::new(SharedNode::Array(Arc::from(
                Vec::<Arc<SharedNode>>::new()
            ))))),
            "array"
        );
        assert_eq!(
            crate::interp::type_name(&Value::Shared(Arc::new(SharedNode::Str("x".into())))),
            "string"
        );
    }

    #[test]
    fn shared_is_truthy() {
        assert!(Value::Shared(shared_obj()).is_truthy());
        // Even a "scalar" frozen node is truthy as a Shared wrapper (it is a
        // container value to the user). Spec §3.5: a Shared is truthy.
        assert!(Value::Shared(Arc::new(SharedNode::Int(0))).is_truthy());
    }

    #[test]
    fn shared_equality_is_arc_identity() {
        let a = shared_obj();
        let v1 = Value::Shared(a.clone());
        let v2 = Value::Shared(a.clone()); // SAME Arc
        assert_eq!(v1, v2, "two clones of one Arc are equal (Arc identity)");
        // A structurally-identical but DISTINCT Arc is NOT equal.
        let other = Value::Shared(shared_obj());
        assert_ne!(v1, other, "distinct Arcs are not equal even if structural");
        // A Shared never equals a non-frozen container.
        use indexmap::IndexMap;
        let plain = Value::Object(ObjectCell::new(IndexMap::new()));
        assert_ne!(v1, plain);
    }

    #[test]
    fn shared_displays_like_underlying_kind() {
        let v = Value::Shared(shared_obj());
        assert_eq!(v.to_string(), "{region: \"us\", limits: [10, 100]}");
        assert_eq!(
            Value::Shared(Arc::new(SharedNode::Str("hi".into()))).to_string(),
            "hi"
        );
    }

    // ── SHAPE Task 1.1 ── ObjectCell accessor API ────────────────────────────
    // Helper: build a plain `ObjectCell` (not `Cc`-wrapped) from `&[(key, int)]`.
    // `ObjectCell::new` returns `Cc<ObjectCell>`; deref via `Cc::deref`.
    fn obj(pairs: &[(&str, i64)]) -> Cc<ObjectCell> {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), Value::Int(*v));
        }
        ObjectCell::new(m)
    }

    #[test]
    fn object_accessors_mirror_indexmap_semantics() {
        let mut m = IndexMap::new();
        m.insert("a".to_string(), Value::Int(1));
        m.insert("b".to_string(), Value::Int(2));
        let o = ObjectCell::new(m);
        assert_eq!(o.len(), 2);
        assert_eq!(o.get("a"), Some(Value::Int(1)));
        assert_eq!(o.get_index_of("b"), Some(1));
        o.insert("a", Value::Int(9)); // overwrite: position kept
        assert_eq!(
            o.get_index(0).map(|(k, _)| k.to_string()),
            Some("a".into())
        );
        o.insert("c", Value::Int(3)); // new key: appended
        let keys: Vec<String> = {
            let mut v = vec![];
            o.for_each(|k, _| v.push(k.to_string()));
            v
        };
        assert_eq!(keys, ["a", "b", "c"]);
        assert_eq!(o.shift_remove("b"), Some(Value::Int(2)));
        assert_eq!(o.get_index_of("c"), Some(1)); // order preserved after removal
    }

    #[test]
    fn object_content_eq_is_order_insensitive_like_indexmap_eq() {
        // replicates IndexMap::eq for the named-enum-payload comparison (value.rs:1447)
        let a = obj(&[("x", 1), ("y", 2)]);
        let b = obj(&[("y", 2), ("x", 1)]);
        assert!(a.content_eq(&b));
        assert!(!a.content_eq(&obj(&[("x", 1)])));
    }

    // ── SHAPE Task 2.2 — accessor battery on SLAB-mode cells ────────────────

    /// Build a slab `ObjectCell` by appending each key via the registry.
    fn slab_obj(pairs: &[(&str, i64)]) -> Cc<ObjectCell> {
        use crate::vm::shape::{ShapeRegistry, EMPTY_SHAPE};
        let mut reg = ShapeRegistry::new();
        let mut shape = EMPTY_SHAPE;
        for (k, _) in pairs {
            shape = reg.add_key(shape, k).expect("test slab: add_key");
        }
        let mut values = Vec::with_capacity(pairs.len());
        for (_, v) in pairs {
            values.push(Value::Int(*v));
        }
        ObjectCell::new_slab(reg.keys_of(shape), values, shape)
    }

    #[test]
    fn slab_mode_accessor_battery() {
        use crate::vm::shape::{ShapeRegistry, EMPTY_SHAPE};
        let mut reg = ShapeRegistry::new();

        // Build a 3-key slab: {a:1, b:2, c:3}
        let s_a = reg.add_key(EMPTY_SHAPE, "a").unwrap();
        let s_ab = reg.add_key(s_a, "b").unwrap();
        let s_abc = reg.add_key(s_ab, "c").unwrap();
        let cell = ObjectCell::new_slab(reg.keys_of(EMPTY_SHAPE), vec![], EMPTY_SHAPE);
        cell.slab_append(s_a, reg.keys_of(s_a), Value::Int(1));
        cell.slab_append(s_ab, reg.keys_of(s_ab), Value::Int(2));
        cell.slab_append(s_abc, reg.keys_of(s_abc), Value::Int(3));

        // len / is_empty
        assert_eq!(cell.len(), 3);
        assert!(!cell.is_empty());

        // get
        assert_eq!(cell.get("a"), Some(Value::Int(1)));
        assert_eq!(cell.get("b"), Some(Value::Int(2)));
        assert_eq!(cell.get("c"), Some(Value::Int(3)));
        assert_eq!(cell.get("z"), None);

        // contains_key
        assert!(cell.contains_key("a"));
        assert!(!cell.contains_key("z"));

        // get_index_of
        assert_eq!(cell.get_index_of("a"), Some(0));
        assert_eq!(cell.get_index_of("b"), Some(1));
        assert_eq!(cell.get_index_of("c"), Some(2));
        assert_eq!(cell.get_index_of("z"), None);

        // get_index
        assert_eq!(cell.get_index(0), Some((Rc::from("a"), Value::Int(1))));
        assert_eq!(cell.get_index(2), Some((Rc::from("c"), Value::Int(3))));
        assert_eq!(cell.get_index(3), None);

        // value_at
        assert_eq!(cell.value_at(0), Some(Value::Int(1)));
        assert_eq!(cell.value_at(3), None);

        // set_value_at
        assert!(cell.set_value_at(1, Value::Int(99)));
        assert_eq!(cell.value_at(1), Some(Value::Int(99)));
        assert!(!cell.set_value_at(5, Value::Int(0)));

        // entries order
        let entries: Vec<(String, i64)> = cell
            .entries()
            .into_iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    if let Value::Int(n) = v { n } else { panic!("expected Int") },
                )
            })
            .collect();
        assert_eq!(
            entries,
            [("a".to_string(), 1), ("b".to_string(), 99), ("c".to_string(), 3)]
        );

        // for_each order
        let mut keys = vec![];
        cell.for_each(|k, _| keys.push(k.to_string()));
        assert_eq!(keys, ["a", "b", "c"]);

        // try_for_each
        let mut keys2 = vec![];
        cell.try_for_each::<(), _>(|k, _| {
            keys2.push(k.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(keys2, ["a", "b", "c"]);

        // insert existing key — position kept, still in slab mode
        cell.insert("a", Value::Int(42));
        assert_eq!(cell.get_index_of("a"), Some(0)); // position 0 preserved
        assert_eq!(cell.value_at(0), Some(Value::Int(42)));
        // shape must be non-zero (still slab)
        assert_ne!(cell.shape.get(), 0);

        // insert new key on slab — demotes to dict
        cell.insert("d", Value::Int(4));
        assert_eq!(cell.shape.get(), 0); // demoted → shape 0
        assert_eq!(cell.get("d"), Some(Value::Int(4)));

        // keys_snapshot (post-demotion, dict mode)
        let ks = cell.keys_snapshot();
        assert_eq!(ks, ["a", "b", "c", "d"]);

        // to_index_map
        let m = cell.to_index_map();
        assert_eq!(m.len(), 4);
    }

    #[test]
    fn slab_shift_remove_demotes() {
        let o = slab_obj(&[("x", 10), ("y", 20), ("z", 30)]);
        assert_ne!(o.shape.get(), 0); // starts in slab mode
        let removed = o.shift_remove("x");
        assert_eq!(removed, Some(Value::Int(10)));
        assert_eq!(o.shape.get(), 0); // demoted
        // remaining order: y, z
        let mut keys = vec![];
        o.for_each(|k, _| keys.push(k.to_string()));
        assert_eq!(keys, ["y", "z"]);
        assert_eq!(o.get_index_of("y"), Some(0));
        assert_eq!(o.get_index_of("z"), Some(1));
    }

    #[test]
    fn slab_content_eq_across_modes() {
        let slab = slab_obj(&[("a", 1), ("b", 2)]);
        // dict with same content, different insertion order → content_eq is order-insensitive
        let mut m = IndexMap::new();
        m.insert("b".to_string(), Value::Int(2));
        m.insert("a".to_string(), Value::Int(1));
        let dict = ObjectCell::new(m);
        assert!(slab.content_eq(&dict));
        assert!(dict.content_eq(&slab));
    }

    #[test]
    fn demote_to_dict_is_noop_on_dict() {
        let o = obj(&[("a", 1), ("b", 2)]);
        assert_eq!(o.shape.get(), 0);
        o.demote_to_dict(); // no-op
        assert_eq!(o.len(), 2);
        assert_eq!(o.shape.get(), 0);
    }
}
