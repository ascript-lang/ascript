//! M17 structured-concurrency / cancel-on-drop regression tests (end-to-end via
//! the real binary). These lock in the behavior that the memory-leak fix
//! introduced:
//!
//! - an un-awaited `async fn` call is **cancelled** when its future is dropped
//!   (so its side effect does not run), while `task.spawn(...)` **detaches** the
//!   task so it runs to completion;
//! - `race` **cancels the losers** and `timeout` **cancels the timed-out work**;
//! - `gather` still runs its inputs **concurrently** and preserves order.
//!
//! A regression to the old spawn-and-orphan model would make the cancellation
//! assertions fail (the side effects would reappear) — and, for the leak itself,
//! the in-flight high-water-mark unit test in `src/interp.rs`
//! (`unawaited_async_loop_keeps_inflight_bounded`) would fail.

use std::process::Command;

fn run(name: &str, src: &str) -> String {
    let file = std::env::temp_dir().join(format!("ascript_sc_{name}.as"));
    std::fs::write(&file, src).unwrap();
    let bin = env!("CARGO_BIN_EXE_ascript");
    let output = Command::new(bin).arg("run").arg(&file).output().unwrap();
    assert!(output.status.success(), "process failed: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn unawaited_async_call_is_cancelled() {
    // The future is dropped at the end of the expression statement -> the task is
    // aborted before its post-sleep side effect runs.
    let out = run(
        "cancel",
        "import * as time from \"std/time\"\n\
         async fn work() { await time.sleep(20)\n print(\"worked\") }\n\
         work()\n\
         print(\"main\")\n",
    );
    assert!(out.contains("main"), "got: {out:?}");
    assert!(!out.contains("worked"), "unawaited call must be cancelled: {out:?}");
}

#[test]
fn spawn_detaches_so_the_task_runs() {
    // `task.spawn` opts out of cancel-on-drop: the task runs to completion even
    // though the handle is discarded.
    let out = run(
        "detach",
        "import * as time from \"std/time\"\n\
         import * as task from \"std/task\"\n\
         async fn work() { await time.sleep(5)\n print(\"worked\") }\n\
         task.spawn(work())\n\
         print(\"main\")\n",
    );
    assert!(out.contains("main"), "got: {out:?}");
    assert!(out.contains("worked"), "spawned task must run to completion: {out:?}");
}

#[test]
fn awaited_async_call_still_runs() {
    // Back-compat: awaiting keeps a handle alive across the work, so it completes.
    let out = run(
        "awaited",
        "async fn work() { return 21 }\n\
         print(await work() + await work())\n",
    );
    assert!(out.contains("42"), "got: {out:?}");
}

#[test]
fn race_cancels_the_loser() {
    // The fast branch wins; the slow branch is cancelled and its side effect
    // (appending to `ran`) never happens.
    let out = run(
        "race",
        "import * as task from \"std/task\"\n\
         import * as time from \"std/time\"\n\
         import * as array from \"std/array\"\n\
         let ran = []\n\
         async fn slow() { await time.sleep(60)\n array.push(ran, 1)\n return \"slow\" }\n\
         async fn fast() { return \"fast\" }\n\
         print(await task.race([slow(), fast()]))\n\
         await time.sleep(120)\n\
         print(len(ran))\n",
    );
    assert!(out.contains("fast"), "fast should win: {out:?}");
    assert!(out.trim_end().ends_with('0'), "race loser must be cancelled (len(ran)==0): {out:?}");
}

#[test]
fn timeout_cancels_the_timed_out_work() {
    let out = run(
        "timeout",
        "import * as task from \"std/task\"\n\
         import * as time from \"std/time\"\n\
         import * as array from \"std/array\"\n\
         let ran = []\n\
         async fn slow() { await time.sleep(60)\n array.push(ran, 1)\n return \"x\" }\n\
         let [v, err] = await task.timeout(5, slow())\n\
         print(err != nil)\n\
         await time.sleep(120)\n\
         print(len(ran))\n",
    );
    assert!(out.contains("true"), "timeout should fire: {out:?}");
    assert!(out.trim_end().ends_with('0'), "timed-out work must be cancelled (len(ran)==0): {out:?}");
}

#[test]
fn gather_preserves_order() {
    // Order is preserved regardless of completion order (later items finish first).
    let out = run(
        "gather",
        "import * as task from \"std/task\"\n\
         import * as time from \"std/time\"\n\
         async fn w(ms, v) { await time.sleep(ms)\n return v }\n\
         print(await task.gather([w(60, 1), w(20, 2), w(40, 3)]))\n",
    );
    assert!(out.contains("[1, 2, 3]"), "order preserved: {out:?}");
}
// NOTE: gather's *concurrency* (wall ≈ max, not sum) is asserted in-process by
// `gather_runs_concurrently_wall_time` in src/interp.rs — timing a subprocess
// here would measure binary cold-start, not the gather, and be flaky.
