//! Polymorphic inline caches (V11-T3) for shaped property access.
//!
//! An *inline cache* (IC) memoizes the result of resolving a property name on a
//! receiver of a particular SHAPE (V11-T2 hidden classes). The generic lookup
//! `IndexMap::get_index_of(name) -> i` is O(1) but still hashes the key; an IC
//! collapses that to a single `recv.shape == cached_shape` integer compare plus a
//! direct `values.get_index(cached_index)`, which is the fast path the perf slice
//! (V11-T6) relies on.
//!
//! The cache adapts through four states as it observes more shapes at one call
//! site (classic monomorphic → polymorphic → megamorphic progression):
//!
//! - [`InlineCache::Cold`] — never executed; the first hit records a [`Mono`].
//! - [`InlineCache::Mono`] — exactly one `(shape, index)` seen. The hot case.
//! - [`InlineCache::Poly`] — 2..=[`POLY_MAX`] distinct shapes, linearly scanned.
//! - [`InlineCache::Mega`] — too many shapes; the IC gives up and the call site
//!   always takes the generic path (recording would only thrash the cache).
//!
//! [`Mono`]: InlineCache::Mono
//!
//! ## Correctness invariants
//!
//! The IC is ONLY ever a fast path IN FRONT of the existing generic lookup; it
//! must produce a result byte-identical to that generic path for every input or
//! it is a bug (the V11 three-way differential enforces this). To that end:
//!
//! - The cache stores a `(shape, index)` pair where `index` is the receiver's
//!   stable `IndexMap` position for that name. Because a shape uniquely identifies
//!   the ordered key layout (V11-T2), `(shape, name)` always maps to the same
//!   index — so caching the index keyed by shape is sound. A mutation that ADDS a
//!   key transitions the shape (V11-T2), so the next access sees a different shape
//!   and MISSES the cache (re-resolving); reassigning an existing key keeps the
//!   shape and the cached index stays valid.
//! - A NAME THAT IS NOT A FIELD never enters the cache: `record` is only called
//!   on a successful field resolution (`get_index_of` returned `Some`). A
//!   method-named access on an instance returns `None` from the field lookup, so
//!   the IC stays cold/unchanged and the call falls through to the generic
//!   member-read (which finds the method → bound method). The IC can therefore
//!   NEVER return a wrong value for a method-named access.
//! - Receivers WITHOUT a shaped layout (modules, enums, strings, schema values,
//!   nil) never reach the IC fast path — the run loop guards those before
//!   consulting the cache.

/// Maximum number of distinct shapes a [`InlineCache::Poly`] site tracks before
/// it degrades to [`InlineCache::Mega`]. Four matches the V11 plan (a typical
/// polymorphic call site sees a handful of shapes; beyond that linear scan stops
/// paying off).
pub const POLY_MAX: usize = 4;

/// One inline-cache slot: the memoized shape→index mapping for a single
/// `GET_PROP`/`SET_PROP`/`CALL_METHOD` call site. `!Send` by construction (it
/// lives behind the chunk's `RefCell`, never shared across threads). See the
/// module docs for the state machine and correctness invariants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InlineCache {
    /// Never executed. The first resolution records a [`InlineCache::Mono`].
    #[default]
    Cold,
    /// Exactly one shape seen so far: `recv.shape == shape` ⇒ field at `index`.
    Mono { shape: u32, index: u32 },
    /// 2..=[`POLY_MAX`] distinct shapes, scanned linearly. `len` entries of
    /// `entries` are live (the rest are padding and never read).
    Poly {
        entries: [(u32, u32); POLY_MAX],
        len: u8,
    },
    /// Saturated: too many shapes; always take the generic path, never record.
    Mega,
}

impl InlineCache {
    /// The cached field index for `shape`, or `None` on a miss (cold, a different
    /// mono shape, an unseen poly shape, or mega). A `Some` result is the SAME
    /// index the generic `get_index_of` would return for the field name at this
    /// call site — the caller may read it directly.
    #[inline]
    pub fn lookup(&self, shape: u32) -> Option<u32> {
        match self {
            InlineCache::Cold | InlineCache::Mega => None,
            InlineCache::Mono { shape: s, index } => (*s == shape).then_some(*index),
            InlineCache::Poly { entries, len } => entries[..*len as usize]
                .iter()
                .find(|(s, _)| *s == shape)
                .map(|(_, i)| *i),
        }
    }

    /// Record that `shape` resolved field `name` to `index`, advancing the cache
    /// state (Cold→Mono→Poly→Mega). Idempotent for an already-cached shape (it
    /// re-confirms the same index). Called ONLY after a successful generic FIELD
    /// resolution, so it never caches a non-field name.
    pub fn record(&mut self, shape: u32, index: u32) {
        match self {
            InlineCache::Cold => {
                *self = InlineCache::Mono { shape, index };
            }
            InlineCache::Mono { shape: s, index: i } => {
                if *s == shape {
                    // Same shape re-seen: index is stable, nothing changes.
                    *i = index;
                } else {
                    // A second distinct shape promotes mono → poly.
                    let mut entries = [(0u32, 0u32); POLY_MAX];
                    entries[0] = (*s, *i);
                    entries[1] = (shape, index);
                    *self = InlineCache::Poly { entries, len: 2 };
                }
            }
            InlineCache::Poly { entries, len } => {
                let n = *len as usize;
                if let Some(slot) = entries[..n].iter_mut().find(|(s, _)| *s == shape) {
                    slot.1 = index; // already tracked; refresh index (stable).
                } else if n < POLY_MAX {
                    entries[n] = (shape, index);
                    *len = (n + 1) as u8;
                } else {
                    // A (POLY_MAX+1)-th distinct shape saturates the cache.
                    *self = InlineCache::Mega;
                }
            }
            InlineCache::Mega => { /* saturated: never record again. */ }
        }
    }
}

