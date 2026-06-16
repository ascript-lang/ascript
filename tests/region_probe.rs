//! REGION Phase-0 probe — end-to-end allocation-lifetime accounting (spec §5.2, Task 0.2).
//!
//! These tests run REAL `.as` programs through the specialized VM under
//! `--features region-probe` and assert the histogram the probe builds. They are the
//! integration counterpart to the unit tests in `src/vm/region_probe.rs` (which pin
//! the thread-local state machine in isolation).
//!
//! Compiled ONLY under `--features region-probe` — in a default build this file is an
//! empty crate (no probe symbols exist), so it never affects the default test suite.
//!
//! THREADING NOTE: `vm_run_source` builds its own `tokio` `LocalSet` and drives the
//! whole program (incl. spawned tasks) on the CURRENT thread, so the probe's
//! thread-locals it touches are this test thread's. We use a single-threaded
//! `#[tokio::test(flavor = "current_thread")]` and `reset()` at the top of each test
//! to isolate the shared thread-local stats.

#![cfg(feature = "region-probe")]

use ascript::vm::region_probe::{self, ProbeStats, SiteClass};

/// Run `src` on the specialized VM, returning the probe histogram accumulated during
/// the run (reset beforehand for isolation). The end-of-run drop of every live cell
/// happens inside `vm_run_source`'s final `gc::collect()` + drop, so by the time this
/// returns the stats are complete for cells that die by program end.
async fn run_and_probe(src: &str) -> ProbeStats {
    region_probe::reset();
    let (_out, _exit) = ascript::vm_run_source(src)
        .await
        .expect("program ran without panic");
    region_probe::stats()
}

/// `[in_task]` count for a kind's literal site.
fn lit_in_task(row: &[[u64; 2]; 2]) -> u64 {
    row[SiteClass::Literal as usize][1]
}
/// `[escaped]` count for a kind's literal site.
fn lit_escaped(row: &[[u64; 2]; 2]) -> u64 {
    row[SiteClass::Literal as usize][0]
}

#[tokio::test(flavor = "current_thread")]
async fn vm_object_literal_at_top_level_is_literal_in_task() {
    // A bare object literal that dies at top-level (task 0) → [Literal][in_task].
    // Main is "task 0", which never retires before program end, so it is in-task.
    let s = run_and_probe("let o = {a: 1, b: 2}; print(o.a)").await;
    assert!(
        lit_in_task(&s.object) >= 1,
        "expected >=1 [Literal][in_task] object, got stats {:?}",
        s.object
    );
}

#[tokio::test(flavor = "current_thread")]
async fn vm_array_literal_is_classified_literal() {
    let s = run_and_probe("let a = [1, 2, 3]; print(a[0])").await;
    assert!(
        lit_in_task(&s.array) >= 1,
        "expected >=1 [Literal] array, got {:?}",
        s.array
    );
}

#[tokio::test(flavor = "current_thread")]
async fn literal_dying_in_a_spawned_task_records_in_task() {
    // The object literal `{n: i}` is built and dropped entirely INSIDE the spawned
    // async-fn body's task — it never escapes — so it is [Literal][in_task]. We
    // build several to make the count robust against any single-alloc noise.
    let src = r#"
        async fn work(i) {
            let o = {n: i, doubled: i + i}
            return o.doubled
        }
        let total = 0
        for (i in 0..20) {
            total = total + await work(i)
        }
        print(total)
    "#;
    let s = run_and_probe(src).await;
    assert!(
        lit_in_task(&s.object) >= 20,
        "expected >=20 in-task literal objects from the spawned bodies, got {:?}",
        s.object
    );
}

#[tokio::test(flavor = "current_thread")]
async fn literal_stored_to_a_global_outliving_its_task_records_escaped() {
    // The spawned task builds an object literal and stows it in a module-global
    // collection. The task ENDS (its body returns) while the object stays reachable
    // from the global; the object is dropped only at PROGRAM end, by which point its
    // birth task has long retired → [Literal][escaped].
    let src = r#"
        import * as array from "std/array"
        let kept = []
        async fn stash(i) {
            let o = {n: i}
            array.push(kept, o)
            return nil
        }
        for (i in 0..15) {
            await stash(i)
        }
        print(len(kept))
    "#;
    let s = run_and_probe(src).await;
    assert!(
        lit_escaped(&s.object) >= 15,
        "expected >=15 escaped literal objects (stored to a global, task ended \
         first), got escaped={} in_task={} (full row {:?})",
        lit_escaped(&s.object),
        lit_in_task(&s.object),
        s.object
    );
}

#[tokio::test(flavor = "current_thread")]
async fn in_task_and_escaped_are_distinguished_in_one_run() {
    // One program with BOTH a captured (escaped) and a transient (in-task) literal in
    // each spawned task. The classification must separate them.
    let src = r#"
        import * as array from "std/array"
        let kept = []
        async fn work(i) {
            let escaping = {tag: "keep", n: i}
            array.push(kept, escaping)
            let transient = {tag: "drop", n: i}
            return transient.n
        }
        let sum = 0
        for (i in 0..10) {
            sum = sum + await work(i)
        }
        print(len(kept) + sum)
    "#;
    let s = run_and_probe(src).await;
    assert!(
        lit_in_task(&s.object) >= 10,
        "transient literals should be in-task, got {:?}",
        s.object
    );
    assert!(
        lit_escaped(&s.object) >= 10,
        "stashed literals should be escaped, got {:?}",
        s.object
    );
}
