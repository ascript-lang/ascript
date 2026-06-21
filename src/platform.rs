//! WASM §5.3.3 — the ONE home for raw ambient platform sources (clock, entropy,
//! sleep). This module sits **BELOW** the `det.rs` determinism seams: when a
//! determinism context is armed (`Interp::clock_now_ms`/`clock_monotonic_ms`/
//! `next_seeded_f64`), these functions are never consulted — they back only the
//! `None` (non-deterministic) branch. So SP9 Record/Replay is unchanged: the seams
//! stay above, the raw sources move here.
//!
//! **Native arms are byte-for-byte the previous inline implementations** (Gate W-1:
//! the differential corpus + `cargo test det` prove byte-identity). The wasm arms
//! use JS-backed sources (`Date.now`/`performance.now`/`getrandom(js)`/`setTimeout`)
//! so a `wasm32-unknown-unknown` build does not panic on the missing OS clock/entropy
//! (`SystemTime::now` / `Instant::now` are "not implemented on this platform").

/// A process-global start instant for [`monotonic_ms`], lazily initialized. Global
/// (not thread-local) so readings are comparable across threads under a multi-thread
/// runtime. Moved here verbatim from `stdlib/time.rs` (it backs `time.monotonic` via
/// `time::real_monotonic_ms`, unchanged on native).
#[cfg(not(target_family = "wasm"))]
static START: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// The real wall clock in ms since the Unix epoch. Backs `interp::real_now_ms`
/// (the `None`-mode fallback for `time.now`/`date.now`). Saturates to 0 on a
/// pre-epoch clock — identical to the previous inline `SystemTime` body.
#[cfg(not(target_family = "wasm"))]
pub fn now_unix_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}
#[cfg(target_family = "wasm")]
pub fn now_unix_ms() -> f64 {
    js_sys::Date::now()
}

/// The real monotonic clock in ms since process start. Backs
/// `time::real_monotonic_ms` (the `None`-mode fallback for `time.monotonic`).
/// Identical to the previous inline `START.elapsed()` body.
#[cfg(not(target_family = "wasm"))]
pub fn monotonic_ms() -> f64 {
    START.elapsed().as_secs_f64() * 1000.0
}
#[cfg(target_family = "wasm")]
pub fn monotonic_ms() -> f64 {
    use wasm_bindgen::JsCast;
    // `performance.now()` is monotonic ms since the page/worker context start. Fall
    // back to `Date.now()` if `performance` is unavailable (older / restricted hosts).
    js_sys::Reflect::get(&js_sys::global(), &"performance".into())
        .ok()
        .and_then(|p| {
            js_sys::Reflect::get(&p, &"now".into())
                .ok()
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                .and_then(|f| f.call0(&p).ok())
                .and_then(|v| v.as_f64())
        })
        .unwrap_or_else(js_sys::Date::now)
}

/// A non-zero seed for the ambient (non-deterministic) `math.random()` PRNG. Native:
/// today's `math.rs seed()` body verbatim — system-clock nanos XOR a stack address,
/// `.max(1)` to stay non-zero. NOT cryptographic. (In deterministic mode the PRNG is
/// seeded by the `det.rs` `SeededRng`, never this.)
#[cfg(not(target_family = "wasm"))]
pub fn entropy_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    let local = 0u8;
    let addr = &local as *const u8 as u64;
    (nanos ^ addr).max(1)
}
#[cfg(target_family = "wasm")]
pub fn entropy_seed() -> u64 {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).expect("getrandom(js) for math.random seed");
    u64::from_le_bytes(b) | 1
}

/// Async sleep for `ms` milliseconds. Native: `tokio::time::sleep` (unchanged). Wasm:
/// a hand-rolled `setTimeout` future (no `gloo` dep) — the JS callback wakes the task,
/// which works because the whole program future is polled by the browser microtask
/// loop (§5.3.6). Contract: resolves after ≥`ms` ms.
#[cfg(not(target_family = "wasm"))]
pub async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}
#[cfg(target_family = "wasm")]
pub async fn sleep_ms(ms: u64) {
    use wasm_bindgen::JsCast;
    let p = js_sys::Promise::new(&mut |resolve, _reject| {
        let g = js_sys::global();
        if let Ok(set_timeout) = js_sys::Reflect::get(&g, &"setTimeout".into()) {
            if let Ok(f) = set_timeout.dyn_into::<js_sys::Function>() {
                let _ = f.call2(&g, &resolve, &(ms as f64).into());
            }
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn now_unix_ms_matches_systemtime() {
        // The native arm must be byte-for-byte the previous inline `SystemTime` body:
        // within a few seconds of a freshly-computed wall clock.
        let inline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0);
        let got = now_unix_ms();
        assert!(
            (got - inline).abs() < 5000.0,
            "now_unix_ms {got} should be within 5s of inline SystemTime {inline}"
        );
        assert!(got > 1_600_000_000_000.0, "epoch-ms should be a real wall clock");
    }

    #[test]
    fn monotonic_ms_is_monotone() {
        let a = monotonic_ms();
        // Spin briefly so the second reading is strictly later (the clock advances).
        let mut spin = 0u64;
        while monotonic_ms() <= a && spin < 100_000_000 {
            spin = spin.wrapping_add(1);
        }
        let b = monotonic_ms();
        assert!(b >= a, "monotonic_ms must not go backwards: {a} -> {b}");
    }

    #[test]
    fn entropy_seed_nonzero() {
        // The ONLY contract `math.rs seed()` guarantees (and all the PRNG needs): a
        // NON-ZERO seed. Variance is per-RUN (wall-clock nanos differ across process
        // launches), NOT per-CALL: two adjacent calls within the same nanosecond share
        // the same `nanos` AND the same stack address, so `a != b` is NOT guaranteed
        // (it collides the large majority of the time) and must never be asserted — that
        // was a flaky assertion. The seed is timing-only and never an observable script
        // value (deterministic mode seeds the PRNG from `det.rs SeededRng`, never this),
        // so per-call variance is irrelevant to correctness.
        assert_ne!(entropy_seed(), 0, "seed must be non-zero (PRNG state must never be 0)");
        assert_ne!(entropy_seed(), 0);
    }

    #[tokio::test]
    async fn sleep_ms_waits_at_least_the_duration() {
        let t0 = std::time::Instant::now();
        sleep_ms(10).await;
        assert!(
            t0.elapsed() >= std::time::Duration::from_millis(10),
            "sleep_ms(10) must wait >= 10ms"
        );
    }
}
