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

/// A1 (Task 2.1 gate): capture-free calls must show reduced per-call allocs
/// after the empty-cells fast path lands. A1 is NOT gated on `call_fast` (it is
/// behavior-invisible — an empty vec is indistinguishable from an all-None vec
/// via `.get`), so BOTH `on` and `off` paths benefit. The assert therefore uses
/// an absolute slope bound that proves the cells-vec alloc is gone: before A1
/// the slope was ~3 (cells-vec + 2 arg Vecs); after A1 it is ~2.
///
/// The strict bound (`< 1.0`) is tightened in Task 2.2 once A2 (in-place arg
/// binding, which eliminates the 2 arg Vecs and is gated on `call_fast`) lands.
#[test]
fn capture_free_call_alloc_slope_drops_with_a1() {
    let mk = |n: u64| {
        format!(
            "fn f(a, b) {{ return a + b }}\nlet s = 0\nfor (i in 0..{n}) {{ s = f(s, 1) }}\nprint(s)"
        )
    };
    // `mk` is a non-capturing closure (Copy), so it can be passed by value twice.
    let on = slope(mk, true);
    let off = slope(mk, false);
    // A1 removes the cells vector (1 allocation) from every capture-free call.
    // Pre-A1 slope was ~3; post-A1 slope is ~2. Both on and off benefit since
    // A1 is always-on. The bound proves the cells-vec is gone, not just noise.
    assert!(
        on < 3.0,
        "A1 post: per-call alloc slope should be below 3.0 (cells-vec removed), got on={on}"
    );
    assert!(
        off < 3.0,
        "A1 post: per-call alloc slope should be below 3.0 (cells-vec removed), got off={off}"
    );
    // Both paths are identical for A1 (not gated); the on/off delta proves
    // no regression is introduced by the call_fast infrastructure.
    let delta = (on - off).abs();
    assert!(
        delta < 0.5,
        "A1: call_fast on/off paths should be identical (A1 not gated), delta={delta} on={on} off={off}"
    );
}
