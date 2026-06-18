//! RESIL Phase 0 — preflight pins (spec §1).
//!
//! These tests document the **shipped behavior** that the `std/resilience` design
//! composes on top of.  They must PASS today (they reflect current ground truth);
//! a failure means the design's foundation moved → STOP and report.
//!
//! Pins:
//!  1. `ASO_FORMAT_VERSION` — RESIL is a pure-stdlib addition; no new opcode,
//!     no new bytecode layout.  Pinned at 29 (ELIDE bumped 28 → 29).
//!  2. `task.retry` v1 contract — the three shipped retry semantics that RESIL
//!     builds on (spec §3.4): success on Kth attempt, exhaustion + re-raise,
//!     no-retry of error pairs.
//!  3. Substrate pins — SharedFuture multi-await, panicking-future double-await,
//!     `sync.semaphore` acquire/release round-trip, `lru` eviction.

// ──────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Run an `.as` source string through the VM and return captured stdout.
/// Mirrors the idiom used in `src/stdlib/task_mod.rs` unit tests.
async fn run(src: &str) -> String {
    ascript::run_source(src)
        .await
        .unwrap_or_else(|e| panic!("program should run cleanly; got error: {}", e.message))
}

// ──────────────────────────────────────────────────────────────────────────────
// Pin 1 — `.aso` format version
// ──────────────────────────────────────────────────────────────────────────────

