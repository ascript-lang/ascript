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

// ---------------------------------------------------------------------------
// REGION §3 — the proven-dead `ObjectCell` recycler (the `region-spike` engine
// change the Phase-2 A/B measures).
//
// The pool captures dead object cells at flagged kill sites (`SetLocal`
// overwrite / `Pop`, selected by `region_candidates`) when the dying value is a
// uniquely-owned `Value::Object` (`strong_count() == 1` — the runtime deadness
// proof), and hands them back at `Op::NewObject` instead of `Cc::new`. Pooled
// cells stay in gcmodule's object space (their `Trace` visits an emptied
// container → nothing), capacity-only, bounded by `cap`. NO unsafe.
//
// Soundness rests ENTIRELY on the `strong_count() == 1` guard, never on the
// static analysis (spec §3.3): a self-edge / live alias has count ≥ 2 → the
// check misses → the normal drop path → the cycle collector reclaims as today.
// ---------------------------------------------------------------------------

#[cfg(feature = "region-spike")]
pub use spike::{RegionPool, RegionScope, RegionStats};

#[cfg(feature = "region-spike")]
mod spike {
    use crate::value::{ObjectCell, OwnedKind, Value};
    use gcmodule::Cc;
    use std::cell::Cell;

    /// Default per-`Vm` pool capacity (spec §2.4). Overridable via
    /// `ASCRIPT_REGION_POOL_CAP`; a recycle into a full pool falls through to the
    /// normal drop (overflow stat bumped). Bounds memory held across a task's awaits.
    pub const DEFAULT_REGION_POOL_CAP: usize = 256;

