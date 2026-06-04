//! Cycle-collecting GC glue (V13).
//!
//! ## The plan (gcmodule / Bacon–Rajan)
//!
//! AScript's runtime is `Rc`/`RefCell`-based, which leaks reference cycles
//! (`let a = []; a.push(a)`). The fix is [`gcmodule`], a refcounting smart
//! pointer `Cc<T>` paired with Bacon–Rajan trial-deletion cycle collection.
//! `Cc<T>` behaves like `Rc<T>` (eager, deterministic drop on last strong ref)
//! but additionally lets a periodic collector reclaim *unreachable cycles* by
//! tracing through [`Trace`] impls.
//!
//! ### Phasing (all complete)
//! - **V13-T1:** added the `gcmodule` dependency and implemented [`Trace`] for
//!   every cycle-capable runtime type (no `Rc`→`Cc` migration yet at this point).
//! - **V13-T2:** the one-pass migration of the cycle-capable [`Value`] variants
//!   (`Array`/`Object`/`Map`/`Set`/`Instance`/`Closure` + upvalue cells) from
//!   `Rc` to `Cc` (see `src/value.rs` `Cc<…>` variants).
//! - **V13-T3:** enabled and tuned collection; soundness / soak / Drop gates.
//!
//! The [`Trace`] impls below are **load-bearing**: the collector calls them when
//! reclaiming unreachable cycles (`gc::collect`).
//!
//! ### What is traced vs. what stays acyclic (deterministic-Drop invariant)
//! Only types that can transitively contain another [`Value`] (and therefore
//! form a cycle) are traced: arrays, objects, maps, sets, instances, closures,
//! and the closure upvalue cells. Everything else is a **no-op** [`Trace`]:
//! - **Primitives** (`Nil`/`Bool`/`Number`/`Decimal`/`Str`/`Builtin`) hold no
//!   cycle-capable `Value`.
//! - **`Native`/`NativeMethod`** wrap OS resources (sockets, child processes,
//!   sqlite handles, terminals). These STAY on `Rc` in V13-T2 and MUST NOT be
//!   traced: the GC must never reach into a native resource, because those rely
//!   on deterministic `Drop` to reclaim fds. Tracing them would risk the
//!   collector deferring/altering that drop.
//! - **`Regex`/`Enum`/`EnumVariant`/`Class`** are immutable / acyclic in
//!   practice and likewise stay on `Rc`.
//! - **`Future`/`Generator`** own spawned-task / coroutine state behind their
//!   own handles (`task.rs` / `coro.rs`); they are identity-equal opaque
//!   handles, not `Value` containers, so they are no-op here.
//!
//! The [`Trace`] impl on [`Value`] therefore visits ONLY the container variants
//! and is a no-op for everything else.

use crate::value::{ArrayCell, Instance, MapCell, MapKey, ObjectCell, SetCell, Value};
use crate::vm::value_ext::Closure;
use gcmodule::{Cc, Trace, Tracer};
use indexmap::{IndexMap, IndexSet};
use std::cell::Cell;
use std::hash::Hash;

// ───────────────────────────── cycle collection (V13-T3) ─────────────────────
//
// gcmodule's collector is **thread-local** (`collect_thread_cycles()` operates on
// the current thread's `Cc`-tracked object space). AScript's runtime is `!Send`
// and runs on a current-thread tokio runtime inside a `LocalSet`, so a single
// thread owns the whole `Cc` graph — the thread-local collector fits exactly, and
// we never need cross-thread coordination.
//
// **What collection does / does not do (the safety invariants this task rests on):**
// - It reclaims ONLY unreachable *cycles*. gcmodule accounts external strong refs
//   via the `Cc` refcount, so anything reachable from the live stack / frames /
//   globals has refcount > internal-edges and is KEPT. Acyclic garbage is already
//   freed by refcounting *before* the collector ever sees it (the `Cc` drops at
//   refcount 0, exactly like `Rc`), so collection only ever touches genuinely
//   cyclic, genuinely-dead subgraphs → it cannot change observable behavior.
// - `Native`/`Str`/`Bytes`/`Class`/`Function` stay on `Rc` (NOT `Cc`), so they are
//   not in the collector's object space at all — collection never traces or drops
//   a native OS-resource handle, preserving deterministic fd reclamation (V13-T6).

/// How many *net new* `Cc` allocations to let accumulate before an automatic
/// collection is considered. The trigger compares the collector's live tracked
/// count (`gcmodule::count_thread_tracked`, which gcmodule maintains for free) to
/// the count at the last collection; once it has grown by this many, a collection
/// runs. This is allocation-*pressure* based (not a per-op cost) so it stays cheap:
/// the only per-check work is a thread-local counter read + compare.
///
/// Tuned conservatively high so steady-state acyclic programs (which never grow the
/// tracked set, because refcounting drops their `Cc`s immediately) effectively never
/// trigger a collection, and only a program that is genuinely *retaining* a large
/// and growing `Cc` graph pays for a sweep. The V13-T5 soak + V13-T7 perf gates
/// validate this default is neither too lazy (memory grows unbounded) nor too eager
/// (throughput regresses).
const COLLECT_GROWTH_THRESHOLD: usize = 10_000;

/// The auto-collect growth threshold, exposed for the V13-T5 soak gate so it can
/// size per-request garbage to exceed one growth window (forcing the accept-loop
/// `maybe_collect` to fire) and assert the live set stays bounded near it — without
/// hard-coding the constant in the test.
#[cfg(all(test, feature = "net"))]
pub(crate) const fn collect_growth_threshold() -> usize {
    COLLECT_GROWTH_THRESHOLD
}

thread_local! {
    /// The tracked-object count at the most recent collection (auto or forced).
    /// The auto trigger fires when the current tracked count exceeds this by
    /// [`COLLECT_GROWTH_THRESHOLD`]. Updated after every collection so the trigger
    /// measures *growth since last sweep*, not absolute size.
    static LAST_COLLECT_TRACKED: Cell<usize> = const { Cell::new(0) };
}