/// RESIL must NOT bump `ASO_FORMAT_VERSION`.
///
/// `std/resilience` is a pure-stdlib module: new exported functions dispatch
/// through the existing opcode set with no new `FnProto` field, no new
/// serialised layout, and no new bytecode constant format.
///
/// History:
///   - DEFER bumped 27 → 28 (DeferPush / DeferPushMethod opcodes).
///   - ELIDE bumped 28 → 29 (Op::CallElided opcode).
///   - RESIL bumps NOTHING.
#[test]
fn aso_format_version_unchanged_by_resil() {
    assert_eq!(
        ascript::vm::aso::ASO_FORMAT_VERSION,
        29,
        "RESIL must not bump ASO_FORMAT_VERSION (pinned at the value found on branch creation); \
         if another spec legitimately bumped it, update this literal in THAT spec's branch"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Pin 2 — task.retry v1 contract
// ──────────────────────────────────────────────────────────────────────────────
//
// Mirrors the three unit tests in src/stdlib/task_mod.rs:
//   retry_succeeds_on_third_attempt
//   retry_exhausts_and_reraises
//   retry_does_not_retry_error_pairs
//
// These are integration-level copies so that a future edit to task_mod.rs cannot
// silently weaken retry-v1 semantics without failing these tests.

/// retry returns the success value when the fn panics K-1 times then succeeds.
///
/// Ground truth from task_mod.rs `retry_succeeds_on_third_attempt`:
///   retry(flaky, {attempts:5, baseMs:1}) where flaky panics twice then returns "ok"
///   → prints "ok" then "3" (the call count).
#[tokio::test]
async fn retry_v1_succeeds_on_third_attempt() {
    let out = run(r#"
import { retry } from "std/task"
let counter = [0]
async fn flaky() {
    counter[0] = counter[0] + 1
    if (counter[0] < 3) {
        assert(false, "not yet")
    }
    return "ok"
}
let result = await retry(flaky, {attempts: 5, baseMs: 1})
print(result)
print(counter[0])
"#)
    .await;
    assert_eq!(out, "ok\n3\n");
}

/// retry exhausts all attempts and re-raises the last panic.
///
/// Ground truth from task_mod.rs `retry_exhausts_and_reraises`:
///   retry(always_fails, {attempts:3, baseMs:1}) where always_fails always panics
///   → after 3 attempts the panic surfaces; counter == 3.
#[tokio::test]
async fn retry_v1_exhausts_and_reraises() {
    let out = run(r#"
import { retry } from "std/task"
let counter = [0]
async fn always_fails() {
    counter[0] = counter[0] + 1
    assert(false, "always bad")
}
let [v, err] = recover(() => {
    await retry(always_fails, {attempts: 3, baseMs: 1})
    return nil
})
print(v)
print(err != nil)
print(counter[0])
"#)
    .await;
    assert_eq!(out, "nil\ntrue\n3\n");
}

/// retry does NOT retry a Tier-1 error-pair return; the pair is returned immediately.
///
/// Ground truth from task_mod.rs `retry_does_not_retry_error_pairs`:
///   retry(returns_err_pair, {attempts:5, baseMs:1}) where the fn returns [nil, {…}]
///   → result is the array, type "array"; counter == 1 (called exactly once, no retry).
#[tokio::test]
async fn retry_v1_does_not_retry_error_pairs() {
    let out = run(r#"
import { retry } from "std/task"
let counter = [0]
async fn returns_err_pair() {
    counter[0] = counter[0] + 1
    return [nil, {message: "user error"}]
}
let result = await retry(returns_err_pair, {attempts: 5, baseMs: 1})
print(type(result))
print(counter[0])
"#)
    .await;
    // result is the [nil, {message:…}] array; type "array"; counter == 1 (no retry).
    assert_eq!(out, "array\n1\n");
}

// ──────────────────────────────────────────────────────────────────────────────
// Pin 3 — substrate pins
// ──────────────────────────────────────────────────────────────────────────────

/// A `SharedFuture` awaited by TWO awaiters delivers the same value to both.
///
/// The ResultCell stores the result and clones it out on each `get()` call, so
/// awaiting the same future variable twice must yield identical results.
/// RESIL's circuit-breaker probe pattern (await the probe future, then await
/// the result future) relies on this being sound.
#[tokio::test]
async fn substrate_shared_future_multi_await_same_value() {
    let out = run(r#"
async fn compute() {
    return 42
}
let f = compute()
let a = await f
let b = await f
print(a)
print(b)
print(a == b)
"#)
    .await;
    assert_eq!(out, "42\n42\ntrue\n");
}

/// A panicking async fn awaited twice delivers the same panic message to both
/// `recover` calls (the ResultCell stores the Control::Panic and clones it out).
#[tokio::test]
async fn substrate_panicking_future_multi_await_both_panic() {
    let out = run(r#"
async fn boom() {
    assert(false, "kaboom")
}
let f = boom()
let [v1, e1] = recover(() => await f)
let [v2, e2] = recover(() => await f)
print(v1)
print(v2)
print(e1 != nil)
print(e2 != nil)
print(e1.message == e2.message)
"#)
    .await;
    assert_eq!(out, "nil\nnil\ntrue\ntrue\ntrue\n");
}

/// `sync.semaphore` acquire/release round-trip: capacity-1 semaphore can be
/// acquired, released, and acquired again — the second acquire succeeds.
/// RESIL uses a semaphore for the circuit-breaker concurrency gate.
#[tokio::test]
async fn substrate_semaphore_acquire_release_roundtrip() {
    let out = run(r#"
import { semaphore, acquire, release } from "std/sync"
let s = semaphore(1)
await acquire(s)
release(s)
await acquire(s)
print("ok")
"#)
    .await;
    assert_eq!(out, "ok\n");
}

/// `lru` eviction: a capacity-2 cache evicts the least-recently-used entry on
/// the third `set`.  The evicted key returns nil/miss on `get`.
/// RESIL uses an LRU cache for the adaptive-timeout warm history.
#[tokio::test]
async fn substrate_lru_eviction_evicts_lru_entry() {
    let out = run(r#"
import { new } from "std/lru"
let cache = new(2)
cache.set("a", 1)
cache.set("b", 2)
cache.set("c", 3)
print(cache.get("a"))
print(cache.has("b"))
print(cache.has("c"))
"#)
    .await;
    // "a" was inserted first and never promoted, so it is LRU → evicted on set("c").
    // get("a") returns nil (miss); "b" and "c" survive.
    assert_eq!(out, "nil\ntrue\ntrue\n");
}
