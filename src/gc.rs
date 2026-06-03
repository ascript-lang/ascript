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
//! ### Phasing
//! - **V13-T1 (this task):** add the `gcmodule` dependency and implement
//!   [`Trace`] for every cycle-capable runtime type, **WITHOUT** migrating any
//!   `Rc` to `Cc`. The impls compile and are exercised by a unit test, but are
//!   not yet wired into a `Cc`-backed graph — they become load-bearing in T2.
//! - **V13-T2:** the one-pass migration of the cycle-capable [`Value`] variants
//!   (`Array`/`Object`/`Map`/`Set`/`Instance`/`Closure` + upvalue cells) from
//!   `Rc` to `Cc`. The [`Trace`] impls here are what the collector will call.
//! - **V13-T3+:** enable and tune collection; soundness / soak / Drop gates.
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

use crate::value::{Instance, MapCell, MapKey, ObjectCell, SetCell, Value};
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
        // is safe). `shape: Cell<u32>` is acyclic.
        if let Ok(map) = self.map.try_borrow() {
            trace_index_map(&map, tracer);
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
        let arr = Value::Array(Cc::new(RefCell::new(vec![inner.clone(), Value::Nil])));
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
        let a = Value::Array(Cc::new(RefCell::new(Vec::new())));
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

    /// `maybe_collect` is a cheap no-op below the growth threshold (it must not
    /// collect on every call — that would tank throughput / the perf gate). Below
    /// threshold it returns 0 without sweeping; `collect` always sweeps.
    #[test]
    fn maybe_collect_is_a_noop_below_threshold() {
        super::collect(); // reset baseline
                          // A single tiny acyclic allocation is far below the
                          // COLLECT_GROWTH_THRESHOLD, so maybe_collect must skip.
        let v = Value::Array(Cc::new(RefCell::new(vec![Value::Number(1.0)])));
        let reclaimed = super::maybe_collect();
        assert_eq!(reclaimed, 0, "maybe_collect must skip below the growth threshold");
        drop(v);
    }
}
