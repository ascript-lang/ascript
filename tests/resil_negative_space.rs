//! RESIL Phase 0 + Phase 6 — preflight pins (spec §1) + negative-space pins (spec §5.2).
//!
//! These tests document the **shipped behavior** that the `std/resilience` design
//! composes on top of.  They must PASS today (they reflect current ground truth);
//! a failure means the design's foundation moved → STOP and report.
//!
//! Phase 0 Pins:
//!  1. `ASO_FORMAT_VERSION` — RESIL is a pure-stdlib addition; no new opcode,
//!     no new bytecode layout.  Pinned at 29 (ELIDE bumped 28 → 29).
//!  2. `task.retry` v1 contract — the three shipped retry semantics that RESIL
//!     builds on (spec §3.4): success on Kth attempt, exhaustion + re-raise,
//!     no-retry of error pairs.
//!  3. Substrate pins — SharedFuture multi-await, panicking-future double-await,
//!     `sync.semaphore` acquire/release round-trip, `lru` eviction.
//!
//! Phase 6 Pins (Task 6.2 — negative-space):
//!  4. Opcode count unchanged at 121 (RESIL adds no opcode — pure stdlib).
//!     Reuses the `from_u8` filter technique from `tests/par_negative_space.rs`.
//!  5. Hook-order pin — schema's call-site hook fires BEFORE resilience's.
//!     A dual-tagged object (`__kind` ∈ SCHEMA_KINDS, `__resil` ∈ RESIL_KINDS)
//!     routes to the schema hook when a schema method is called, because
//!     `member_call_is_hook` (and `call_method_recv`) check schema FIRST (spec §2.3).
//!  6. `OptMember` non-routing pin — `policy?.method(...)` does NOT route through
//!     the resilience call-site hook; `?.` compiles to `GetPropOpt` (field read,
//!     nil-short-circuits) then `Call`, never `CallMethod`, so the hook is bypassed.

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

// ──────────────────────────────────────────────────────────────────────────────
// Phase 6 / Task 6.2 — negative-space pins
// ──────────────────────────────────────────────────────────────────────────────

// ── Pin 4 — opcode count unchanged ───────────────────────────────────────────

/// RESIL adds NO new opcode — it is a pure-stdlib module dispatching through
/// the existing call machinery.  The `Op` variant count must remain at 121
/// (DEFER added `DeferPush`/`DeferPushMethod` → 120; ELIDE added `CallElided`
/// → 121; RESIL contributes NOTHING).
///
/// Uses the `from_u8` filter technique from `tests/par_negative_space.rs` and
/// `tests/shape_negative_space.rs`: a new opcode is only reachable once it has
/// a `from_u8` arm, so counting reachable discriminants catches any variant
/// inserted anywhere in the enum — not just appended at the end.
#[test]
fn no_new_opcode_for_resil() {
    /// Pre-RESIL opcode count (and current value — RESIL adds none).
    /// DEFER added DeferPush + DeferPushMethod (→ 120).
    /// ELIDE added CallElided (→ 121).
    /// Update this constant only if a NON-RESIL feature adds a new opcode.
    const EXPECTED_OP_COUNT: usize = 121;

    let op_count = (0u16..=255)
        .filter(|&b| ascript::vm::opcode::Op::from_u8(b as u8).is_some())
        .count();

    assert_eq!(
        op_count, EXPECTED_OP_COUNT,
        "Op variant count changed — RESIL must add NO opcodes (resilience policies \
         dispatch through existing call/member machinery); update EXPECTED_OP_COUNT \
         only if a non-RESIL feature adds the new opcode."
    );

    // Complementary: `CallElided` (discriminant 120) is still the last variant.
    assert_eq!(
        ascript::vm::opcode::Op::CallElided as u16 + 1,
        EXPECTED_OP_COUNT as u16,
        "CallElided is no longer the last Op variant — a new opcode was appended \
         after it; update EXPECTED_OP_COUNT if it is NOT attributable to RESIL."
    );
}

// ── Pin 5 — hook-order: schema fires BEFORE resilience ───────────────────────