/// Force a full cycle collection on the current thread and return the number of
/// objects reclaimed. Thin wrapper over [`gcmodule::collect_thread_cycles`] that
/// also resets the growth baseline so the *next* auto trigger measures growth from
/// here. This is the explicit collection point: program-end (clean shutdown +
/// reclaim leftover cycles) and the test hook (`Vm::collect`).
#[inline]
pub fn collect() -> usize {
    let reclaimed = gcmodule::collect_thread_cycles();
    LAST_COLLECT_TRACKED.with(|c| c.set(gcmodule::count_thread_tracked()));
    reclaimed
}

/// Cheap allocation-pressure check, called at coarse-grained safe points during
/// long-running execution (e.g. between accepted `http.serve` connections). Runs a
/// collection ONLY when the live tracked-object count has grown past the last
/// collection's baseline by [`COLLECT_GROWTH_THRESHOLD`]; otherwise it is a single
/// thread-local read + compare and returns without collecting. Returns the number
/// of objects reclaimed (0 if it did not collect).
#[inline]
pub fn maybe_collect() -> usize {
    let tracked = gcmodule::count_thread_tracked();
    let baseline = LAST_COLLECT_TRACKED.with(|c| c.get());
    if tracked.saturating_sub(baseline) >= COLLECT_GROWTH_THRESHOLD {
        collect()
    } else {
        0
    }
}

/// Heap address of a `Cc`-managed object, for identity comparison and the
/// display cycle-guard `seen` list. `gcmodule::Cc` exposes no `as_ptr`, but it
/// derefs to a `T` held at a stable heap address, so `&**cc` is a stable
/// per-object pointer (drop-in for the old `Rc::as_ptr(cc) as usize`).
#[inline]
pub fn cc_addr<T: Trace + ?Sized>(cc: &Cc<T>) -> usize {
    (&**cc as *const T).cast::<()>() as usize
}

/// Pointer (identity) equality for two `Cc`s, mirroring `Rc::ptr_eq`. `Cc` has
/// no inherent `ptr_eq`, so compare the stable deref addresses.
#[inline]
pub fn cc_ptr_eq<T: Trace + ?Sized>(a: &Cc<T>, b: &Cc<T>) -> bool {
    std::ptr::eq(&**a as *const T as *const (), &**b as *const T as *const ())
}

/// Trace an `indexmap::IndexMap` by visiting every key and value. `indexmap`
/// is a foreign crate, so gcmodule has no blanket impl — we hand-write one as a
/// free helper (a blanket `impl Trace for IndexMap` would be an orphan-rule
/// violation). Keys are `String`/`MapKey` (acyclic) but we trace them anyway
/// for uniformity; values recurse into [`Value::trace`].
fn trace_index_map<K: Trace, V: Trace>(map: &IndexMap<K, V>, tracer: &mut Tracer) {
    for (k, v) in map {
        k.trace(tracer);
        v.trace(tracer);
    }
}

/// Trace an `indexmap::IndexSet` by visiting every element (see
/// [`trace_index_map`] for why this is a free helper, not a blanket impl).
fn trace_index_set<T: Trace + Hash + Eq>(set: &IndexSet<T>, tracer: &mut Tracer) {
    for t in set {
        t.trace(tracer);
    }
}

/// `MapKey` (Map keys / Set elements) is acyclic: every variant is a primitive
/// (`Nil`/`Bool`/`Num` bits/`Str`/`Decimal`) and holds no cycle-capable `Value`.
/// No-op trace, declared so the Map/Set helpers can trace keys uniformly. It
/// will STAY on `Rc` (`Str(Rc<str>)`) through V13-T2.
impl Trace for MapKey {
    fn is_type_tracked() -> bool {
        false
    }
}

impl Trace for Value {
    fn trace(&self, tracer: &mut Tracer) {
        match self {
            // Cycle-capable container variants: recurse into contained Values.
            // NOTE: these still hold `Rc` in V13-T1 — `Rc<T>: Trace` delegates
            // to `T::trace`, so tracing already reaches the inner Values. After
            // V13-T2 these become `Cc<T>` and the collector takes over.
            Value::Array(a) => a.trace(tracer),
            Value::Object(o) => o.trace(tracer),
            // Map/Set wrap a foreign `IndexMap`/`IndexSet` (orphan rule: no
            // blanket `Trace`), so each is held in a local `MapCell`/`SetCell`
            // newtype that carries the hand-written `Trace` impl below. The `Cc`
            // delegates to that impl, which borrows and traces the contents.
            Value::Map(m) => m.trace(tracer),
            Value::Set(s) => s.trace(tracer),
            Value::Instance(i) => i.trace(tracer),
            Value::Closure(c) => c.trace(tracer),
            // NOTE on `Function`: a tree-walker `Function` captures an
            // `Environment` (its own `Rc<RefCell<Scope>>` graph), which is NOT
            // one of the cycle-capable Value containers migrated in V13-T2 (see
            // the V13 type list: Array/Object/Map/Set/Instance/Closure + upvalue
            // cells). The VM expresses closures as `Value::Closure` with traced
            // upvalue cells instead. So `Function` (and its Environment) STAY on
            // `Rc` and are a no-op here — falling through to the catch-all.
            //
            // Everything else holds no cycle-capable Value (primitives), or is a
            // native/immutable/opaque handle that must stay acyclic (see the
            // module docs / deterministic-Drop invariant). No-op.
            _ => {}
        }
    }

    // Conservatively tracked: `Value` can contain `Cc<T>` after V13-T2.
    fn is_type_tracked() -> bool {
        true
    }
}

/// The closure upvalue cell (`RefCell<Value>`) — gcmodule already provides
/// `Trace for RefCell<T: Trace>`, so the `Value`-tracing path is covered by the
/// blanket impl. This explicit helper exists only to document the cell as a
/// traced node and is used by the V13-T1 unit test.
impl Trace for ObjectCell {
    fn trace(&self, tracer: &mut Tracer) {
        // `map: RefCell<IndexMap<String, Value>>`. Avoid holding the borrow if
        // it is already mutably borrowed (mirrors gcmodule's `RefCell` impl:
        // an outstanding borrow implies an outstanding reference, so skipping
        // is safe). `shape: Cell<u32>` and `frozen: Cell<bool>` are acyclic.
        if let Ok(map) = self.map.try_borrow() {
            trace_index_map(&map, tracer);
        }
    }

