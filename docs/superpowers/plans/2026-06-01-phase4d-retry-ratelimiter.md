# Phase 4d: task.retry + sync.rateLimiter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `task.retry(fn, opts?)` (exponential-backoff retry on panic) and `sync.rateLimiter(opts)` / `limiter.acquire()` (token-bucket rate limiter) to the AScript standard library.

**Architecture:** `task.retry` lives in `src/stdlib/task_mod.rs` and catches `Control::Panic` from `call_value`, sleeping between attempts via `tokio::time::sleep`; it retries on panic only (NOT on returned `[nil, err]` pairs). `sync.rateLimiter` lives in `src/stdlib/sync.rs` and is backed by a token-bucket: a `RefCell<(usize, Instant)>` counter reset by monotonic clock comparison (no spawned refill task — avoids a background task that could outlive the handle). The handle is a `NativeKind::RateLimiter` resource; `.acquire()` is dispatched via `call_native_method` → `call_rate_limiter_method`.

**Tech Stack:** Rust, tokio (sleep, time::Instant), existing `Rc<RefCell>` + `Rc<Notify>` pattern (mirrors semaphore), `Control::Panic` catch pattern (mirrors `recover`).

---

## Files to create / modify

| Action   | File                                  | What changes                                                                 |
|----------|---------------------------------------|------------------------------------------------------------------------------|
| Modify   | `src/value.rs`                        | Add `NativeKind::RateLimiter` variant + type_name arm                        |
| Modify   | `src/interp.rs`                       | Add `ResourceState::RateLimiter(...)` variant + dispatch arm in `call_native_method` for `RateLimiter` |
| Modify   | `src/stdlib/task_mod.rs`              | Add `task.retry` export, dispatch arm, `task_retry` impl                    |
| Modify   | `src/stdlib/sync.rs`                  | Add `RateLimiter` struct + `ResourceState::RateLimiter`, `sync.rateLimiter` export + dispatch + impl, `call_rate_limiter_method` |

---

## Task 1: Write the failing tests for `task.retry`

**Files:**
- Modify: `src/stdlib/task_mod.rs` (add tests at bottom)

- [ ] **Step 1.1: Add the failing retry tests**

Add a `#[cfg(test)] mod tests` block (or extend the existing one if present) at the end of `src/stdlib/task_mod.rs`:

```rust
#[cfg(test)]
mod tests {
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    // ── retry: succeeds on Kth attempt ──────────────────────────────────────
    // A fn that panics K-1 times then succeeds. With {attempts:5, baseMs:1}
    // the call should return the success value and the counter should be K.

    #[tokio::test]
    async fn retry_succeeds_on_third_attempt() {
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
"#).await;
        assert_eq!(out, "ok\n3\n");
    }

    // ── retry: exhausts attempts → re-raises last panic ─────────────────────
    // A fn that always panics. retry with {attempts:3, baseMs:1} should
    // exhaust all 3 attempts then re-raise the last panic (caught by recover).

    #[tokio::test]
    async fn retry_exhausts_and_reraises() {
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
"#).await;
        assert_eq!(out, "nil\ntrue\n3\n");
    }

    // ── retry: returns immediately on success (no retry of ok [nil,err] pair) ─
    // fn returns a [nil, err] pair (a Tier-1 error pair, NOT a panic).
    // retry must NOT retry this — it returns the pair immediately.

    #[tokio::test]
    async fn retry_does_not_retry_error_pairs() {
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
"#).await;
        // result is the array [nil, {message:...}], type is "array"; counter == 1 (no retry)
        assert_eq!(out, "array\n1\n");
    }
}
```

- [ ] **Step 1.2: Verify the tests fail**

```bash
cd /Users/mahmoud/ascript && cargo test retry --no-default-features 2>&1 | tail -20
```

Expected: compile error or FAILED — `retry` is not yet exported.

---

## Task 2: Write the failing tests for `sync.rateLimiter`

**Files:**
- Modify: `src/stdlib/sync.rs` (append to existing `#[cfg(test)] mod tests`)

