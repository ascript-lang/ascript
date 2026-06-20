//! SP9 §3 — determinism oracle.
//!
//! These tests exercise the `DeterminismContext` seams through the
//! `run_source_deterministic(src, seed)` test entry point (the eventual
//! `--deterministic --seed N` CLI flag maps to this path):
//!
//! - **same-seed-same-output** (spec §3.5): a single-task program using random +
//!   time produces byte-identical output across two runs with the same seed.
//! - **default-mode-byte-identical**: the SAME program run WITHOUT deterministic
//!   mode behaves exactly as pre-SP9 (the seams are inert when `determinism` is
//!   `None`) — asserted indirectly by checking it produces well-formed output and
//!   the three-way differential (separately) stays byte-identical.
//! - **distinct seeds diverge**: a different seed yields a different sequence.

/// A program that prints two random draws and the (virtual) clock — the canonical
/// non-determinism trio.
const RAND_TIME_PROGRAM: &str = r#"
import { random } from "std/math"
import { now } from "std/time"
print(random())
print(random())
print(now())
"#;

#[tokio::test]
async fn same_seed_same_output() {
    let a = ascript::run_source_deterministic(RAND_TIME_PROGRAM, 42)
        .await
        .expect("run a");
    let b = ascript::run_source_deterministic(RAND_TIME_PROGRAM, 42)
        .await
        .expect("run b");
    assert_eq!(a, b, "same seed must give byte-identical output");
    // Sanity: it actually produced three lines.
    assert_eq!(a.lines().count(), 3, "expected three printed lines");
}

#[tokio::test]
async fn distinct_seeds_diverge() {
    let a = ascript::run_source_deterministic(RAND_TIME_PROGRAM, 1)
        .await
        .expect("seed 1");
    let b = ascript::run_source_deterministic(RAND_TIME_PROGRAM, 2)
        .await
        .expect("seed 2");
    assert_ne!(a, b, "distinct seeds should produce distinct random output");
}

/// `time.sleep` in deterministic mode does NOT sleep real time — it advances the
/// virtual clock. A program that sleeps 10 minutes between two `now()` reads sees
/// the clock jump by 600000 ms, instantly (this test would take 10 real minutes if
/// the sleep were real).
#[tokio::test]
async fn deterministic_sleep_advances_virtual_clock_without_real_delay() {
    let src = r#"
import { now, sleep } from "std/time"
let t0 = now()
await sleep(600000)
let t1 = now()
print(t1 - t0)
"#;
    let started = std::time::Instant::now();
    let out = ascript::run_source_deterministic(src, 7)
        .await
        .expect("run");
    let elapsed = started.elapsed();
    // NUM §4: `time.now()` returns a float, so the elapsed delta prints "600000.0".
    assert_eq!(out.trim(), "600000.0", "virtual clock advanced by the slept ms");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "deterministic sleep must not block real time (took {elapsed:?})"
    );
}

/// Default (non-deterministic) mode is byte-identical to pre-SP9: the seams are
/// inert. A program that does NOT use random/clock has identical output whether or
/// not deterministic mode is entered (the deterministic run only differs on the
/// seamed values, which this program never reads).
#[tokio::test]
async fn default_mode_inert_for_non_seamed_program() {
    let src = "print(1 + 2)\nprint(\"hello\")\n";
    let plain = ascript::run_source(src).await.expect("plain");
    let det = ascript::run_source_deterministic(src, 123)
        .await
        .expect("deterministic");
    assert_eq!(
        plain, det,
        "a program that reads no clock/RNG is identical in both modes"
    );
}

/// SP9 §2.7 symmetry: under `--no-default-features` the `workflow` feature is
/// compiled out, so `import "std/workflow"` is an unknown-module error on BOTH
/// engines. (When the feature IS on, `tests/workflow.rs` exercises the real module.)
#[cfg(not(feature = "workflow"))]
#[tokio::test]
async fn workflow_module_absent_without_feature() {
    let src = "import { run } from \"std/workflow\"\n";
    let err = ascript::run_source(src).await.expect_err("must error");
    assert!(
        err.message.contains("std/workflow") || err.message.contains("unknown"),
        "expected an unknown-module error, got: {}",
        err.message
    );
}

/// uuid.v4 + crypto.randomBytes are reproducible under deterministic mode.
/// (`std/uuid` is `data`-gated, `std/crypto` is `crypto`-gated; under
/// `--no-default-features` they are compiled out, so this test is gated too.)
#[cfg(all(feature = "data", feature = "crypto"))]
#[tokio::test]
async fn deterministic_uuid_and_random_bytes_reproducible() {
    let src = r#"
import { v4 } from "std/uuid"
import { randomBytes } from "std/crypto"
print(v4())
print(randomBytes(8))
"#;
    let a = ascript::run_source_deterministic(src, 99).await.expect("a");
    let b = ascript::run_source_deterministic(src, 99).await.expect("b");
    assert_eq!(a, b, "same seed → identical uuid + random bytes");
    // A v4 UUID is 36 chars with the version-4 nibble.
    let first = a.lines().next().unwrap();
    assert_eq!(first.len(), 36);
    assert_eq!(&first[14..15], "4");
}

