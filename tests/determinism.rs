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
    assert_eq!(out.trim(), "600000", "virtual clock advanced by the slept ms");
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
