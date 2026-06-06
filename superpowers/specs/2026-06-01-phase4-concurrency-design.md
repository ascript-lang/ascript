# Phase 4 — Concurrency & Resilience Design

- **Date:** 2026-06-01
- **Status:** Design — proceeding under the standing multi-phase goal.
- **Roadmap:** Phase 4 of `2026-05-31-batteries-completeness-roadmap.md`.
- **Owner:** Mahmoud Kayyali

## Goal

Add the task-coordination and resilience layer on top of the M17 async engine: **channels**
(producer/consumer between spawned tasks), **semaphore** (concurrency cap / backpressure),
**timers** (`interval`/`debounce`/`throttle`), and **resilience** (`retry`/backoff,
`rateLimit`). These coordinate *concurrent tasks on the single thread* — NOT threads (no
mutex/waitGroup; the runtime is `!Send`/`current_thread` and `gather` already covers "wait for
N"). Per the brainstorm: parallelism is absent, concurrency is real, so these are the
tokio-`sync`-style primitives a `current_thread` program still needs.

## Architecture & conventions

- The runtime is `current_thread`, `Rc`/`RefCell`, `!Send`; tasks are `spawn_local` futures
  bound to `Value::Future` handles (cancel-on-drop). Channels/semaphores are **native resources**
  in `Interp.resources` (`ResourceState` variants), referenced by `Value::Native` handles — the
  same model as TCP/stdin. NEVER hold a `resources`/`RefCell` borrow across `.await` (take-out
  pattern; clippy `await_holding_refcell_ref = deny` enforces).
- Backed by `tokio::sync` (`mpsc`, `Semaphore`) — already in the dependency tree.
- Expected runtime failures (send-on-closed, recv-on-closed) → consider Tier-1 vs nil; misuse
  (wrong arg types) → Tier-2 panic. Decisions inline.
- Modules: **`std/sync`** (channel + semaphore + rateLimit) is new (core/no gate — concurrency is
  fundamental and tokio is already core). Timers extend **`std/time`**; `retry` extends
  **`std/task`**.
- Stdlib module + `impl Interp` async dispatch (these await) like `call_io`; register in BOTH
  `mod.rs` arms; clippy clean both configs; run BOTH `cargo test` configs; docs + README +
  example.

## Sub-phases
- **4a — Channels** (`std/sync`: channel/send/recv/close/tryRecv) — new native resource.
- **4b — Semaphore** (`std/sync`: semaphore/acquire/release/withPermit).
- **4c — Timers** (`std/time`: interval, debounce, throttle).
- **4d — Resilience** (`std/task`: retry/backoff; `std/sync`: rateLimit).

4d's rateLimit builds on 4b semaphore + 4c timing; 4a/4b independent.

---

## 4a — Channels (`std/sync`)

`tokio::sync::mpsc`-backed FIFO queue for passing values between `task.spawn`ed tasks.

- `sync.channel(capacity = 0) -> channel` — `capacity 0` (or omitted) → **unbounded**
  (`mpsc::unbounded_channel`); `capacity > 0` → **bounded** (`mpsc::channel(capacity)`, sender
  awaits when full = backpressure). Returns one `Value::Native` channel handle (the resource holds
  both sender and receiver halves; a single multi-use handle is simpler for a scripting language
  than split sender/receiver values).
- `sync.send(ch, v) -> [ok, err]` — Tier-1. async (awaits on a full bounded channel). Sending to a
  **closed** channel → `[false, err]` (expected condition, not a panic).
- `sync.recv(ch) -> value | nil` — async. Returns the next value; **`nil` when the channel is
  closed AND drained** (clean end-of-stream sentinel). (Document that `nil` values can't be
  distinguished from close — acceptable; a `[value, ok]` form may be added later if needed.)
- `sync.tryRecv(ch) -> [value, ok]` — non-blocking: `[v, true]` if a value was ready, `[nil, false]`
  if empty/closed.
- `sync.close(ch)` — close the sending side; subsequent `recv` drains then returns `nil`.
- **ResourceState::Channel** variant holding the mpsc sender + a `Rc<RefCell<Receiver>>` (or the
  receiver taken-out-across-await). Cancel-on-drop: dropping the handle drops both halves.
- `for await v of ch` integration: if the generator `for await` protocol can drive a channel
  (M17), expose the channel as iterable so `for await v of ch { }` recvs until close. If wiring
  `for await` to a native resource is non-trivial, defer that sugar and document `while` +
  `sync.recv` as the loop idiom (NOT a silent drop — an explicit, noted scope line).

### Tests (4a)
unbounded send/recv FIFO order; bounded backpressure (sender awaits when full, unblocks on recv —
via two spawned tasks); recv returns nil after close+drain; send-to-closed → `[false, err]`;
tryRecv empty → `[nil, false]`; a producer task + consumer task exchanging N values via
`task.spawn`. Example.

---

## 4b — Semaphore (`std/sync`)

`tokio::sync::Semaphore`-backed concurrency limiter.

- `sync.semaphore(permits) -> semaphore` — `permits` a positive integer (else Tier-2 panic).
- `sync.acquire(s) -> nil` — async; awaits until a permit is free, takes one. (Returns nil; the
  script must pair with `release`.)
- `sync.release(s)` — return a permit.
- `sync.withPermit(s, fn) -> value` — async; acquires, awaits `fn()`, releases even if `fn`
  panics (the ergonomic RAII-substitute — preferred API; `acquire`/`release` are the low-level
  pair). Returns `fn`'s result; re-raises its panic after releasing.
- `sync.available(s) -> number` — current free permits (introspection).
- **ResourceState::Semaphore** holding `Rc<tokio::sync::Semaphore>` (Semaphore is `!Send`-safe for
  `current_thread`; if its guard lifetime fights the resource model, track an integer count + a
  notify, or use `Semaphore` with manual `add_permits`/`try_acquire` + an await loop — implementer
  picks the clean approach, documents it).

### Tests (4b)
acquire blocks past `permits` until release (spawn N+1 tasks, assert the N+1th waits); withPermit
releases on normal and panicking `fn`; available() reflects state; non-positive permits → panic.

---

## 4c — Timers (`std/time`)

- `time.interval(ms) -> generator` — an **async generator** (M17) yielding successively (a tick
  value, e.g. the tick index) every `ms`, usable as `for await _ of time.interval(1000) { }`.
  Backed by `tokio::time::interval`. Dropping the generator stops it (cancel-on-drop). If
  returning a true `Value::Generator` from native code is hard, expose it as a channel-like
  resource driven by `for await`/`recv` — document the chosen shape.
- `time.debounce(fn, ms) -> function` — returns a wrapped function: each call resets a timer;
  `fn` runs `ms` after the last call. Trailing-edge.
- `time.throttle(fn, ms) -> function` — returns a wrapped function that invokes `fn` at most once
  per `ms` window (leading-edge).
- **State:** debounce/throttle need per-wrapper persistent state (last-call time / pending timer).
  Back the wrapped function with a native resource holding the timing state + (for debounce) the
  pending `Value::Future` of the delayed call (replaced/cancelled on each new call). The returned
  value is a `Value::Builtin`/closure capturing the resource id. Implementer confirms the closure
  mechanism (how AScript represents a native closure capturing state) and documents it.
- **Decision:** debounce is trailing-edge, throttle is leading-edge (the common defaults);
  document. Both are fire-and-forget for side effects (the wrapped fn's return value of a deferred
  call isn't surfaced — document; a promise-returning variant is future work).

### Tests (4c)
interval yields ~N times over N*ms (use a bounded loop, assert count/monotonic ticks);
debounce collapses a burst into one trailing call; throttle caps calls per window. (Time-based
tests use the runtime clock; keep them short and tolerance-based to avoid flakiness, or drive a
deterministic tick where possible.)

---

## 4d — Resilience (`std/task` retry, `std/sync` rateLimit)

- `task.retry(fn, opts?) -> value` — async; calls `fn()`; on a **panic OR a `[nil, err]`-style
  failure** (decide: retry on thrown panic; optionally on a returned err — default retry on
  panic only, document), retries up to `opts.attempts` (default 3) with backoff
  (`opts.baseMs` default 100, exponential `base * 2^n`, `opts.maxMs` cap, optional `opts.jitter`).
  Returns `fn`'s success value; after exhausting attempts, re-raises the last failure.
- `sync.rateLimit(opts) -> function-or-limiter` — token-bucket: `opts.perSecond` (or
  `opts.count`/`opts.windowMs`). Expose either `sync.rateLimiter(opts)` returning a limiter with
  `await limiter.acquire()` (gate before an action), implemented as semaphore (4b) refilled by a
  timer (4c). Pick the limiter-object shape; document.

### Tests (4d)
retry succeeds after K failures (a fn failing K-1 times then succeeding); exhausts and re-raises
after `attempts`; backoff timing roughly increases; rateLimit caps throughput over a window.

---

## Cross-cutting
- NO new language syntax (all builtins/modules) UNLESS `for await` over channels/intervals needs a
  protocol hook — if so, that touches the generator/iter protocol only (no grammar), and must keep
  tree-sitter/parser conformance passing. Prefer reusing the existing `for await` generator
  protocol; document any hook.
- Structured-concurrency invariant preserved: channel/semaphore/timer resources cancel-on-drop;
  no orphaned tasks; no `RefCell`/resource borrow across `.await`.
- Integration: `examples/concurrency_toolkit.as` (producer/consumer channel, semaphore-bounded
  fan-out, debounce, retry); docs pages for `std/sync` + the `std/time`/`std/task` additions;
  README; full gates (both test configs, clippy both, fmt, conformance, idempotence); holistic
  review; merge `--no-ff`.

## Decisions (made; flagged)
1. No threading mutex/waitGroup (single-threaded; `gather` covers join). **Settled.**
2. Channels: single multi-use native handle (not split sender/receiver values); unbounded by
   default, bounded for backpressure; `recv` returns `nil` at close+drain. **Settled.**
3. Semaphore: explicit acquire/release + ergonomic `withPermit` (release-on-panic). **Settled.**
4. debounce trailing-edge, throttle leading-edge. **Settled.**
5. retry: on panic by default (err-retry opt-in), exponential backoff. **Settled.**
6. New `std/sync` module (core, no feature gate). **Settled.**

## Open implementation choices (decide during impl, document)
- `for await` over channels/intervals: wire to the generator protocol if clean, else document
  `while`+`recv` and a bounded interval loop (explicit scope note, not a silent drop).
- Semaphore guard representation under the resource model.
- debounce/throttle native-closure state mechanism + whether deferred return values are surfaced
  (default: not; document).
- rateLimiter object vs wrapped-function shape.