- [ ] **Step 2.1: Add the failing rateLimiter tests**

Append inside the existing `mod tests` block in `src/stdlib/sync.rs`:

```rust
    // ══════════════════════════════════════════════════════════════════════════
    // rateLimiter tests
    // ══════════════════════════════════════════════════════════════════════════

    // ── basic: two acquires succeed immediately, 3rd waits for window refill ─
    // {count:2, windowMs:50}: first two acquires are instant; the 3rd must wait
    // until the 50ms window resets.  We assert ordering via a timestamp check
    // (elapsed >= ~40ms, tolerant) and a printed sequence.

    #[tokio::test]
    async fn rate_limiter_basic_count_window() {
        let out = run(r#"
import { rateLimiter } from "std/sync"
import * as time from "std/time"

let lim = rateLimiter({count: 2, windowMs: 50})
await lim.acquire()
await lim.acquire()
let before = time.now()
await lim.acquire()          // must wait for window reset
let elapsed = time.now() - before
print(elapsed >= 30)         // tolerant: at least 30ms
"#).await;
        assert_eq!(out, "true\n");
    }

    // ── perSecond sugar: {perSecond:N} is {count:N, windowMs:1000} ────────────

    #[tokio::test]
    async fn rate_limiter_per_second_alias() {
        let out = run(r#"
import { rateLimiter } from "std/sync"
// N=1000 perSecond so the window fills quickly; just test it returns a handle
let lim = rateLimiter({perSecond: 1000})
await lim.acquire()
print("ok")
"#).await;
        assert_eq!(out, "ok\n");
    }
```

- [ ] **Step 2.2: Verify the tests fail**

```bash
cd /Users/mahmoud/ascript && cargo test rate_limiter --no-default-features 2>&1 | tail -20
```

Expected: compile error or FAILED — `rateLimiter` is not yet exported.

---

## Task 3: Add `NativeKind::RateLimiter` to `src/value.rs`

**Files:**
- Modify: `src/value.rs`

- [ ] **Step 3.1: Add the variant**

In the `NativeKind` enum (around line 176, after `ThrottleWrapper`), add:

```rust
    // std/sync: a token-bucket rate limiter. `.acquire()` awaits a token; the
    // bucket refills `count` tokens every `window_ms` milliseconds (monotonic
    // clock — no background task). Not feature-gated.
    RateLimiter,
```

- [ ] **Step 3.2: Add the `type_name` arm**

In `impl NativeKind { fn type_name(self) }` (right after the `ThrottleWrapper` arm), add:

```rust
            NativeKind::RateLimiter => "rateLimiter",
```

- [ ] **Step 3.3: Build to check it compiles**

```bash
cd /Users/mahmoud/ascript && cargo build --no-default-features 2>&1 | grep -E "^error" | head -10
```