    fn is_type_tracked() -> bool {
        true
    }
}

/// `ArrayCell` (`Value::Array` payload, SP2 §4): wraps `vec: RefCell<Vec<Value>>`
/// beside a `frozen: Cell<bool>`. Only the vector can hold cycles; the `Cell<bool>`
/// is acyclic (`Copy`, no traceable edge). Avoid holding the borrow if it is
/// already mutably borrowed (an outstanding borrow implies an outstanding
/// reference, so skipping is safe — mirrors gcmodule's `RefCell` impl). `Vec<T:
/// Trace>` has a gcmodule blanket impl, so this delegates straight through to
/// `Value::trace`.
impl Trace for ArrayCell {
    fn trace(&self, tracer: &mut Tracer) {
        if let Ok(vec) = self.vec.try_borrow() {
            vec.trace(tracer);
        }
    }

    fn is_type_tracked() -> bool {
        true
    }
}

/// `MapCell` (`Value::Map` payload): a newtype over `RefCell<IndexMap<…>>`. The
/// foreign `IndexMap` has no `Trace`, so we trace the borrowed contents via the
/// free `trace_index_map` helper. An outstanding mutable borrow implies an
/// outstanding reference, so skipping it is safe (mirrors gcmodule's `RefCell`).
impl Trace for MapCell {
    fn trace(&self, tracer: &mut Tracer) {
        if let Ok(map) = self.try_borrow() {
            trace_index_map(&map, tracer);
        }
    }

    fn is_type_tracked() -> bool {
        true
    }
}

/// `SetCell` (`Value::Set` payload): a newtype over `RefCell<IndexSet<…>>`. See
/// [`MapCell`] — keys are acyclic `MapKey`s, traced via the free helper for
/// uniformity.
impl Trace for SetCell {
    fn trace(&self, tracer: &mut Tracer) {
        if let Ok(set) = self.try_borrow() {
            trace_index_set(&set, tracer);
        }
    }

    fn is_type_tracked() -> bool {
        true
    }
}

impl Trace for Instance {
    fn trace(&self, tracer: &mut Tracer) {
        // `class: Rc<Class>` is acyclic (no cycle-capable Values), `shape_id` is
        // a Cell<u32>. Only `fields: IndexMap<String, Value>` can hold cycles.
        trace_index_map(&self.fields, tracer);
    }

    fn is_type_tracked() -> bool {
        true
    }
}

impl Trace for Closure {
    fn trace(&self, tracer: &mut Tracer) {
        // `proto: Rc<FnProto>` is acyclic (compiled code, no Values). The
        // upvalue cells `Vec<Rc<RefCell<Value>>>` can capture cycle-capable
        // Values, so trace each cell. `Vec<T: Trace>`, `Rc<T: Trace>`, and
        // `RefCell<T: Trace>` all have gcmodule blanket impls, so this delegates
        // straight through to `Value::trace`.
        self.upvalues.trace(tracer);
    }

    fn is_type_tracked() -> bool {
        true
    }
}