/// A value dual-tagged with BOTH a `__kind` (schema) and a `__resil` (resilience)
/// field routes to the SCHEMA hook — NOT the resilience hook — when a schema
/// method is called.
///
/// Ground truth from `src/interp.rs` `member_call_is_hook` / `call_method_recv`
/// (and the mirror in `src/vm/run.rs` `shared_method_dispatch`):
///   1. Schema check: `is_schema_value(recv) && is_schema_method(name)` — schema kinds
///      are {"string","number","bool","nil","any","literal","array","object","map",
///      "optional","union","oneOf"} (keyed on `__kind`); schema methods include
///      `parse`, `minLength`, `maxLength`, `min`, `max`, `default`, etc.
///   2. Resilience check: `is_resilience_value(recv) && is_resilience_method(name)` —
///      resilience kinds are {"breaker","limiter","keyedLimiter","bulkhead","retry",
///      "memoize"} (keyed on `__resil`); resilience methods are `call`, `state`,
///      `stats`, `reset`, `acquire`, `tryAcquire`, `run`, `get`, `delete`, `clear`,
///      `len`.
///
/// The two METHOD SETS are DISJOINT (no name appears in both), so the hook-order
/// is only observable via the PREDICATE check order on the receiver.  We construct
/// a dual-tagged object: `__kind: "string"` (schema-tagged) AND `__resil: "breaker"`
/// (resilience-tagged).  Calling `.parse(42)` on it:
///   - Schema check: `is_schema_value` → true (`__kind:"string"` ∈ SCHEMA_KINDS) AND
///     `is_schema_method("parse")` → true.  Schema wins → routes to `call_schema`.
///   - Resilience check would have won if schema were absent/checked second.
///
/// The schema `"string".parse(42)` call validates 42 as a string and returns the
/// Tier-1 error pair `[nil, err]`.  We assert the err is non-nil, proving schema
/// (not resilience) handled the call — a resilience dispatch would have panicked
/// (the recv has no real resilience state) rather than returning a pair.
#[cfg(feature = "resilience")]
#[tokio::test]
async fn hook_order_schema_wins_over_resilience_on_dual_tagged_value() {
    let out = run(r#"
import * as schema from "std/schema"

// Build a value that satisfies BOTH is_schema_value (has __kind:"string" which
// is in SCHEMA_KINDS) AND is_resilience_value (has __resil:"breaker" which is
// in RESIL_KINDS).  Constructing it raw — no stdlib call — so neither hook
// fires during construction.
let dual = {__kind: "string", __resil: "breaker", __state: "closed"}

// Call .parse(42) on it.  Schema method "parse" + schema predicate ("string" kind)
// → schema hook fires first.  schema."string".parse(42) returns [nil, err] because
// 42 is not a string.  If the resilience hook had fired instead it would have panicked
// (dual has no real resilience machinery).
let [v, err] = dual.parse(42)
print(v)
print(err != nil)
print(type(err))
"#)
    .await;
    // schema hook fired: parse returned the Tier-1 pair.
    // v=nil, err=object (the parse-error record), type "object".
    assert_eq!(out, "nil\ntrue\nobject\n");
}

// ── Pin 6 — OptMember (?) does NOT route through the resilience hook ──────────

/// `policy?.method(...)` with a non-nil policy does NOT route through the
/// resilience call-site hook; it falls back to the generic field-read → call path.
///
/// `?.` on a non-nil receiver is lowered by the compiler to `GetPropOpt(name)`
/// (a nil-short-circuiting field READ) then a plain `Call` — NOT `CallMethod`.
/// The resilience hook lives only on `CallMethod` / `ExprKind::Call { callee:
/// Member { .. } }` (plain `.`), so `?.` bypasses it entirely.
///
/// Observable consequence: `breaker?.state()` does NOT invoke the `state` method
/// via the hook.  Instead it reads the stored field named `"state"` from the
/// breaker object (absent — resilience objects store `"__state"`, not `"state"`),
/// gets nil, and calls nil — which is a recoverable Tier-2 panic.
///
/// We verify:
///   (a) `policy.state()` via the PLAIN `.` hook returns "closed" (hook fires, ok).
///   (b) `policy?.state()` panics — the field is absent → nil → not callable.
///
/// This pins that `?.` is NOT a transparent alias for `.` on hook-dispatched receivers.
#[cfg(feature = "resilience")]
#[tokio::test]
async fn opt_member_does_not_route_through_resilience_hook() {
    // (a) Plain dot call — hook fires, returns the current state string.
    let out_plain = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({})
print(b.state())
"#)
    .await;
    assert_eq!(out_plain, "closed\n", "plain .state() should route through hook");

    // (b) Optional-chaining call — bypasses the hook; reads "state" field (absent),
    // gets nil, calling nil panics.  Wrap in recover to observe the error.
    let out_opt = run(r#"
import * as resilience from "std/resilience"
let b = resilience.breaker({})
let [v, err] = recover(() => b?.state())
print(v)
print(err != nil)
"#)
    .await;
    // v=nil (panic), err=non-nil (panic message).
    assert_eq!(
        out_opt, "nil\ntrue\n",
        "?.state() should NOT route through the hook; nil field → call nil → panic"
    );
}
