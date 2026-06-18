//! RESIL §8 holistic-review probe (added by the final independent reviewer).
//!
//! The §8 claim is that resilience policies that consult the monotonic clock
//! (breaker cooldown, memoize TTL) route that clock through the determinism
//! seam (`Interp::clock_monotonic_ms`), so under Record/Replay (the
//! `run_source_deterministic` virtual-clock path) the same program with the
//! same seed replays byte-identically — and `time.sleep` advances the VIRTUAL
//! clock so a cooldown/TTL boundary is crossed instantly and deterministically.
//!
//! These tests would have been the missing §8 evidence: they assert (a)
//! same-seed byte-identity for a breaker-cooldown→half-open scenario and a
//! memoize-TTL-expiry scenario, and (b) that the virtual clock actually drives
//! the policy clocks (the sleep crosses the boundary with no real delay).

/// A breaker that opens on 2 failures, then a virtual-clock sleep past the
/// cooldown re-admits a probe call (half-open) which succeeds and re-closes it.
/// Run twice with the same seed → byte-identical (the cooldown clock is
/// det-routed). If the breaker consulted the REAL clock instead of the virtual
/// one, the 50ms sleep would not deterministically cross a 5000ms cooldown.
const BREAKER_COOLDOWN: &str = r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
fn fail() { return [nil, {message: "down", code: "err"}] }
fn ok() { return 7 }
let b = resilience.breaker({name: "p", failureRate: 0.5, window: 2, minCalls: 2, cooldownMs: 5000, halfOpenMax: 1})
b.call(fail)
b.call(fail)
print(b.state())
await time.sleep(6000)
let [v, e] = b.call(ok)
print(v)
print(b.state())
"#;

/// A memoize with a 1000ms TTL: first call computes, second (immediate) hits the
/// cache, then a virtual-clock sleep past the TTL forces a recompute. The hit
/// counter proves the TTL boundary was crossed via the det-routed clock.
const MEMOIZE_TTL: &str = r#"
import * as resilience from "std/resilience"
import * as time from "std/time"
let n = [0]
async fn load() { n[0] = n[0] + 1; return 10 }
let m = resilience.memoize({ttlMs: 1000})
let [a, _ea] = await m.get("k", load)
print(a)
let [b, _eb] = await m.get("k", load)
print(b)
await time.sleep(1500)
let [c, _ec] = await m.get("k", load)
print(c)
print(n[0])
"#;

#[tokio::test]
async fn breaker_cooldown_replays_byte_identically() {
    let a = ascript::run_source_deterministic(BREAKER_COOLDOWN, 99)
        .await
        .expect("run a");
    let b = ascript::run_source_deterministic(BREAKER_COOLDOWN, 99)
        .await
        .expect("run b");
    assert_eq!(a, b, "breaker-cooldown scenario must replay byte-identically");
    // The breaker opened (2 failures), then the half-open probe succeeded after
    // the virtual-clock cooldown elapsed.
    assert!(a.starts_with("open\n"), "breaker should open after 2 failures: {a:?}");
    assert!(a.contains("\n7\n"), "half-open probe should succeed (return 7): {a:?}");
    assert!(a.trim_end().ends_with("closed"), "breaker should re-close after a successful probe: {a:?}");
}

#[tokio::test]
async fn memoize_ttl_boundary_replays_byte_identically() {
    let a = ascript::run_source_deterministic(MEMOIZE_TTL, 7)
        .await
        .expect("run a");
    let b = ascript::run_source_deterministic(MEMOIZE_TTL, 7)
        .await
        .expect("run b");
    assert_eq!(a, b, "memoize-TTL scenario must replay byte-identically");
    // exactly two computes: the first call + the post-TTL recompute (the
    // immediate second call was a cache hit).
    assert!(a.trim_end().ends_with("\n2"), "TTL must expire after the virtual sleep → 2 computes: {a:?}");
}