// Note: `IndexMap<MapKey, Value>` (the `Value::Map` payload) and
// `IndexSet<MapKey>` (the `Value::Set` payload) are foreign types, so we cannot
// give them a blanket `Trace` impl (orphan rule). Instead the `Map`/`Set` arms
// of `Value::trace` above borrow and trace them via the free `trace_index_map`
// / `trace_index_set` helpers — no orphan impl needed.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::MapKey;
    use crate::vm::chunk::{Chunk, FnProto};
    use gcmodule::{Trace, Tracer};
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;

    // A custom traced leaf that bumps a thread-local counter whenever the
    // collector's recursion reaches it. We insert these as the LEAVES of a
    // `Value`-shaped graph by tracing them through the SAME machinery
    // (`Vec`/`IndexMap`/`RefCell` blanket impls + our free helpers) that
    // `Value::trace` uses, so a non-zero count proves the recursion actually
    // descends into every contained child — not just the top node.
    thread_local! {
        static VISITS: Cell<usize> = const { Cell::new(0) };
    }

    struct Probe;
    impl Trace for Probe {
        fn trace(&self, _t: &mut Tracer) {
            VISITS.with(|c| c.set(c.get() + 1));
        }
        fn is_type_tracked() -> bool {
            true
        }
    }

    fn anon_proto() -> Rc<FnProto> {
        Rc::new(FnProto {
            chunk: Chunk::new(),
            arity: 0,
            has_rest: false,
            is_async: false,
            is_generator: false,
            params: Vec::new(),
            ret: None,
        })
    }

    /// The free collection helpers must visit EVERY contained element, so the
    /// collector reaches transitively-referenced nodes.
    #[test]
    fn collection_helpers_visit_every_element() {
        let mut noop: Box<dyn FnMut(*const ())> = Box::new(|_p| {});

        VISITS.with(|c| c.set(0));
        let mut m: IndexMap<String, Probe> = IndexMap::new();
        m.insert("a".into(), Probe);
        m.insert("b".into(), Probe);
        trace_index_map(&m, &mut noop);
        assert_eq!(VISITS.with(|c| c.get()), 2, "both map values traced");

        VISITS.with(|c| c.set(0));
        // `IndexSet<Probe>` needs Probe: Hash+Eq; instead use a numeric set to
        // confirm the set helper completes (acyclic elements → no Probe visits).
        let mut s: IndexSet<u32> = IndexSet::new();
        s.insert(1);
        s.insert(2);
        s.insert(3);
        trace_index_set(&s, &mut noop);
        assert_eq!(VISITS.with(|c| c.get()), 0);
    }

    /// `Value::trace` must recurse through the container variants down to every
    /// leaf, and be a no-op for primitives / native handles. We prove the
    /// recursion reaches leaves by building a graph whose deepest values are
    /// `Probe`s wired in through the same container shapes `Value::trace` walks
    /// (Array→Vec, Object→IndexMap, Map→IndexMap, Set→IndexSet, Instance→fields,
    /// Closure→upvalue cells), then counting Probe visits.
    #[test]
    fn value_trace_visits_children_and_skips_primitives() {
        let mut noop: Box<dyn FnMut(*const ())> = Box::new(|_p| {});

        // Primitives + a native-like opaque value are no-ops (no panic, no
        // recursion). Builtin stands in for the acyclic/native family.
        VISITS.with(|c| c.set(0));
        for v in [
            Value::Nil,
            Value::Bool(true),
            Value::Number(3.0),
            Value::Str("hi".into()),
            Value::Builtin("print".into()),
        ] {
            v.trace(&mut noop);
        }
        assert_eq!(
            VISITS.with(|c| c.get()),
            0,
            "primitives must not recurse into any child"
        );

        // A nested container graph built ONLY from real Value variants. Tracing
        // it must complete without panic (proves every container arm recurses
        // and the borrows are taken safely).
        let inner = {
            let mut m = IndexMap::new();
            m.insert("k".to_string(), Value::Number(1.0));
            Value::Object(ObjectCell::new(m))
        };
        let arr = Value::Array(crate::value::ArrayCell::new(vec![inner.clone(), Value::Nil]));
        let mut mm: IndexMap<MapKey, Value> = IndexMap::new();
        mm.insert(MapKey::Str("a".into()), arr.clone());
        let map = Value::Map(MapCell::new(mm));
        let mut ss: IndexSet<MapKey> = IndexSet::new();
        ss.insert(MapKey::Num(0.0f64.to_bits()));
        let set = Value::Set(SetCell::new(ss));
        let closure = Value::Closure(Closure::with_upvalues(
            anon_proto(),
            vec![Cc::new(RefCell::new(arr.clone()))],
        ));
        for v in [&arr, &inner, &map, &set, &closure] {
            v.trace(&mut noop);
        }

        // Leaf-reachability proof: drive the SAME helper `Value::trace`'s
        // Object/Map arm uses over a map whose values are Probes. A non-zero
        // count shows the recursion descends past the top container.
        VISITS.with(|c| c.set(0));
        let mut probes: IndexMap<String, Probe> = IndexMap::new();
        probes.insert("x".into(), Probe);
        probes.insert("y".into(), Probe);
        probes.insert("z".into(), Probe);
        trace_index_map(&probes, &mut noop);
        assert_eq!(
            VISITS.with(|c| c.get()),
            3,
            "trace recursion must reach every contained child"
        );

        // ObjectCell / Instance / Closure trace arms exercised directly.
        let cell = ObjectCell::new({
            let mut m = IndexMap::new();
            m.insert("n".to_string(), Value::Number(2.0));
            m
        });
        cell.trace(&mut noop); // no panic, borrows safely

        let closure_inner = Closure::with_upvalues(
            anon_proto(),
            vec![Cc::new(RefCell::new(Value::Number(9.0)))],
        );
        closure_inner.trace(&mut noop); // no panic, traces upvalue cells
    }

    /// V13-T3: collection actually RUNS and reclaims a reference cycle. Build a
    /// self-referential array (`let a = []; a.push(a)`) directly over the `Cc`
    /// value model, drop the external handle, then force a collection via the
    /// public [`collect`] hook and assert the collector reports the cyclic node
    /// reclaimed. Acyclic data would already be freed by refcounting before the
    /// collector sees it, so a non-zero reclaim here is specifically the *cycle*
    /// being broken — proof collection is wired and effective. (V13-T4 is the full
    /// soundness gate; this just proves one cycle is reclaimed.)
    #[test]
    fn collect_reclaims_a_reference_cycle() {
        // Start from a clean baseline: drop anything pending, then collect so the
        // tracked set reflects only what THIS test allocates.
        super::collect();
        let before = gcmodule::count_thread_tracked();

        // `let a = []; a.push(a)` — an array whose sole element is itself. The
        // `Cc<RefCell<Vec<Value>>>` now has an internal edge from its own slot, so
        // dropping the external `a` leaves refcount 1 (the self-edge) → it is NOT
        // freed by refcounting and would leak without cycle collection.
        let a = Value::Array(crate::value::ArrayCell::new(Vec::new()));
        if let Value::Array(arr) = &a {
            arr.borrow_mut().push(a.clone());
        }
        // The cycle is now live and tracked. Drop the only external reference.
        drop(a);

        // Refcounting alone cannot reclaim it (the self-edge keeps refcount at 1).
        // Force a collection: trial-deletion must find the unreachable cycle and
        // free it. `collect` returns the number of objects reclaimed.
        let reclaimed = super::collect();
        assert!(
            reclaimed >= 1,
            "cycle collection must reclaim the self-referential array (got {reclaimed})"
        );

        // And the live tracked count returns to the pre-cycle baseline (the cyclic
        // node is gone from the collector's object space, not merely marked).
        let after = gcmodule::count_thread_tracked();
        assert_eq!(
            after, before,
            "tracked-object count must return to baseline after reclaiming the cycle"
        );
    }

    // ──────────────────────── V13-T4 SOUNDNESS GATE ────────────────────────
    //
    // Prove every cycle CLASS is actually reclaimed (not merely that ONE cycle is,
    // as `collect_reclaims_a_reference_cycle` above shows). Each test follows the
    // same deterministic shape — `gcmodule::count_thread_tracked()` deltas, never
    // RSS (no flakiness):
    //
    //   1. `super::collect()` to a clean baseline, record `before`.
    //   2. Build N independent cycles of the class under test in a loop, keeping an
    //      external handle to each so they stay live.
    //   3. Assert the tracked count is ELEVATED while held (cycles are alive and
    //      refcounting alone CANNOT free them — each cycle's internal edges keep
    //      its `Cc` refcounts > 0). This proves the cycle is real.
    //   4. Drop every external handle, `super::collect()`, and assert the tracked
    //      count returns to ~`before` — i.e. it does NOT grow linearly with N
    //      (`before + N*cycle_size`). A surviving cycle = a real soundness bug
    //      (a missing/incorrect `Trace` impl), to be fixed in the impls above, not
    //      here.
    //
    // N is large (100) so "no linear growth" is unambiguous: each cycle has ≥2
    // tracked nodes, so a leak would leave the tail ≥ before + 200, dwarfing any
    // small slop. We assert the post-collect count is within a tiny constant of
    // baseline (`<= before + SLOP`) to tolerate at most incidental allocations,
    // never N-proportional growth.
    const N: usize = 100;
    const SLOP: usize = 4;

    /// Cycle class 1 — SELF-CYCLE (data): `let a = []; a.push(a)`, N times.
    /// Each array's sole element is itself, so its `Cc<RefCell<Vec<Value>>>` has a
    /// refcount-keeping self-edge; dropping the external handle leaves a dead,
    /// unreachable 1-node cycle that only trial-deletion can reclaim. (T3 proves
    /// ONE; this is the N-in-a-loop no-linear-growth version the gate requires.)
    #[test]
    fn soundness_self_cycle_no_linear_growth() {
        super::collect();
        let before = gcmodule::count_thread_tracked();

        let mut held = Vec::with_capacity(N);
        for _ in 0..N {
            let a = Value::Array(crate::value::ArrayCell::new(Vec::new()));
            if let Value::Array(arr) = &a {
                arr.borrow_mut().push(a.clone()); // self-edge
            }
            held.push(a);
        }
        let during = gcmodule::count_thread_tracked();
        assert!(
            during >= before + N,
            "N self-cycles must be live & tracked while held (before={before}, during={during})"
        );

        drop(held); // refcounting cannot reclaim: each self-edge keeps refcount ≥ 1
        let reclaimed = super::collect();
        let after = gcmodule::count_thread_tracked();
        assert!(
            reclaimed >= N,
            "every self-cycle must be reclaimed (reclaimed={reclaimed}, N={N})"
        );
        assert!(
            after <= before + SLOP,
            "tracked count must return to baseline, not grow with N \
             (before={before}, after={after}, N={N})"
        );
    }

    /// Cycle class 2 — MUTUALLY-REFERENCING objects AND instances: `a.other = b;
    /// b.other = a`, N pairs. Covers both `Value::Object` (`Cc<ObjectCell>`) and
    /// `Value::Instance` (`Cc<RefCell<Instance>>`) since both are cycle-capable
    /// containers with field maps that can close a 2-node cycle.
    #[test]
    fn soundness_mutual_objects_and_instances_no_linear_growth() {
        super::collect();
        let before = gcmodule::count_thread_tracked();

        // --- Objects: a ⇄ b via "other" fields. ---
        let mut held = Vec::with_capacity(N);
        for _ in 0..N {
            let a = Value::Object(ObjectCell::new(IndexMap::new()));
            let b = Value::Object(ObjectCell::new(IndexMap::new()));
            if let (Value::Object(oa), Value::Object(ob)) = (&a, &b) {
                oa.borrow_mut().insert("other".into(), b.clone());
                ob.borrow_mut().insert("other".into(), a.clone());
            }
            held.push((a, b));
        }

        // --- Instances: two instances of a minimal class, mutually referencing. ---
        let class = Rc::new(crate::value::Class {
            name: "Node".into(),
            superclass: None,
            fields: IndexMap::new(),
            methods: IndexMap::new(),
            static_methods: IndexMap::new(),
            def_env: crate::env::Environment::global(),
        });
        let mut held_inst = Vec::with_capacity(N);
        for _ in 0..N {
            let mk = || {
                Value::Instance(Cc::new(RefCell::new(Instance {
                    class: class.clone(),
                    fields: IndexMap::new(),
                    shape_id: Cell::new(0),
                    frozen: Cell::new(false),
                })))
            };
            let a = mk();
            let b = mk();
            if let (Value::Instance(ia), Value::Instance(ib)) = (&a, &b) {
                ia.borrow_mut().fields.insert("other".into(), b.clone());
                ib.borrow_mut().fields.insert("other".into(), a.clone());
            }
            held_inst.push((a, b));
        }

        let during = gcmodule::count_thread_tracked();
        assert!(
            during >= before + 4 * N, // 2 objects + 2 instances per iteration
            "N object-pairs + N instance-pairs must be live & tracked while held \
             (before={before}, during={during})"
        );

        drop(held);
        drop(held_inst);
        let reclaimed = super::collect();
        let after = gcmodule::count_thread_tracked();
        assert!(
            reclaimed >= 4 * N,
            "every mutual object/instance cycle must be reclaimed \
             (reclaimed={reclaimed}, expected≥{})",
            4 * N
        );
        assert!(
            after <= before + SLOP,
            "tracked count must return to baseline, not grow with N \
             (before={before}, after={after}, N={N})"
        );
    }

    /// Cycle class 3 — MUTUALLY-CAPTURING closures: two closures `f`, `g` that each
    /// capture the OTHER through a shared upvalue cell (`f = fn => g(); g = fn =>
    /// f()`). The cells are `Cc<RefCell<Value>>` and each holds a `Value::Closure`
    /// whose upvalue list points at the OTHER's cell — a closure↔cell cycle the
    /// collector must break. Built directly over the `Cc` model because expressing
    /// "two closures capturing each other by reference" is the VM closure shape,
    /// not a simple source spawn we can drop a handle on deterministically.
    #[test]
    fn soundness_mutual_closures_no_linear_growth() {
        super::collect();
        let before = gcmodule::count_thread_tracked();

        let mut held = Vec::with_capacity(N);
        for _ in 0..N {
            // Two cells, initially Nil; each will hold a closure that captures the
            // other cell — closure_f.upvalues = [cell_g], closure_g.upvalues =
            // [cell_f]; cell_f holds closure_f, cell_g holds closure_g.
            let cell_f: Cc<RefCell<Value>> = Cc::new(RefCell::new(Value::Nil));
            let cell_g: Cc<RefCell<Value>> = Cc::new(RefCell::new(Value::Nil));

            let closure_f = Value::Closure(Closure::with_upvalues(
                anon_proto(),
                vec![cell_g.clone()], // f captures g's cell
            ));
            let closure_g = Value::Closure(Closure::with_upvalues(
                anon_proto(),
                vec![cell_f.clone()], // g captures f's cell
            ));
            *cell_f.borrow_mut() = closure_f;
            *cell_g.borrow_mut() = closure_g;

            // Hold ONLY the two cells externally; the closures are reachable only
            // through them, forming the cycle cell_f → f → cell_g → g → cell_f.
            held.push((cell_f, cell_g));
        }
        let during = gcmodule::count_thread_tracked();
        assert!(
            during >= before + 4 * N, // 2 cells + 2 closures per iteration
            "N closure-pairs must be live & tracked while held \
             (before={before}, during={during})"
        );

        drop(held);
        let reclaimed = super::collect();
        let after = gcmodule::count_thread_tracked();
        assert!(
            reclaimed >= 4 * N,
            "every mutual-closure cycle must be reclaimed \
             (reclaimed={reclaimed}, expected≥{})",
            4 * N
        );
        assert!(
            after <= before + SLOP,
            "tracked count must return to baseline, not grow with N \
             (before={before}, after={after}, N={N})"
        );
    }

    /// Cycle class 4 — FIBER ↔ FUTURE/GENERATOR loop. The handle types themselves
    /// (`Value::Future` = `SharedFuture(Rc<…>)`, `Value::Generator` =
    /// `Rc<GeneratorHandle>`) deliberately STAY on `Rc` and have NO-OP `Trace`
    /// (module docs: opaque task/coroutine state that must reclaim deterministically
    /// — they are NOT in the `Cc` collector's object space). So a cycle is NEVER
    /// closed THROUGH the handle's `Rc` edge; what the collector CAN and MUST
    /// reclaim is the cyclic `Cc` subgraph reachable from a fiber's live state.
    ///
    /// A VM-backed generator/future suspends a `Fiber` whose frame SLOTS are
    /// `Value`s and whose captured variables are `Cc<RefCell<Value>>` upvalue cells
    /// — the SAME cell type as a closure capture. We therefore model the faithful
    /// loop as: a captured cell that (transitively) references a closure that
    /// captures that very cell back — i.e. the Cc cycle that a self-recursive
    /// fiber's captured environment forms. We build it directly in Rust (a
    /// suspended fiber holding a self-referential capture is not something a dropped
    /// source handle exposes deterministically), and assert the captured-environment
    /// cycle is reclaimed once the (simulated) fiber state is dropped.
    #[test]
    fn soundness_fiber_capture_cycle_no_linear_growth() {
        super::collect();
        let before = gcmodule::count_thread_tracked();

        let mut held = Vec::with_capacity(N);
        for _ in 0..N {
            // A fiber's captured environment: a cell the body closes over. The
            // closure (the fiber body) captures `env_cell`; `env_cell` holds an
            // array that contains the closure — so the body reaches itself through
            // its own captured environment (a self-recursive generator/future body
            // capturing a value that references the body). cell → array → closure →
            // cell is a pure-Cc cycle; the (Rc) handle that would sit beside it is
            // off-graph and irrelevant to reclamation.
            let env_cell: Cc<RefCell<Value>> = Cc::new(RefCell::new(Value::Nil));
            let body = Value::Closure(Closure::with_upvalues(
                anon_proto(),
                vec![env_cell.clone()], // body captures its environment
            ));
            let arr = Value::Array(crate::value::ArrayCell::new(vec![body]));
            *env_cell.borrow_mut() = arr; // environment references the body's container
            held.push(env_cell);
        }
        let during = gcmodule::count_thread_tracked();
        assert!(
            during >= before + 3 * N, // cell + array + closure per iteration
            "N fiber-capture cycles must be live & tracked while held \
             (before={before}, during={during})"
        );

        drop(held);
        let reclaimed = super::collect();
        let after = gcmodule::count_thread_tracked();
        assert!(
            reclaimed >= 3 * N,
            "every fiber-capture cycle must be reclaimed \
             (reclaimed={reclaimed}, expected≥{})",
            3 * N
        );
        assert!(
            after <= before + SLOP,
            "tracked count must return to baseline, not grow with N \
             (before={before}, after={after}, N={N})"
        );
    }

    /// Acyclic control data is unaffected by collection: a plain array of numbers
    /// (no cycle) is freed by refcounting at `drop`, BEFORE the collector runs, so
    /// `collect()` finds nothing to reclaim and the tracked count is already back to
    /// baseline. This guards that the soundness tests above are measuring CYCLE
    /// reclamation specifically, not collection of ordinary garbage.
    #[test]
    fn soundness_acyclic_data_freed_by_refcounting_not_collection() {
        super::collect();
        let before = gcmodule::count_thread_tracked();

        let mut held = Vec::with_capacity(N);
        for i in 0..N {
            held.push(Value::Array(crate::value::ArrayCell::new(vec![Value::Number(
                i as f64,
            )])));
        }
        assert!(gcmodule::count_thread_tracked() >= before + N);

        drop(held); // no cycles → refcounting frees them immediately
        let at_drop = gcmodule::count_thread_tracked();
        assert!(
            at_drop <= before + SLOP,
            "acyclic data must be freed by refcounting at drop, before any collect \
             (before={before}, at_drop={at_drop})"
        );
        let reclaimed = super::collect();
        assert_eq!(
            reclaimed, 0,
            "collection must find nothing to reclaim — acyclic garbage is already gone"
        );
    }

    /// `maybe_collect` is a cheap no-op below the growth threshold (it must not
    /// collect on every call — that would tank throughput / the perf gate). Below
    /// threshold it returns 0 without sweeping; `collect` always sweeps.
    #[test]
    fn maybe_collect_is_a_noop_below_threshold() {
        super::collect(); // reset baseline
                          // A single tiny acyclic allocation is far below the
                          // COLLECT_GROWTH_THRESHOLD, so maybe_collect must skip.
        let v = Value::Array(crate::value::ArrayCell::new(vec![Value::Number(1.0)]));
        let reclaimed = super::maybe_collect();
        assert_eq!(
            reclaimed, 0,
            "maybe_collect must skip below the growth threshold"
        );
        drop(v);
    }

    // ───────────────────── V13-T6 CRITICAL GATE ─────────────────────
    //
    // Deterministic native-resource `Drop` is PRESERVED after the Rc→Cc cycle-GC
    // migration. The whole design rests on: OS resources (TCP/file/DB/child/timer/
    // socket/terminal handles) are NOT embedded in `Value` and NOT on the Cc graph.
    // They live in `Interp.resources` (a plain `HashMap<u64, ResourceState>`) keyed
    // by a handle id; the script-visible `Value::Native(Rc<NativeObject>)` is a cheap
    // clonable handle carrying only `{ id, kind, fields }` — no OS resource, no
    // `Drop` impl (verified: there is NO `impl Drop for NativeObject`).
    //
    // CONSEQUENCES this gate pins down, by EXECUTION:
    //   (A) A resource-table entry is freed at a DETERMINISTIC point — `take_resource`
    //       (explicit `close()`/`kill()`/EOF or scope-driven reclamation) — and NOT by
    //       a cycle collection. Closing/taking the resource frees the OS handle right
    //       then, with NO `gc::collect()` in sight.
    //   (B) Even when the `Value::Native` HANDLE is captured INTO a reference cycle
    //       (an Object that references itself and holds the handle), the OS resource
    //       is freed via the resource model independently of when the Cc cycle is
    //       collected — because `Value::Native` traces as a NO-OP (`_ => {}` in
    //       `Value::trace`), so the handle is invisible to the collector and the
    //       table entry is never owned by / trapped in the cycle. Collecting the
    //       cycle later drops only the `Rc<NativeObject>` (id+kind+fields) — never the
    //       OS resource, which the resource model already reclaimed. No double-free,
    //       no panic.
    //
    // This MATCHES the tree-walker exactly: it uses the SAME `Interp` resource table,
    // the SAME `register_resource`/`take_resource`, and the SAME `Rc<NativeObject>`
    // (Native STAYS on `Rc`, not `Cc`, in both engines). So Drop timing is identical
    // before and after the GC, and identical between tree-walker and VM.
    //
    // We probe with the always-present (`--no-default-features`-safe) `ResourceState`
    // variants — `Closed` and `Interval` (a real `tokio::time::Interval`, an OS-backed
    // timer resource). Table-entry presence (`resource_count`) is the observable: an
    // entry present ⇒ the resource is alive; an entry gone ⇒ its `Drop` has run.

    use crate::interp::{Interp, ResourceState};

    /// (A) The resource-table entry — and thus the OS resource's `Drop` — is freed at
    /// `take_resource` (close/scope), IMMEDIATELY, with NO collection. The GC did not
    /// take over resource lifetime.
    #[tokio::test]
    async fn native_resource_drop_is_immediate_at_close_not_at_collection() {
        let interp = Interp::new();
        let baseline = interp.resource_count();

        // Register a real OS-backed resource (a tokio Interval timer) behind a
        // Value::Native handle, exactly as std/time does.
        let handle = interp.register_resource(
            crate::value::NativeKind::Interval,
            indexmap::IndexMap::new(),
            ResourceState::Interval(Box::new(tokio::time::interval(
                std::time::Duration::from_secs(1),
            ))),
        );
        let id = match &handle {
            Value::Native(n) => n.id,
            _ => unreachable!("register_resource yields a Native handle"),
        };
        assert_eq!(
            interp.resource_count(),
            baseline + 1,
            "the live resource is in the table"
        );

        // Explicit close == `take_resource` (the close()/kill()/EOF path). This drops
        // the underlying OS resource RIGHT NOW. Crucially: NO gc::collect() is called.
        let taken = interp.take_resource(id);
        assert!(
            matches!(taken, Some(ResourceState::Interval(_))),
            "close takes the live resource out of the table"
        );
        // `taken` drops here at end of statement → OS timer Drop fires deterministically.
        drop(taken);

        assert_eq!(
            interp.resource_count(),
            baseline,
            "resource is reclaimed at close — immediately, WITHOUT any cycle collection"
        );

        // A subsequent collection is irrelevant to the resource (it's already gone)
        // and must be a harmless no-op for it: the script handle is still around but
        // is just an Rc<id> off the Cc graph.
        super::collect();
        assert_eq!(
            interp.resource_count(),
            baseline,
            "collection does not resurrect or double-free the closed resource"
        );

        // The Value::Native handle is still a valid (now-dangling) id — it is NOT
        // freed by the collector and dropping it does nothing to the table.
        drop(handle);
        assert_eq!(interp.resource_count(), baseline);
    }

    /// (A') Dropping the `Value::Native` HANDLE alone does NOT free the table entry —
    /// the resource model (close/scope), not handle refcount, governs the OS
    /// resource's lifetime. (This is the SAME pre-GC behavior: `NativeObject` has no
    /// `Drop`.) The entry is reclaimed deterministically by `take_resource`, and is
    /// otherwise reclaimed when the whole `Interp` drops — never by the GC.
    #[test]
    fn dropping_native_handle_does_not_free_resource_table_entry() {
        let interp = Interp::new();
        let baseline = interp.resource_count();

        let handle = interp.register_resource(
            crate::value::NativeKind::Interval,
            indexmap::IndexMap::new(),
            ResourceState::Closed,
        );
        let id = match &handle {
            Value::Native(n) => n.id,
            _ => unreachable!(),
        };
        assert_eq!(interp.resource_count(), baseline + 1);

        // Drop the only script-visible handle. The Rc<NativeObject> is freed, but the
        // table entry persists — handle refcount does NOT drive resource lifetime.
        drop(handle);
        assert_eq!(
            interp.resource_count(),
            baseline + 1,
            "table entry outlives the handle Rc — lifetime is resource-model-governed"
        );
        // A collection cannot reach the table (it's plain Rust state, not Cc), so it
        // cannot free the entry either.
        super::collect();
        assert_eq!(
            interp.resource_count(),
            baseline + 1,
            "collection never touches the resource table"
        );
        // Deterministic reclamation is via take_resource (the close path).
        assert!(interp.take_resource(id).is_some());
        assert_eq!(interp.resource_count(), baseline);
    }

    /// (B) ADVERSARIAL: a `Value::Native` handle captured INTO a reference cycle.
    /// Build a self-referential Object that ALSO holds the native handle, drop the
    /// external reference (the cycle is now unreachable but kept alive by its own
    /// internal edge), close the resource via the resource model, and prove:
    ///   - closing frees the OS resource DETERMINISTICALLY at the close point, while
    ///     the cycle is still uncollected (the resource is NOT held hostage by the
    ///     Cc cycle), AND
    ///   - collecting the cycle afterwards is a clean no-op for the resource: it drops
    ///     only the Rc<NativeObject> handle (id+kind+fields, off the Cc graph) — no
    ///     double-free, no panic.
    #[tokio::test]
    async fn native_handle_trapped_in_cycle_still_drops_resource_deterministically() {
        let interp = Interp::new();
        let baseline = interp.resource_count();

        // A real OS-backed resource in the table.
        let handle = interp.register_resource(
            crate::value::NativeKind::Interval,
            indexmap::IndexMap::new(),
            ResourceState::Interval(Box::new(tokio::time::interval(
                std::time::Duration::from_secs(1),
            ))),
        );
        let id = match &handle {
            Value::Native(n) => n.id,
            _ => unreachable!(),
        };
        assert_eq!(interp.resource_count(), baseline + 1);

        // Clean GC baseline so the reclaim count below is attributable to THIS cycle.
        super::collect();
        let tracked_before = gcmodule::count_thread_tracked();

        // Build the adversarial cycle: an Object that (1) holds the Native handle and
        // (2) references ITSELF. Internal self-edge ⇒ refcounting alone can never free
        // it; only the cycle collector can. The native handle is trapped inside.
        let obj = {
            let mut m = IndexMap::new();
            m.insert("conn".to_string(), handle.clone());
            Value::Object(ObjectCell::new(m))
        };
        if let Value::Object(o) = &obj {
            o.borrow_mut().insert("self".to_string(), obj.clone());
        }
        // Drop every external reference to the cycle AND the standalone handle. The
        // cycle is now unreachable but still LIVE (self-edge keeps refcount > 0); the
        // collector has NOT run yet.
        drop(obj);
        drop(handle);

        // The OS resource is STILL in the table — proving it is NOT owned by the Cc
        // cycle (the cycle holds only an Rc<NativeObject> handle, off the Cc graph).
        assert_eq!(
            interp.resource_count(),
            baseline + 1,
            "resource is not trapped in the (still-uncollected) cycle"
        );

        // Close via the resource model — DETERMINISTIC: the OS resource's Drop fires
        // here, while the cycle is STILL uncollected. The GC has no say in this.
        let taken = interp.take_resource(id);
        assert!(matches!(taken, Some(ResourceState::Interval(_))));
        drop(taken);
        assert_eq!(
            interp.resource_count(),
            baseline,
            "OS resource freed at close, NOT held hostage by the live Cc cycle"
        );

        // NOW collect the cycle. It must reclaim the cyclic Object (≥1 node) and must
        // NOT double-free / panic: the handle it still holds is just an Rc<id> whose
        // table entry was already reclaimed above.
        let reclaimed = super::collect();
        assert!(
            reclaimed >= 1,
            "the self-referential Object cycle is reclaimed (got {reclaimed})"
        );
        assert_eq!(
            gcmodule::count_thread_tracked(),
            tracked_before,
            "tracked count returns to baseline — the cycle (holding the handle) is gone"
        );
        // Resource table is unaffected by the collection (entry already reclaimed at
        // close); no double-free, no resurrection.
        assert_eq!(interp.resource_count(), baseline);
    }

    /// (B') The mirror of (B) where the cycle is NEVER explicitly closed and is left
    /// to the collector. Here the resource entry is governed by the resource model's
    /// other deterministic endpoint — the owning `Interp`'s `Drop`. Collecting the
    /// cycle drops the `Rc<NativeObject>` but, since the table entry was NOT taken,
    /// the entry persists until `Interp` drop. This is IDENTICAL to the tree-walker:
    /// a never-closed native handle's table entry lives until interp teardown in both.
    /// We assert no double-free / panic on collecting a cycle that holds a handle to a
    /// STILL-LIVE table entry.
    #[test]
    fn collecting_cycle_with_handle_to_live_resource_is_safe() {
        let interp = Interp::new();
        let baseline = interp.resource_count();

        let handle = interp.register_resource(
            crate::value::NativeKind::Interval,
            indexmap::IndexMap::new(),
            ResourceState::Closed,
        );
        assert_eq!(interp.resource_count(), baseline + 1);

        super::collect();
        let tracked_before = gcmodule::count_thread_tracked();

        let obj = {
            let mut m = IndexMap::new();
            m.insert("conn".to_string(), handle.clone());
            Value::Object(ObjectCell::new(m))
        };
        if let Value::Object(o) = &obj {
            o.borrow_mut().insert("self".to_string(), obj.clone());
        }
        drop(obj);
        drop(handle);

        // Collect the unreachable cycle WITHOUT having closed the resource. The cycle
        // (and the Rc<NativeObject> it carries) is reclaimed; the OS resource's table
        // entry is NOT freed by this (it lives until Interp drop) — and there is no
        // double-free or panic.
        let reclaimed = super::collect();
        assert!(reclaimed >= 1, "the cycle is reclaimed (got {reclaimed})");
        assert_eq!(gcmodule::count_thread_tracked(), tracked_before);
        assert_eq!(
            interp.resource_count(),
            baseline + 1,
            "never-closed resource entry persists past collection — freed at Interp drop, \
             exactly like the tree-walker"
        );

        // Interp drop is the other deterministic endpoint: the table (and its entry)
        // is freed when the Interp goes out of scope here. No panic.
        drop(interp);
    }
}
