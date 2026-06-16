//! REGION — kill-site strong-count accessor + pin tests (spec §3.3).
//!
//! The REGION spike recycles a dead heap object in place at a `NewObject` kill
//! site. The deadness proof is a strong-reference-count check on the object's
//! tracked `Cc<ObjectCell>`: a count of `1` means this `Cc` is the SOLE owner —
//! no live alias escaped — so the storage can be reused instead of freshly
//! allocated.
//!
//! gcmodule 0.3.3 keeps `Cc::ref_count()` `pub(crate)`, with no public path from
//! a `Cc<T>` handle to its strong count. This branch vendors gcmodule LOCALLY
//! (`vendor/gcmodule`, wired via `[patch.crates-io]`) with ONE added public
//! accessor, [`gcmodule::Cc::strong_count`]. This module re-exports it and pins
//! its semantics on a TRACKED `Cc<ObjectCell>` so the spike builds on a verified
//! count, not an assumption.
//!
//! G6 NOTE (spec): the PRODUCTION dependency decision (upstream PR vs official
//! vendor) is DEFERRED to a REGION GO outcome (Phase 3). This accessor is
//! spike-local and does not ship.

/// Read the strong reference count of a tracked object cell at a candidate kill
/// site. A return of `1` is the deadness proof: this `Cc` is the only owner.
///
/// Thin wrapper over the vendor-local [`gcmodule::Cc::strong_count`] so the
/// dependency seam has a single named entry point in the engine (the spike calls
/// this, not the raw gcmodule method).
#[allow(dead_code)]
#[inline]
pub fn object_cell_strong_count(cell: &gcmodule::Cc<crate::value::ObjectCell>) -> usize {
    cell.strong_count()
}

#[cfg(test)]
mod tests {
    use super::object_cell_strong_count;
    use crate::value::ObjectCell;
    use gcmodule::Cc;
    use indexmap::IndexMap;

    /// A fresh, distinct, TRACKED `Cc<ObjectCell>` (shape 0 / dict-mode is fine —
    /// the count semantics are independent of storage mode). `ObjectCell::new`
    /// goes through `Cc::new`, and `ObjectCell::is_type_tracked() == true`, so the
    /// resulting `Cc` is GC-tracked (carries a `GcHeader`) — the exact handle the
    /// kill-site sees.
    fn fresh_tracked_cell() -> Cc<ObjectCell> {
        ObjectCell::new(IndexMap::new())
    }

    /// The load-bearing pin: 1-at-birth / 2-after-clone / 1-after-drop on a
    /// TRACKED `Cc<ObjectCell>`. This VERIFIES (does not assume) the count
    /// machinery the deadness proof relies on. gcmodule debug-asserts 1-at-birth
    /// internally; here we also prove the clone-bump and drop-decrement that the
    /// kill site cares about, and that the tracked metadata bits do not leak into
    /// the strong count.
    #[test]
    fn strong_count_birth_clone_drop_on_tracked_object_cell() {
        let cell = fresh_tracked_cell();
        assert_eq!(
            object_cell_strong_count(&cell),
            1,
            "a fresh tracked Cc<ObjectCell> must have strong_count 1 (sole owner — the kill-site deadness verdict)",
        );

        let alias = cell.clone();
        assert_eq!(
            object_cell_strong_count(&cell),
            2,
            "cloning a Cc<ObjectCell> must bump the strong count to 2 (a live alias — NOT dead)",
        );
        // The clone observes the same shared count.
        assert_eq!(object_cell_strong_count(&alias), 2);

        drop(alias);
        assert_eq!(
            object_cell_strong_count(&cell),
            1,
            "dropping the alias must return the strong count to 1 (sole owner again — recyclable)",
        );
    }

    /// Independent handles do not share a count — guards against a global/static
    /// counter confusion (each `Cc<ObjectCell>` owns its own object's count).
    #[test]
    fn strong_count_is_per_object_not_global() {
        let a = fresh_tracked_cell();
        let b = fresh_tracked_cell();
        assert_eq!(object_cell_strong_count(&a), 1);
        assert_eq!(object_cell_strong_count(&b), 1);

        let a2 = a.clone();
        assert_eq!(object_cell_strong_count(&a), 2);
        // Cloning `a` must NOT affect an unrelated cell `b`.
        assert_eq!(object_cell_strong_count(&b), 1);
        drop(a2);
        assert_eq!(object_cell_strong_count(&a), 1);
    }

    // TODO (Task 1.3): the self-referential kill-point case — build `obj.me = obj`
    // through the engine so the cell holds a Value::Object edge back to itself,
    // then assert strong_count >= 2 at the would-be kill point (a self-cycle is a
    // live alias and MUST NOT be recycled). Deferred to Task 1.3: constructing a
    // cyclic object via the engine needs VM/value-mutation machinery not wired in
    // this accessor-only task. Not load-bearing for the count-semantics pin here.
}
