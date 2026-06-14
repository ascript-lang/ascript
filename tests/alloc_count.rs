//! CALL Gate-18: per-call/per-element allocation-count slope probes.
//!
//! A dedicated binary because it installs a `#[global_allocator]`. Slopes
//! `(allocs(2N) − allocs(N)) / N` are immune to startup/runtime noise; the
//! budgets are generous tripwires, not exact counts — a regression that doubles
//! a slope trips them, allocator jitter does not.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.realloc(p, l, new_size) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Drive `src` through `vm_run_source` (call_fast=true) or
/// `vm_run_source_no_call_fast` (call_fast=false) on a worker-stack
/// current-thread runtime — the exact `!Send` idiom the differential uses.
fn run_program(src: &str, call_fast: bool) {
    ascript::run_on_worker_stack({
        let src = src.to_string();
        move || async move {
            if call_fast {
                ascript::vm_run_source(&src).await.expect("vm_run_source failed");
            } else {
                ascript::vm_run_source_no_call_fast(&src)
                    .await
                    .expect("vm_run_source_no_call_fast failed");
            }
        }
    });
}

/// Return the number of allocations triggered by running `src`.
fn allocs_for(src: &str, call_fast: bool) -> u64 {
    let before = ALLOCS.load(Relaxed);
    run_program(src, call_fast);
    ALLOCS.load(Relaxed) - before
}

/// Slope = `(allocs(2N) − allocs(N)) / N` — allocations per loop iteration,
/// immune to one-time startup/init noise.
fn slope(make_src: impl Fn(u64) -> String, call_fast: bool) -> f64 {
    const N: u64 = 20_000;
    let a = allocs_for(&make_src(N), call_fast);
    let b = allocs_for(&make_src(2 * N), call_fast);
    (b.saturating_sub(a)) as f64 / N as f64
}

/// A1+A2 (Task 2.1/2.2 gate): capture-free calls must show reduced per-call allocs.
/// A1 (empty-cells fast path) is always-on and benefits both call_fast=true and
/// call_fast=false. A2 (in-place arg binding) is gated on call_fast=true and
/// eliminates the 2 arg Vecs (the `vec![Value::Nil; argc]` in push_closure_frame
/// and the `BoundArgs.values` Vec from check_call_args).
///
/// After A2: `call_fast=true` path should reach ~0 allocs/call (slope < 1.0).
/// After A1 only: `call_fast=false` path is ~2 (cells-vec gone, 2 arg Vecs remain).
#[test]
fn capture_free_call_alloc_slope_drops_with_a1_and_a2() {
    let mk = |n: u64| {
        format!(
            "fn f(a, b) {{ return a + b }}\nlet s = 0\nfor (i in 0..{n}) {{ s = f(s, 1) }}\nprint(s)"
        )
    };
    // `mk` is a non-capturing closure (Copy), so it can be passed by value twice.
    let on = slope(mk, true);
    let off = slope(mk, false);
    println!("A1+A2 capture_free slope: call_fast=true → {on:.3}/call, call_fast=false → {off:.3}/call");
    // A2: with call_fast=true, in-place binding eliminates the 2 arg Vecs.
    // Combined with A1 (cells-vec gone), the qualifying call shape allocates ~0.
    assert!(
        on < 1.0,
        "A2 post: per-call alloc slope should be below 1.0 (in-place binding), got on={on}"
    );
    // A1 (no A2): call_fast=false still removes the cells-vec but keeps 2 arg Vecs.
    // Slope should be below 3.0 (was ~3 pre-A1, now ~2).
    assert!(
        off < 3.0,
        "A1 post: per-call alloc slope should be below 3.0 (cells-vec removed), got off={off}"
    );
    // The in-place path should be meaningfully faster than the fallback.
    assert!(
        on < off,
        "A2: call_fast=true slope {on} should be less than call_fast=false slope {off}"
    );
}