// ---------------------------------------------------------------------------
// REPLAY Task 0 (spec §10.2) — pin the ENGINE-SHARED determinism seam.
//
// `det.rs` is engine-shared by construction: every clock/RNG/uuid seam accessor
// lives on `Interp`, which BOTH the tree-walker (`run_source_deterministic`) and
// the bytecode VM (`vm_run_source_deterministic`) hold. The cross-engine
// record/replay matrix (record-on-A / replay-on-B) rests on this property, so it
// is pinned here BEFORE any new REPLAY code: the SAME seam, the SAME seed, on two
// engines → byte-identical output. A drift fails loudly.
// ---------------------------------------------------------------------------

/// A program touching the full non-determinism trio (clock + random) plus uuid,
/// so the engine-shared seam covers every accessor (`clock_now_ms`,
/// `next_random_f64`, `fill_seeded_bytes`).
#[cfg(feature = "data")]
const CROSS_ENGINE_DET_PROGRAM: &str = r#"
import { random } from "std/math"
import { now } from "std/time"
import { v4 } from "std/uuid"
print(random())
print(random())
print(now())
print(v4())
"#;

/// REPLAY Task 0 Step 1: the tree-walker det entry and the VM det entry, run with
/// the SAME seed, produce byte-identical output. This is the §10.2 cross-engine
/// determinism foundation — asserted, not assumed.
#[cfg(feature = "data")]
#[tokio::test]
async fn cross_engine_deterministic_output_is_byte_identical() {
    let tw = ascript::run_source_deterministic(CROSS_ENGINE_DET_PROGRAM, 42)
        .await
        .expect("tree-walker det run");
    let vm = ascript::vm_run_source_deterministic(CROSS_ENGINE_DET_PROGRAM, 42)
        .await
        .expect("VM det run");
    assert_eq!(
        tw, vm,
        "tree-walker and VM determinism seams must be byte-identical for the same seed"
    );
    // Sanity: it actually produced four lines (two randoms, a clock, a uuid).
    assert_eq!(tw.lines().count(), 4, "expected four printed lines");
}

/// REPLAY Task 0 Step 1: a distinct seed diverges on the VM det entry too (the seam
/// is genuinely seeded on the VM, not a no-op).
#[cfg(feature = "data")]
#[tokio::test]
async fn vm_deterministic_distinct_seeds_diverge() {
    let a = ascript::vm_run_source_deterministic(CROSS_ENGINE_DET_PROGRAM, 1)
        .await
        .expect("seed 1");
    let b = ascript::vm_run_source_deterministic(CROSS_ENGINE_DET_PROGRAM, 2)
        .await
        .expect("seed 2");
    assert_ne!(a, b, "distinct seeds must diverge on the VM det entry");
}

/// REPLAY Task 0 Step 2: documentation-by-test — pin that the serial `ascript test`
/// path runs on the TREE-WALKER `Interp`. The public `run_tests` entry routes through
/// `run_tests_serial` → `Interp::load_module` (the tree-walker loader, NOT the VM) →
/// `run_registered_tests_filtered`. A `.as` fixture registering one passing and one
/// failing `test(...)` exercises that exact wiring; the summary reflecting
/// 1-pass/1-fail proves the tree-walker test runner ran the registrations. REPLAY
/// Task 7 (`test --record/--replay`) builds on this loader+runner seam.
#[tokio::test]
async fn ascript_test_runs_on_tree_walker_interp() {
    // `test` and `assert` are GLOBAL builtins (no import); registering through them
    // exercises `register_test` + the tree-walker runner.
    let src = r#"
test("passes", () => { assert(1 + 1 == 2, "math works") })
test("fails", () => { assert(1 + 1 == 3, "intentional failure") })
"#;
    let file = std::env::temp_dir().join(format!(
        "ascript_replay_t0_testrunner_{}.as",
        std::process::id()
    ));
    std::fs::write(&file, src).unwrap();
    let summary = ascript::run_tests(&[file.to_string_lossy().into_owned()])
        .await
        .expect("test run");
    let _ = std::fs::remove_file(&file);
    // The tree-walker `load_module` + `run_registered_tests_filtered` ran both tests.
    assert_eq!(summary.passed, 1, "exactly one test passed");
    assert_eq!(summary.failed, 1, "exactly one test failed");
}
