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
