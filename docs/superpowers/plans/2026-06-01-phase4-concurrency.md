# Phase 4 — Concurrency & Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use `- [ ]` checkboxes.

**Goal:** Channels + semaphore (`std/sync`), timers (`std/time`), retry/rateLimit (`std/task`/`std/sync`) on the M17 async engine. Coordination primitives for the single-threaded runtime (no threading locks). Full spec: `docs/superpowers/specs/2026-06-01-phase4-concurrency-design.md`.

**Architecture:** Channels/semaphores/timer-state are **native resources** (`ResourceState` variants in `interp.rs`, referenced by `Value::Native` handles) backed by `tokio::sync` (mpsc/Semaphore) — same model as TCP/stdin. Async ops dispatch through an `impl Interp` method (like `call_io`) using the take-out-across-await pattern (never hold a `resources`/`RefCell` borrow across `.await`). Callable wrappers (debounce/throttle) are `Value::NativeMethod` bound to a state resource. No new language syntax.

**Conventions:** Tier-1 `[v,err]` for expected runtime failures (send-on-closed); Tier-2 panic for misuse; register modules in BOTH `mod.rs` arms; clippy clean both configs; RUN both `cargo test` configs; docs + README + example; cancel-on-drop preserved.

Sub-phases: 4a channels → 4b semaphore → 4c timers → 4d resilience → integration. 4d builds on 4b/4c.

---

## Sub-phase 4a: Channels (`std/sync`)

**Files:** `src/stdlib/sync.rs` (new), `src/stdlib/mod.rs` (register both arms, core/no gate), `src/interp.rs` (`ResourceState::Channel` variant + `impl Interp` async dispatch `call_sync`), tests + (defer example to integration).

- [ ] **Step 1 — failing tests** (in sync.rs, using the lex→parse→exec `val`/`run_source` helper and `task.spawn` for producer/consumer): unbounded FIFO (`sync.channel()`, send 1,2,3, recv → 1,2,3); recv returns `nil` after `close`+drain; `sync.send` to a closed channel → `[false, err]`; `sync.tryRecv` on empty → `[nil, false]`, on ready → `[v, true]`; bounded channel (`sync.channel(1)`) backpressure: a spawned sender of 2 items only completes the 2nd after a recv (assert ordering via shared state). Tier-2 panic on non-channel arg.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:**
  - `ResourceState::Channel` holding the mpsc halves. Use `tokio::sync::mpsc`: store `Sender` (or `UnboundedSender`) + the `Receiver` behind the take-out pattern. Since bounded vs unbounded are different types, wrap in an enum (`ChannelState { Bounded{tx,rx}, Unbounded{tx,rx} }`) or box a trait — implementer picks; keep `recv` take-out-across-await safe.
  - `sync.rs`: `exports()` + `impl Interp::call_sync` (async) for `channel`(capacity opt→bounded/unbounded), `send`(async, `[ok,err]`), `recv`(async, value|nil), `tryRecv`(`[v,ok]`), `close`. Register `pub mod sync` + both `mod.rs` arms (core, no feature gate; tokio is already core).
  - Native handle creation: mirror how an existing native resource (e.g. tcp listener) creates a `Value::Native` with a fresh resource id; `call_sync` looks up the Channel state by id with the take-out/return pattern for the receiver.
- [ ] **Step 4 — verify:** both `cargo test` configs + both clippy. Green, 0 warnings.
- [ ] **Step 5 — commit:** `feat(sync): std/sync channels (channel/send/recv/tryRecv/close)`

---

## Sub-phase 4b: Semaphore (`std/sync`)

**Files:** `src/stdlib/sync.rs` (extend), `src/interp.rs` (`ResourceState::Semaphore`), tests.

- [ ] **Step 1 — failing tests:** `sync.semaphore(2)`; acquire 2 then a 3rd acquire (in a spawned task) waits until a release (assert via shared state/ordering); `sync.withPermit(s, fn)` returns fn's result and releases (available() back to start) even when fn panics (use recover to observe); `sync.available(s)` reflects count; non-positive permits → Tier-2 panic.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** `ResourceState::Semaphore(Rc<tokio::sync::Semaphore>)`. `sync.semaphore(n)`, `acquire`(async), `release`, `withPermit`(async; acquire→await fn→release in all paths incl panic), `available`. Take-out-across-await as needed (Semaphore is shareable via Rc; acquiring may use `acquire_owned`/manual count — implementer picks a clean approach that doesn't hold a resources borrow across await; document).
- [ ] **Step 4 — verify:** both configs + clippy.
- [ ] **Step 5 — commit:** `feat(sync): std/sync semaphore (semaphore/acquire/release/withPermit/available)`