/// SHAPE (Task 6.1 probe): per-object construction allocation slope. Each loop
/// iteration builds a fresh 4-field object literal and reads its fields — the
/// `object_churn` shape. SHAPE's slab construction aims to cut the per-object
/// allocation count vs the old IndexMap. The slope `(allocs(2N)−allocs(N))/N`
/// isolates the per-object cost. Run on BOTH base and candidate (same source)
/// to read the delta.
#[test]
#[ignore]
fn object_construction_alloc_slope() {
    let mk = |n: u64| {
        format!(
            "let total = 0\nfor (i in 0..{n}) {{ let o = {{ id: i, name: \"node\", x: i * 2, y: i + 1 }}\n total = total + o.x + o.y + o.id }}\nprint(total)"
        )
    };
    let on = slope(mk, true);
    let off = slope(mk, false);
    println!("SHAPE object_construction slope: call_fast=true → {on:.3}/object, call_fast=false → {off:.3}/object");
    // Slab construction of this 4-key object is ~2 allocs/object; the OLD IndexMap
    // dict path was ~13. The bound is 6.0 — ~3× headroom above the measured ~2.0 for
    // allocator noise, but it TRIPS on a silent regression back to the dict path
    // (~13). (An earlier `< 50.0` bound passed at both 2 AND 13 → it only caught a
    // catastrophic blowup, not a demotion to dict — tightened per the Phase-6 review.)
    assert!(
        on < 6.0,
        "object construction slope {on}/object regressed toward the IndexMap dict path \
         (slab construction should be ~2/object; >6 suggests a demotion to dict)"
    );
    assert!(
        off < 6.0,
        "object construction slope {off}/object (call_fast off) regressed toward dict"
    );
}

/// A3 (Task 2.3 gate): re-entrant `call_value` calls (native→VM re-entry via
/// `array.map`) must not regress. The anti-false-green proof that pooling actually
/// fires is in `tests/call_fast.rs::pooled_fiber_reuses_counter_is_nonzero`; here
/// we guard against alloc regressions (a new re-entry path that adds allocs/element).
///
/// The slope is `(allocs(2N) − allocs(N)) / N` — allocations per `array.map`
/// element. Fiber pooling saves 2 Vec allocs per re-entry (the `frames` and `stack`
/// Vecs), but per-element cost is dominated by closure call overhead. We therefore
/// assert a generous upper-bound regression tripwire rather than a strict `on < off`
/// (the pool saves ~2 of ~25 allocs/element — too small for a reliable per-run order).
///
/// Budget: both slopes < 50 allocs/element in release (a regression that doubles the
/// slope trips this; allocator jitter does not).
///
/// **Run with `--release --test-threads=1`** — the global `ALLOCS` counter is shared
/// across parallel test threads, so running alongside other alloc_count tests in debug
/// mode produces noise. The functional (non-slope) proof that pooling fires is in
/// `tests/call_fast.rs::pooled_fiber_reuses_counter_is_nonzero` which runs in the
/// normal `cargo test` suite. This test is `#[ignore]` to opt-in for release perf gates.
#[test]
#[ignore]
fn reentrant_call_value_fiber_is_pooled() {
    // array.map over a list — each element drives one `Vm::call_value` re-entry.
    // We vary N (the list length) by embedding it in the source.
    let mk = |n: u64| {
        let elems: Vec<String> = (0..n)
            .map(|i| format!("\"{}\"", (b'a' + (i % 26) as u8) as char))
            .collect();
        format!(
            "import * as array from \"std/array\"\nlet words = [{}]\nlet lengths = array.map(words, (w) => len(w))\nlet _ = array.reduce(lengths, (a, n) => a + n, 0)\nprint(_)",
            elems.join(",")
        )
    };
    let on = slope(mk, true);
    let off = slope(mk, false);
    println!("A3 re-entrant slope: call_fast=true → {on:.3}/element, call_fast=false → {off:.3}/element");
    // A3: pooling must not add per-element allocs (on ≤ off + 2 allows for noise).
    assert!(
        on <= off + 2.0,
        "A3: call_fast=true slope {on} exceeds call_fast=false slope {off} by more than 2 (pooling should not add overhead)"
    );
    // Regression guard: slope should stay below 50 allocs/element in release.
    assert!(
        on < 50.0,
        "A3: call_fast=true re-entry slope {on} is unexpectedly high (possible regression)"
    );
    assert!(
        off < 50.0,
        "A3: call_fast=false re-entry slope {off} is unexpectedly high (possible regression)"
    );
}