    /// Read `ASCRIPT_REGION_POOL_CAP` (parseable, > 0) or the default. Resolved once
    /// at pool construction so the env is not read on the hot path.
    fn resolved_cap() -> usize {
        std::env::var("ASCRIPT_REGION_POOL_CAP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&c| c > 0)
            .unwrap_or(DEFAULT_REGION_POOL_CAP)
    }

    /// REGION §6.7 / Gate-18 coverage counters. `Cell<u64>` so a `&self` pool can
    /// bump them on the synchronous kill/alloc paths (never across `.await`).
    /// Asserted in the coverage tests (anti-false-green, Gate 15).
    #[derive(Default)]
    pub struct RegionStats {
        /// Cells captured into the pool at a kill site (a successful recycle).
        recycled: Cell<u64>,
        /// Cells handed back at `Op::NewObject` (a successful reuse).
        reused: Cell<u64>,
        /// Recycle attempts refused because the pool was full (the value was dropped
        /// normally). A growing overflow is the §2.4 memory-pressure signal.
        overflow: Cell<u64>,
        /// Kill-site checks that found the dying value NOT uniquely owned
        /// (`strong_count() != 1`) or not a `Value::Object` — the normal drop path.
        miss: Cell<u64>,
    }

    impl RegionStats {
        pub fn recycled(&self) -> u64 {
            self.recycled.get()
        }
        pub fn reused(&self) -> u64 {
            self.reused.get()
        }
        pub fn overflow(&self) -> u64 {
            self.overflow.get()
        }
        pub fn miss(&self) -> u64 {
            self.miss.get()
        }
        #[inline]
        fn bump(c: &Cell<u64>) {
            c.set(c.get() + 1);
        }
    }

    /// Per-`Vm` (per-isolate) pool of proven-dead `Cc<ObjectCell>`s (spec §3.1).
    pub struct RegionPool {
        /// The recycled cells. A `Vec` used as a LIFO stack (the most-recently-freed
        /// cell is the hottest in cache). Bounded by `cap`.
        objects: Vec<Cc<ObjectCell>>,
        /// Maximum pooled cells (`ASCRIPT_REGION_POOL_CAP`, default 256).
        cap: usize,
        /// Gate-18 stats.
        pub stats: RegionStats,
    }

    impl Default for RegionPool {
        fn default() -> Self {
            Self::new()
        }
    }

    impl RegionPool {
        /// A fresh empty pool with the env-resolved cap.
        pub fn new() -> Self {
            RegionPool {
                objects: Vec::new(),
                cap: resolved_cap(),
                stats: RegionStats::default(),
            }
        }

        /// The number of currently-pooled cells (test/inspection only).
        pub fn len(&self) -> usize {
            self.objects.len()
        }

        /// `true` when the pool holds no cells.
        pub fn is_empty(&self) -> bool {
            self.objects.is_empty()
        }

        /// The configured cap.
        pub fn cap(&self) -> usize {
            self.cap
        }

        /// KILL-SITE path (spec §3.1/§3.3). `dying` is the value just removed from
        /// its slot/stack at a flagged kill offset. Returns `Some(value)` to hand
        /// back if NOT recyclable (the caller drops it normally); returns `None`
        /// when the cell was captured into the pool (the caller does nothing).
        ///
        /// Recyclable iff: `dying` is a `Value::Object`, its `Cc` is uniquely owned
        /// (`strong_count() == 1` — the deadness proof: no live alias, no container
        /// edge, no upvalue, no Rust temporary holds it), AND the pool has room.
        /// On success the cell is emptied IN PLACE (capacity retained — the win) and
        /// pushed.
        pub fn try_recycle(&mut self, dying: Value) -> Option<Value> {
            // Borrow the Cc to probe the count WITHOUT consuming `dying` (so a miss
            // hands the original value back unmoved).
            {
                let crate::value::ValueKind::Object(cc) = dying.kind() else {
                    RegionStats::bump(&self.stats.miss);
                    return Some(dying);
                };
                if super::object_cell_strong_count(cc) != 1 {
                    RegionStats::bump(&self.stats.miss);
                    return Some(dying);
                }
            }
            if self.objects.len() >= self.cap {
                RegionStats::bump(&self.stats.overflow);
                return Some(dying);
            }
            // Proven dead + room: take the owned Cc and pool it.
            let OwnedKind::Object(cc) = dying.into_kind() else {
                // Unreachable — `kind()` above already proved Object; keep total.
                unreachable!("region try_recycle: kind() said Object but into_kind() did not");
            };
            cc.region_clear_for_pool();
            self.objects.push(cc);
            RegionStats::bump(&self.stats.recycled);
            None
        }

        /// ALLOC-SITE path (spec §3.1). `Op::NewObject` in region mode pops a pooled
        /// cell if available. The CALLER then resets it (`region_reset_to_slab` /
        /// `region_reset_to_dict`) onto the SAME shape verdict a fresh cell takes,
        /// so the reused cell is byte-identical to a freshly-allocated one (no
        /// shape-id staleness). Returns `None` when the pool is empty (the caller
        /// falls back to `Cc::new`).
        pub fn take_object(&mut self) -> Option<Cc<ObjectCell>> {
            let cc = self.objects.pop()?;
            RegionStats::bump(&self.stats.reused);
            Some(cc)
        }

        /// TASK-END trim (spec §2.4) — release the pool down to a floor (`cap / 8`)
        /// so a long-lived/finished task does not pin the full capacity. Called by
        /// [`RegionScope::drop`]. Dropped cells fall to the normal refcount/collector
        /// reclaim path.
        pub fn trim(&mut self) {
            self.objects.truncate(self.cap / 8);
        }
    }

    /// RAII task-region guard (spec §3.4). Created at each `spawn_local` task body
    /// and the `http.serve` per-request handler; its `Drop` trims the per-`Vm` pool
    /// toward a floor (memory bounding — NOT correctness; recycled cells are proven
    /// dead, so cross-task reuse within an isolate is harmless). Cancel-on-drop tasks
    /// trim via this same `Drop` (abort runs destructors).
    ///
    /// Holds a `std::rc::Weak<crate::vm::Vm>` (NOT a strong `Rc`) so the guard never
    /// keeps the `Vm` alive past its own lifetime; the trim is a no-op if the `Vm` is
    /// already gone. A plain owned value across the body's `.await`s — no `RefCell`
    /// borrow is held, so the no-borrow-across-await invariant is untouched.
    pub struct RegionScope {
        vm: std::rc::Weak<crate::vm::Vm>,
    }

    impl RegionScope {
        /// Bracket a task with a region-trim guard. `vm` is held weakly.
        pub fn new(vm: std::rc::Weak<crate::vm::Vm>) -> Self {
            RegionScope { vm }
        }
    }

    impl Drop for RegionScope {
        fn drop(&mut self) {
            if let Some(vm) = self.vm.upgrade() {
                vm.region_trim();
            }
        }
    }
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

    // Task 1.3: the self-referential kill-point case (`obj.me = obj` → strong_count
    // >= 2 at the would-be kill point, a self-cycle that MUST NOT be recycled) is
    // exercised end-to-end through the engine in `tests/region.rs`
    // (`self_referential_object_is_not_recycled`), where the VM/value-mutation
    // machinery is available. The count-semantics pins above are the accessor-level
    // ground truth that test builds on.
}