/// One method-dispatch inline-cache slot for `CALL_METHOD`. Caches, per receiver
/// CLASS IDENTITY (the `Rc<Class>` pointer — NOT the field-layout shape, since two
/// distinct classes can share a field layout, and method resolution is per-class),
/// the resolved compiled method closure plus its defining class — so a hit skips
/// the `find_compiled_method` chain walk. A miss (different class, or a name not
/// resolvable to a compiled method) falls to the generic dispatch.
///
/// This is deliberately MONOMORPHIC: a method call site usually sees one class.
/// On a class change it simply re-resolves and replaces the entry (cheap, and it
/// keeps the cache tiny). Correctness: the cached closure is exactly what
/// `find_compiled_method(class, name)` returns, so a hit is behavior-identical to
/// the generic walk; a `CALL_METHOD` whose receiver is not a VM instance (schema
/// value, non-instance) never reaches this cache, and a name SHADOWED by an
/// instance field is re-checked by the caller (a field wins over a method).
#[derive(Clone, Default)]
pub enum MethodCache {
    /// Never executed, or invalidated.
    #[default]
    Cold,
    /// One receiver class seen: `Rc::as_ptr(class) as usize == class_id` ⇒ this
    /// compiled method (resolved up the chain) and its defining class.
    Mono {
        class_id: usize,
        closure: gcmodule::Cc<crate::vm::value_ext::Closure>,
        defining_class: std::rc::Rc<crate::value::Class>,
    },
}

impl std::fmt::Debug for MethodCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MethodCache::Cold => f.write_str("MethodCache::Cold"),
            MethodCache::Mono {
                class_id,
                defining_class,
                ..
            } => f
                .debug_struct("MethodCache::Mono")
                .field("class_id", class_id)
                .field("class", &defining_class.name)
                .finish(),
        }
    }
}

impl MethodCache {
    /// The cached `(closure, defining_class)` for `class_id`, or `None` on a miss.
    #[inline]
    pub fn lookup(
        &self,
        class_id: usize,
    ) -> Option<(
        gcmodule::Cc<crate::vm::value_ext::Closure>,
        std::rc::Rc<crate::value::Class>,
    )> {
        match self {
            MethodCache::Cold => None,
            MethodCache::Mono {
                class_id: c,
                closure,
                defining_class,
            } => (*c == class_id).then(|| (closure.clone(), defining_class.clone())),
        }
    }

    /// Record the resolved compiled method for `class_id`, replacing any prior
    /// entry.
    pub fn record(
        &mut self,
        class_id: usize,
        closure: gcmodule::Cc<crate::vm::value_ext::Closure>,
        defining_class: std::rc::Rc<crate::value::Class>,
    ) {
        *self = MethodCache::Mono {
            class_id,
            closure,
            defining_class,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_misses() {
        let ic = InlineCache::Cold;
        assert_eq!(ic.lookup(0), None);
        assert_eq!(ic.lookup(7), None);
    }

    #[test]
    fn cold_to_mono_records_and_hits() {
        let mut ic = InlineCache::Cold;
        ic.record(3, 1);
        assert_eq!(ic, InlineCache::Mono { shape: 3, index: 1 });
        assert_eq!(ic.lookup(3), Some(1));
        // A different shape misses while still mono.
        assert_eq!(ic.lookup(4), None);
    }

    #[test]
    fn mono_same_shape_is_stable() {
        let mut ic = InlineCache::Cold;
        ic.record(3, 1);
        ic.record(3, 1);
        assert_eq!(ic, InlineCache::Mono { shape: 3, index: 1 });
    }

    #[test]
    fn mono_to_poly_on_second_shape() {
        let mut ic = InlineCache::Cold;
        ic.record(3, 0); // mono(3)
        ic.record(4, 1); // poly[3,4]
        match ic {
            InlineCache::Poly { len, .. } => assert_eq!(len, 2),
            other => panic!("expected poly, got {other:?}"),
        }
        assert_eq!(ic.lookup(3), Some(0));
        assert_eq!(ic.lookup(4), Some(1));
        assert_eq!(ic.lookup(5), None);
    }

    #[test]
    fn poly_fills_to_four_then_megamorphic_on_fifth() {
        let mut ic = InlineCache::Cold;
        // Five distinct shapes: cold→mono→poly(2,3,4)→mega.
        for (shape, index) in [(10, 0), (11, 1), (12, 2), (13, 3)] {
            ic.record(shape, index);
        }
        match ic {
            InlineCache::Poly { len, .. } => assert_eq!(len, POLY_MAX as u8),
            other => panic!("expected full poly, got {other:?}"),
        }
        // All four still hit.
        assert_eq!(ic.lookup(10), Some(0));
        assert_eq!(ic.lookup(13), Some(3));
        // The fifth distinct shape saturates to mega.
        ic.record(14, 4);
        assert_eq!(ic, InlineCache::Mega);
        // Mega never hits, even for a previously-cached shape.
        assert_eq!(ic.lookup(10), None);
        assert_eq!(ic.lookup(14), None);
    }

    #[test]
    fn mega_never_records() {
        let mut ic = InlineCache::Mega;
        ic.record(1, 1);
        assert_eq!(ic, InlineCache::Mega);
    }

    #[test]
    fn poly_reseen_shape_refreshes_index_no_growth() {
        let mut ic = InlineCache::Cold;
        ic.record(3, 0);
        ic.record(4, 1); // poly len 2
        ic.record(3, 0); // re-seen, stays len 2
        match ic {
            InlineCache::Poly { len, .. } => assert_eq!(len, 2),
            other => panic!("expected poly len 2, got {other:?}"),
        }
    }
}