---

## Sub-phase 4c: Timers (`std/time`)

**Files:** `src/stdlib/time.rs` (extend) + `interp.rs` (`ResourceState::Timer`/interval state; debounce/throttle state), tests.

- [ ] **Step 1 — failing tests:** `time.interval(ms)` → a resource with a `.tick()`/recv-style call that awaits the next tick; loop N times asserting it ticked ~N times (short ms, tolerance; or assert monotonic tick count). `time.debounce(fn, ms)`: rapid burst of calls → fn runs once after the quiet period (assert via a counter the fn increments; await past the window). `time.throttle(fn, ms)`: calls within a window → fn runs at most once (leading-edge). Keep timing tests short + tolerance-based.
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:**
  - `time.interval(ms)`: return a native handle (NOT a true generator — simpler) usable via a bound method `.tick()` (a `Value::NativeMethod`) that awaits the next `tokio::time::interval` tick. Document the `while`/`.tick()` idiom (for-await over it is future work — explicit scope note).
  - `time.debounce(fn, ms)` / `time.throttle(fn, ms)`: create a native state resource holding `{fn: Value, ms, last/pending}`; return a `Value::NativeMethod` bound to it. When the wrapper is called, run the debounce/throttle logic: debounce cancels/replaces a pending delayed `task.spawn`-style call and schedules a new one `ms` after the last call; throttle invokes `fn` immediately if outside the window, else drops. Investigate how `Value::NativeMethod` is invoked (`call_native_method` in interp.rs) and how to spawn the delayed call (reuse the spawn_local/SharedFuture path). Deferred-call return values are NOT surfaced (fire-and-forget side effects) — document.
- [ ] **Step 4 — verify:** both configs + clippy.
- [ ] **Step 5 — commit:** `feat(time): interval/debounce/throttle timers`

---

## Sub-phase 4d: Resilience (`std/task` retry, `std/sync` rateLimit)

**Files:** `src/stdlib/task_mod.rs` (retry) + `src/stdlib/sync.rs` (rateLimit), tests.

- [ ] **Step 1 — failing tests:** `task.retry(fn, {attempts:3, baseMs:1})` — fn panics twice then succeeds → returns success value (assert call count 3); fn always panics → re-raises after 3 attempts (recover to observe); backoff increases (loose timing). `sync.rateLimiter({perSecond:N})` (or count/windowMs) — caps acquisitions per window (assert throughput).
- [ ] **Step 2 — verify fail.**
- [ ] **Step 3 — implement:** `task.retry(fn, opts?)` async: loop attempts; call fn; on panic (default) retry after `baseMs * 2^n` (cap `maxMs`, optional jitter); re-raise last failure after exhausting. `sync.rateLimiter(opts)` → a native handle with an `acquire` method (NativeMethod) gating to `perSecond`, implemented via the 4b semaphore refilled by a 4c timer (or a token-bucket on the monotonic clock — implementer picks).
- [ ] **Step 4 — verify:** both configs + clippy.
- [ ] **Step 5 — commit:** `feat(task,sync): retry with backoff + rateLimiter`

---

## Sub-phase 4 integration

- [ ] `examples/concurrency_toolkit.as`: producer/consumer over a channel (via task.spawn), semaphore-bounded fan-out, a retry that eventually succeeds, debounce demo. Run it (bounded — must complete + print success, no hang). Conformance (treesitter + frontend) + fmt idempotence.
- [ ] Docs: `docs/content/stdlib/` page for `std/sync` (channels/semaphore/rateLimit) + `std/time` timer additions + `task.retry`; README stdlib table; mention in async docs (`docs/content/stdlib/async.md` if present).
- [ ] FULL gates: both `cargo test` configs, both clippy `--all-targets`, `fmt --check`, the example, both conformance tests.
- [ ] Holistic review (focus: no `RefCell`/resource borrow across `.await`; cancel-on-drop preserved; channels don't deadlock/leak; retry re-raises correctly; no regression to existing task/time; no TODOs). Merge `--no-ff`.

## Self-review notes
- Riskiest: 4a channel receiver take-out-across-await (deadlock/borrow hazards) and 4c debounce/throttle NativeMethod-closure + delayed-spawn state. Reviewers must probe borrow-across-await and cancel-on-drop specifically.
- No new syntax → conformance must pass unchanged. `interval` is a `.tick()` resource, not a generator (documented scope choice), to avoid native-generator-body complexity.
- Timing tests: keep short + tolerance-based to avoid CI flakiness; prefer asserting counts/ordering over exact durations.