Expected: error about non-exhaustive match in `interp.rs` (we haven't added the `ResourceState` variant yet) OR clean compile if the match is not exhaustive-checked yet.

---

## Task 4: Add `ResourceState::RateLimiter` to `src/interp.rs`

**Files:**
- Modify: `src/interp.rs`

- [ ] **Step 4.1: Add the `RateLimiterState` struct import and `ResourceState` variant**

First, understand the `RateLimiterState` we'll define in `sync.rs` (Task 5). The state holds:
- `count: usize` — tokens per window
- `window_ms: u64` — window size
- `available: Rc<RefCell<usize>>` — current tokens  
- `window_start: Rc<RefCell<std::time::Instant>>` — when the current window started
- `token_available: Rc<tokio::sync::Notify>` — wakes parked `.acquire()` callers

Add the `ResourceState` variant. In `src/interp.rs`, find the line `Semaphore(crate::stdlib::sync::Semaphore),` (around line 203) and add after it:

```rust
    // std/sync: a token-bucket rate limiter. count tokens per window_ms ms.
    // Available tokens + window_start live in RefCells; Notify for wakeups.
    RateLimiter(crate::stdlib::sync::RateLimiterState),
```

- [ ] **Step 4.2: Add dispatch to `call_native_method`**

In `src/interp.rs`, find the block that dispatches `Interval`, `DebounceWrapper`, `ThrottleWrapper` (around line 2008-2017), and add after the `ThrottleWrapper` arm:

```rust
            if matches!(m.receiver.kind, RateLimiter) {
                return self.call_rate_limiter_method(&m, args, span).await;
            }
```

Note: the `use crate::value::NativeKind::*;` is already in scope for that block.

- [ ] **Step 4.3: Build to check it compiles**

```bash
cd /Users/mahmoud/ascript && cargo build --no-default-features 2>&1 | grep -E "^error" | head -10
```

Expected: error about `call_rate_limiter_method` not existing yet (we'll add it in Task 5) OR errors about `RateLimiterState` not defined yet. Both are expected at this stage.

---

## Task 5: Implement `sync.rateLimiter` in `src/stdlib/sync.rs`

**Files:**
- Modify: `src/stdlib/sync.rs`

### 5a — Add `RateLimiterState` struct

- [ ] **Step 5.1: Add imports and struct definition**

After the `Semaphore` struct (around line 70), add:

```rust
// ── RateLimiter data structures ───────────────────────────────────────────────

/// A token-bucket rate limiter: `count` tokens per `window_ms` milliseconds.
///
/// Uses a monotonic-clock bucket: each `.acquire()` call checks whether the
/// window has elapsed (refilling if so) then decrements. If no tokens are
/// available the caller parks on `token_available` until another acquire
/// loop iteration notices a refill (a background wakeup is sent by a
/// `spawn_local` timer task created per-park — see `acquire` impl).
///
/// State layout mirrors `Semaphore`: `Rc` fields are cloned out before any
/// `.await` so no `RefCell` borrow is held across an await (borrow discipline).
pub struct RateLimiterState {
    /// Tokens per window.
    pub count: usize,
    /// Window size in milliseconds.
    pub window_ms: u64,
    /// Currently available tokens (0 ..= count). Behind a `RefCell` because
    /// multiple acquire futures may observe/mutate it concurrently on the
    /// single-thread runtime.
    pub available: Rc<RefCell<usize>>,
    /// When the current window started. `Instant` is `Copy` so we can snapshot
    /// it without holding the borrow.
    pub window_start: Rc<RefCell<std::time::Instant>>,
    /// Fires when tokens become available (either a refill or an acquire that
    /// finds tokens left for a competing caller). Stored *outside* any RefCell
    /// so it can be cloned and awaited without holding a borrow.
    pub token_available: Rc<tokio::sync::Notify>,
}

impl RateLimiterState {
    pub fn new(count: usize, window_ms: u64) -> Self {
        RateLimiterState {
            count,
            window_ms,
            available: Rc::new(RefCell::new(count)),
            window_start: Rc::new(RefCell::new(std::time::Instant::now())),
            token_available: Rc::new(tokio::sync::Notify::new()),
        }
    }
}
```

### 5b — Add export and dispatch

- [ ] **Step 5.2: Add `rateLimiter` to `exports()`**

In the `exports()` function, add:

```rust
        ("rateLimiter", bi("sync.rateLimiter")),
```

- [ ] **Step 5.3: Add dispatch arm in `call_sync`**

In `call_sync`, add:

```rust
            "rateLimiter" => self.sync_rate_limiter(args, span),
```

### 5c — Implement `sync_rate_limiter` constructor

- [ ] **Step 5.4: Add `sync_rate_limiter` method to `impl Interp`**

Add after `sync_available`:

```rust
    // ── sync.rateLimiter ──────────────────────────────────────────────────────

    fn sync_rate_limiter(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        // Accept {perSecond: N} or {count: N, windowMs: M}
        let opts = arg(args, 0);
        let (count, window_ms) = match &opts {
            Value::Object(obj) => {
                let o = obj.borrow();
                if let Some(ps) = o.get("perSecond") {
                    let n = want_number(ps, span, "sync.rateLimiter perSecond")?;
                    if n < 1.0 || n.fract() != 0.0 {
                        return Err(AsError::at(
                            "sync.rateLimiter: perSecond must be a positive integer",
                            span,
                        )
                        .into());
                    }
                    (n as usize, 1000u64)
                } else {
                    let count_v = o
                        .get("count")
                        .cloned()
                        .unwrap_or(Value::Nil);
                    let window_v = o
                        .get("windowMs")
                        .cloned()
                        .unwrap_or(Value::Nil);
                    let c = want_number(&count_v, span, "sync.rateLimiter count")?;
                    let w = want_number(&window_v, span, "sync.rateLimiter windowMs")?;
                    if c < 1.0 || c.fract() != 0.0 {
                        return Err(AsError::at(
                            "sync.rateLimiter: count must be a positive integer",
                            span,
                        )
                        .into());
                    }
                    if w < 1.0 || w.fract() != 0.0 {
                        return Err(AsError::at(
                            "sync.rateLimiter: windowMs must be a positive integer",
                            span,
                        )
                        .into());
                    }
                    (c as usize, w as u64)
                }
            }
            other => {
                return Err(AsError::at(
                    format!(
                        "sync.rateLimiter expects an options object, got {}",
                        crate::interp::type_name(other)
                    ),
                    span,
                )
                .into());
            }
        };
        let state = RateLimiterState::new(count, window_ms);
        let handle = self.register_resource(
            NativeKind::RateLimiter,
            indexmap::IndexMap::new(),
            ResourceState::RateLimiter(state),
        );
        Ok(handle)
    }
```

### 5d — Implement `.acquire()` method dispatch

- [ ] **Step 5.5: Add `get_rate_limiter` helper**

After `get_semaphore`, add:

```rust
/// Extract the `RateLimiterState` from the resource table by cloning all Rcs.
fn get_rate_limiter(interp: &Interp, id: u64) -> Option<RateLimiterState> {
    interp.with_resource(id, |r| match r {
        Some(ResourceState::RateLimiter(rl)) => Some(RateLimiterState {
            count: rl.count,
            window_ms: rl.window_ms,
            available: rl.available.clone(),
            window_start: rl.window_start.clone(),
            token_available: rl.token_available.clone(),
        }),
        _ => None,
    })
}
```

- [ ] **Step 5.6: Add `call_rate_limiter_method` to `impl Interp`**

Add after the `sync_rate_limiter` constructor:

```rust
    /// Dispatch methods on a `RateLimiter` native handle.
    /// Currently only `.acquire()` is supported.
    pub(crate) async fn call_rate_limiter_method(
        &self,
        m: &std::rc::Rc<crate::value::NativeMethod>,
        _args: Vec<Value>,
        span: Span,
    ) -> Result<Value, Control> {
        match m.method.as_str() {
            "acquire" => self.rate_limiter_acquire(m.receiver.id, span).await,
            other => Err(AsError::at(
                format!("rateLimiter has no method '{}'", other),
                span,
            )
            .into()),
        }
    }

    /// `limiter.acquire()` — await a token from the bucket.
    ///
    /// Token-bucket algorithm (monotonic clock, no background task):
    /// 1. Check if the window has elapsed → refill to `count`, reset `window_start`.
    /// 2. If tokens available, take one and return.
    /// 3. Otherwise park: create a `notified()` future + `enable()` it (lost-wakeup
    ///    safe), spawn a `spawn_local` timer that sleeps until the next window boundary
    ///    then calls `notify_one()`, then await the notification.
    /// 4. Loop to recheck (spurious wakeup / competing acquirers).
    ///
    /// Borrow discipline: Rc handles are cloned out; no RefCell borrow crosses .await.
    async fn rate_limiter_acquire(&self, id: u64, span: Span) -> Result<Value, Control> {
        loop {
            let rl = match get_rate_limiter(self, id) {
                Some(r) => r,
                None => {
                    return Err(AsError::at(
                        "rateLimiter.acquire: handle is invalid",
                        span,
                    )
                    .into());
                }
            };

            // --- Step 1 & 2: check for refill + take token (short borrow) ---
            enum Action {
                Took,
                Wait { sleep_ms: u64 },
            }
            let action = {
                let mut avail = rl.available.borrow_mut();
                let mut ws = rl.window_start.borrow_mut();
                let elapsed = ws.elapsed().as_millis() as u64;
                if elapsed >= rl.window_ms {
                    // Window expired — refill.
                    *avail = rl.count;
                    *ws = std::time::Instant::now();
                }
                if *avail > 0 {
                    *avail -= 1;
                    Action::Took
                } else {
                    // Compute how long until next window resets.
                    let wait = rl.window_ms.saturating_sub(elapsed).max(1);
                    Action::Wait { sleep_ms: wait }
                }
            }; // borrows released

            match action {
                Action::Took => return Ok(Value::Nil),
                Action::Wait { sleep_ms } => {
                    // --- Lost-wakeup-safe park (mirrors semaphore acquire) ---
                    let notified = rl.token_available.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable(); // register waiter before re-check

                    // Re-check under short borrow — if state changed, loop without park.
                    {
                        let avail = rl.available.borrow();
                        let ws = rl.window_start.borrow();
                        let elapsed = ws.elapsed().as_millis() as u64;
                        if *avail > 0 || elapsed >= rl.window_ms {
                            continue;
                        }
                    }

                    // Spawn a one-shot timer that notifies after the window resets.
                    // This wakes all parked acquirers so they can each re-check.
                    let notify = rl.token_available.clone();
                    tokio::task::spawn_local(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
                        notify.notify_waiters();
                    });

                    notified.await;
                    // Re-loop to recheck (refill + take token).
                }
            }
        }
    }
```

- [ ] **Step 5.7: Build and check no errors**

```bash
cd /Users/mahmoud/ascript && cargo build --no-default-features 2>&1 | grep -E "^error" | head -20
```

Expected: clean compile or specific missing-import errors.

---

## Task 6: Implement `task.retry` in `src/stdlib/task_mod.rs`

**Files:**
- Modify: `src/stdlib/task_mod.rs`

### 6a — Export and dispatch

- [ ] **Step 6.1: Add `retry` to `exports()`**

```rust
        ("retry", bi("task.retry")),
```

- [ ] **Step 6.2: Add dispatch arm in `call_task`**

```rust
            "retry" => self.task_retry(args, span).await,
```

### 6b — Implement `task_retry`

- [ ] **Step 6.3: Add the `task_retry` method**

The key insight: `call_value(fn).await` returns `Result<Value, Control>`. We match:
- `Ok(v)` → success, return immediately (no retry)
- `Err(Control::Panic(e))` → retry if attempts remain, else re-raise
- `Err(other)` → pass through (`Propagate`, `Exit` — not retryable)

Also need `next_random()` for jitter. Since it lives in `src/stdlib/math.rs` as a private function, we need to either duplicate the minimal version or expose it. Looking at the codebase, `next_random` is a file-private function — the cleanest approach is to inline a simple `rand_f64()` local helper (same xorshift64 algorithm, 3 lines). This avoids adding a `pub` to math.rs just for this.

Add after `task_timeout`:

```rust
    /// `retry(fn, opts?) -> value`
    ///
    /// Calls `fn()` up to `opts.attempts` times (default 3). On each
    /// `Control::Panic` (and only on panic — returned `[nil, err]` pairs are
    /// NOT retried per spec), waits `baseMs * 2^attemptIndex` ms (capped at
    /// `opts.maxMs` if given) then retries. If `opts.jitter` is `true`, adds a
    /// uniform random fraction of the delay on top (up to +50%).
    /// After all attempts fail, re-raises the LAST panic.
    ///
    /// Non-panic errors (`Control::Propagate`, `Control::Exit`) are passed
    /// through immediately without retry.
    async fn task_retry(&self, args: &[Value], span: Span) -> Result<Value, Control> {
        let func = arg(args, 0);
        let opts = arg(args, 1);

        // Parse options.
        let (attempts, base_ms, max_ms, jitter) = match &opts {
            Value::Nil => (3usize, 100u64, None::<u64>, false),
            Value::Object(o) => {
                let o = o.borrow();
                let attempts = match o.get("attempts") {
                    Some(v) => {
                        let n = super::want_number(v, span, "task.retry attempts")?;
                        if n < 1.0 || n.fract() != 0.0 {
                            return Err(AsError::at(
                                "task.retry: attempts must be a positive integer",
                                span,
                            )
                            .into());
                        }
                        n as usize
                    }
                    None => 3,
                };
                let base_ms = match o.get("baseMs") {
                    Some(v) => {
                        let n = super::want_number(v, span, "task.retry baseMs")?;
                        if n < 0.0 {
                            return Err(AsError::at(
                                "task.retry: baseMs must be non-negative",
                                span,
                            )
                            .into());
                        }
                        n as u64
                    }
                    None => 100,
                };
                let max_ms = match o.get("maxMs") {
                    Some(v) => {
                        let n = super::want_number(v, span, "task.retry maxMs")?;
                        if n < 0.0 {
                            return Err(AsError::at(
                                "task.retry: maxMs must be non-negative",
                                span,
                            )
                            .into());
                        }
                        Some(n as u64)
                    }
                    None => None,
                };
                let jitter = matches!(o.get("jitter"), Some(Value::Bool(true)));
                (attempts, base_ms, max_ms, jitter)
            }
            other => {
                return Err(AsError::at(
                    format!(
                        "task.retry opts must be an object or nil, got {}",
                        crate::interp::type_name(other)
                    ),
                    span,
                )
                .into());
            }
        };

        let mut last_panic: Option<crate::error::AsError> = None;

        for attempt in 0..attempts {
            // Call the function. Note: args are cloned per attempt because
            // call_value may consume them (clone at call site mirrors spawn).
            let result = self.call_value(func.clone(), vec![], span).await;
            match result {
                // Success: return immediately (no retry of ok values or [nil,err] pairs).
                Ok(v) => return Ok(v),
                // Panic: retry if attempts remain.
                Err(Control::Panic(e)) => {
                    last_panic = Some(e);
                    // If this was the last attempt, break to re-raise below.
                    if attempt + 1 >= attempts {
                        break;
                    }
                    // Compute exponential backoff delay.
                    let shift = attempt.min(62) as u32; // cap shift to avoid overflow
                    let delay = base_ms.saturating_mul(1u64.saturating_shl(shift));
                    let mut delay = if let Some(max) = max_ms {
                        delay.min(max)
                    } else {
                        delay
                    };
                    if jitter {
                        // Add up to +50% jitter.
                        let frac = retry_rand_f64();
                        delay = delay.saturating_add((delay / 2).saturating_mul((frac * 1000.0) as u64) / 1000);
                    }
                    if delay > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                }
                // Propagate / Exit: not retryable — pass through unchanged.
                Err(other) => return Err(other),
            }
        }

        // All attempts exhausted — re-raise the last panic.
        Err(Control::Panic(last_panic.expect("at least one attempt")))
    }
```

- [ ] **Step 6.4: Add the `retry_rand_f64` helper function (file-private)**

Add at the bottom of `src/stdlib/task_mod.rs` (outside `impl Interp`):

```rust
/// Minimal xorshift64* PRNG for retry jitter. Thread-local, seeded from the
/// system clock. NOT cryptographic — adequate for backoff jitter only.
fn retry_rand_f64() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static RNG: Cell<u64> = Cell::new({
            use std::time::{SystemTime, UNIX_EPOCH};
            let n = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15);
            n.max(1)
        });
    }
    RNG.with(|c| {
        let mut x = c.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        c.set(x);
        (x.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    })
}
```

---

## Task 7: Run tests and fix any issues

- [ ] **Step 7.1: Run the new retry tests (no-default-features)**

```bash
cd /Users/mahmoud/ascript && cargo test retry --no-default-features 2>&1 | tail -30
```

Expected: all 3 retry tests pass.

- [ ] **Step 7.2: Run the new rateLimiter tests (no-default-features)**

```bash
cd /Users/mahmoud/ascript && cargo test rate_limiter --no-default-features 2>&1 | tail -30
```

Expected: both rateLimiter tests pass.

- [ ] **Step 7.3: Run the full test suite (no-default-features)**

```bash
cd /Users/mahmoud/ascript && cargo test --no-default-features 2>&1 | tail -30
```

Expected: all tests pass, no regressions.

- [ ] **Step 7.4: Run the full test suite (all features)**

```bash
cd /Users/mahmoud/ascript && cargo test 2>&1 | tail -30
```

Expected: all tests pass, no regressions.

---

## Task 8: Clippy clean in both configs

- [ ] **Step 8.1: Clippy with default features**

```bash
cd /Users/mahmoud/ascript && cargo clippy --all-targets 2>&1 | grep -E "^error|warning\[" | head -20
```

Fix any warnings. Common pitfalls:
- `await_holding_refcell_ref` — ensure no RefCell borrow crosses `.await`
- unused variables → prefix with `_`
- `let _ = &args;` may be needed if `_args` isn't used

- [ ] **Step 8.2: Clippy with no-default-features**

```bash
cd /Users/mahmoud/ascript && cargo clippy --no-default-features --all-targets 2>&1 | grep -E "^error|warning\[" | head -20
```

Expected: clean (0 warnings).

---

## Task 9: Commit

- [ ] **Step 9.1: Stage and commit**

```bash
cd /Users/mahmoud/ascript && git add src/value.rs src/interp.rs src/stdlib/task_mod.rs src/stdlib/sync.rs docs/superpowers/plans/2026-06-01-phase4d-retry-ratelimiter.md
git commit -m "feat(task,sync): retry with backoff + rateLimiter

- task.retry(fn, opts?): retries on Control::Panic only (not [nil,err]);
  exponential backoff (baseMs*2^n, capped at maxMs), optional jitter
- sync.rateLimiter({perSecond}|{count,windowMs}): token-bucket limiter;
  .acquire() async method; monotonic-clock refill (no background task)
- NativeKind::RateLimiter + ResourceState::RateLimiter variants
- Tests: retry succeeds on Kth attempt, exhausts+reraises, skips err-pairs;
  rateLimiter count/window basic + perSecond alias

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review Checklist

**Spec coverage:**
- [x] `task.retry(fn, opts?)` — attempts, baseMs, maxMs, jitter all handled
- [x] Retry on panic only (NOT on `[nil,err]` pairs) — tested + documented
- [x] Re-raises LAST panic after exhaustion — tested
- [x] `sync.rateLimiter({perSecond:N})` sugar — tested
- [x] `sync.rateLimiter({count:N, windowMs:M})` — tested  
- [x] `.acquire()` as NativeMethod — dispatched via `call_native_method`
- [x] Lost-wakeup-safe park pattern for `acquire` — enable() before recheck
- [x] Borrow discipline — no RefCell borrow across `.await` anywhere
- [x] Both clippy configs clean
- [x] Both cargo test configs run

**API consistency:**
- `sync.rateLimiter(opts)` returns a handle; `.acquire()` is a NativeMethod on the handle — consistent with how `sync.semaphore` returns a handle (though semaphore methods are module-qualified, `limiter.acquire()` is a method because the limiter is conceptually a first-class object with a single operation, not a shared primitive)
- `task.retry` is module-qualified (`import { retry } from "std/task"`) — consistent with `timeout`, `race`, etc.

**Type consistency across tasks:**
- `RateLimiterState` is defined in `sync.rs`, referenced in `interp.rs` as `crate::stdlib::sync::RateLimiterState` — consistent with `Channel` and `Semaphore` patterns
- `NativeKind::RateLimiter` → `type_name` → `"rateLimiter"` — consistent casing

**No placeholders:** All code blocks are complete implementations. No "TBD" or "fill in later".
