# `std/resilience` — Backend-Hosting Resilience Policies (RESIL) — Design

- **Status:** Draft for review
- **Date:** 2026-06-12
- **Code:** RESIL (goal-perf.md, "Deployment & reach track")
- **Depends on:** nothing in-flight (builds on shipped `std/sync`, `std/lru`, `std/task`,
  `std/log`, `std/telemetry`, the SP9 determinism seams, and the SP12 task-local precedent).
- **Depended on by:** **CNTR** (`ascript init --template server` wires RESIL template policies —
  goal-perf.md line 270).
- **Engines:** pure stdlib + ONE runtime seam (task-local storage). All four modes must stay
  byte-identical (tree-walker == specialized-VM == generic-VM == `.aso`).
- **Breaking:** **no.** No grammar change, no new opcode, no new `Value` kind, **no
  `ASO_FORMAT_VERSION` bump** (27 at writing, `src/vm/aso.rs:167` — pinned by a negative-space
  test). `task.retry` is extended **compatibly** (§3.4).

---

## 0. Read this first — what RESIL is and is not

`std/resilience` (Cargo feature `resilience`, **default-on**, builds on core-only substrates so
the module itself has no heavy deps) is the backend-hosting policy kit: **circuit breaker, token-
bucket rate limiting (plain + keyed), bulkhead + load shedding, retry v2, fallback, singleflight,
stampede-protected memoization, deadline propagation, Prometheus metrics, and health handlers** —
all *per-isolate* (§7), all composable by plain nested calls (§3.5), all observable (§6).

Two design pillars, stated up front because every section leans on them:

1. **Policies are tagged Objects — the `std/schema` precedent, exactly.** A policy is a
   `Value::Object` carrying a `__resil` kind tag (mirroring schema's `{__kind: …}` tagged
   objects, `src/stdlib/schema.rs:131`). Method-style use (`breaker.call(fn)`,
   `limiter.tryAcquire()`) is a **call-site hook** in the two engines' method-dispatch paths,
   added the SAME way schema's was (`is_schema_value && is_schema_method` →
   `call_schema(name, [recv, ...args])`, tree-walker `src/interp.rs:4140`, VM
   `src/vm/run.rs:4873`). Call-position only — a bare `breaker.state` member read still reads the
   stored field. **NO new `Value` kind.**
2. **ONE runtime seam: task-local storage** (§5) — a `tokio::task_local!` cell holding an
   `Rc`'d, immutable, copy-on-spawn per-task record. It follows the shipped SP12
   `TELEMETRY_CURRENT` task-local **exactly** (`src/interp.rs:55-113`): captured at every
   user-code spawn site, scoped into the spawned body, zero-cost when unused (a `None` check),
   proven by a Gate-12 bench. Everything else in RESIL is ordinary stdlib code.

What RESIL is **not**: it is not cross-isolate. Every policy's state lives in one isolate (§7);
multi-isolate `server.serve({workers: N})` gets N independent breakers/limiters — usually what
you want (the Envoy-sidecar analogy: per-replica circuit breaking is the deployed norm). The
documented pattern for genuinely global state is a `worker class` actor (§7.2, with a shipped
example). Hedged requests, AIMD adaptive concurrency, and `std/k8s` are **parked with design
sketches** (§9.3) — recorded, not silently dropped.

## 1. Motivation & shipped substrate (verified 2026-06-12)

AScript already serves HTTP across cores (SRV), isolates faults per connection
(`src/stdlib/http_server.rs` — panic→500, per-connection limits, `maxConcurrent` semaphore), and
retries (`task.retry`). What's missing is the *policy layer* every production gateway hand-rolls:
stop hammering a dead dependency (breaker), bound per-client request rates (keyed limiter), shed
load instead of queueing unboundedly (bulkhead), give a request a total time budget that survives
nested awaits and clamps downstream I/O (deadline propagation), collapse duplicate concurrent
work (singleflight/memoize), and export it all as `/metrics`.

The substrates this design composes (each verified in source — line refs are anchors, re-grep
the symbol before editing):

| Substrate | Where | What RESIL uses |
|---|---|---|
| `SharedFuture` / `ResultCell` | `src/task.rs:33-160` | N awaiters on one future: `ResultCell::get` loops on `Notify::notify_waiters` and every clone observes the same stored `Result<Value, Control>` (`resolve` is first-writer-wins, `task.rs:54-59`); a stored `Control::Panic` re-raises in **every** awaiter (the module doc says so explicitly, `src/stdlib/task_mod.rs:10-11`, and `carries_error_across_boundary` proves it, `src/task.rs:190-197`). This is singleflight's entire mechanism (§3.6). |
| `sync.semaphore` | `src/stdlib/sync.rs:51-70`, acquire at `:483` | The bulkhead's concurrency cap. The lost-wakeup-safe `enable()`-before-recheck acquire loop is reused as-is via the semaphore handle (§3.3). |
| `sync.rateLimiter` (**already exists**) | `src/stdlib/sync.rs:83-111` | A fixed-window limiter shipped earlier. RESIL's limiter is **token-bucket** (smooth refill) + **keyed**; the relationship is documented (§3.2.3) — `sync.rateLimiter` stays, unchanged. |
| `std/lru` | `src/stdlib/lru.rs:33-58` | `LruState` (IndexMap recency, front-eviction at `:151-154`) — the keyed limiter's bucket store and memoize's entry store, reused as the shipped Native handle (composition, not reimplementation). |
| `task.retry` (current contract) | `src/stdlib/task_mod.rs:203-318` | Verified precisely: `retry(fn, opts?)`; opts keys `attempts` (default 3, positive int), `baseMs` (default 100), `maxMs` (optional cap), `jitter` (**bool**, `true` adds up to +50%); delay = `baseMs * 2^attempt` capped at `maxMs`; retries **only `Control::Panic`** — returned `[nil, err]` pairs return immediately (test `retry_does_not_retry_error_pairs`); `Propagate`/`Exit` pass through; after exhaustion the **last** panic re-raises. Jitter uses a thread-local xorshift with a **documented SP9 timing-only exemption** (`task_mod.rs:444-452`): it perturbs only sleep duration, never an observable value, so it is deliberately NOT det-routed. Retry v2 (§3.4) preserves every one of these behaviors. |
| SP9 determinism seams | `src/interp.rs:1376-1406` | `clock_now_ms` (the `time.now`/`date.now` seam), `clock_monotonic_ms` (the `time.monotonic` seam), `next_seeded_f64` (the `math.random` seam) — each branches `Some(ctx)` → virtual/recorded, `None` → real (byte-identical default). **Every OBSERVABLE time/random read in RESIL routes through these** (§8). Test entry: `run_source_deterministic(src, seed)` (`src/lib.rs:678`). |
| SP12 task-local precedent | `src/interp.rs:55-113` | `tokio::task_local! { TELEMETRY_CURRENT: Cell<Option<SpanCtx>> }` + `telemetry_scope(parent, fut)` + `telemetry_capture_current()` — captured at the tree-walker's async spawn sites (`interp.rs:5320`, `:5907`, `:5981`) and seeded at the entry points (`telemetry_root_scope`, used e.g. `lib.rs:542`). The deadline/trace local (§5) is this pattern, made core and engine-complete. |
| Telemetry metrics registry | `src/stdlib/telemetry/model.rs:155-186` | `MetricInstrument` keeps **cumulative local points** (`MetricPoint` per attribute set) — a real local registry. RESIL mirrors into it via the SP12 soft hook when telemetry is initialized, but does **not** depend on it (§6.1 records why). |
| http_server routes/handlers | `src/stdlib/http_server.rs:1036-1110` | `register_route` accepts any `is_callable` value; verb shortcuts `get/post/...`; middleware `use(mw)` with `(req, next)`. `Value::NativeMethod` is callable (it's in `task_spawn`'s callable set, `task_mod.rs:87-91`) — that is how `metricsHandler()`/`health()` return mountable handlers (§6.2/§6.3). |
| http client timeout opts | `src/stdlib/net_http.rs:572-592` | `timeout {connect, read, total}` → reqwest builder. The deadline clamp site (§5.4): effective total = `min(requested, remaining)`. |
| Capability gate | `src/stdlib/mod.rs:325-374` | `required_cap` is a complete enumeration with a completeness test (`mod.rs:991`, `:1086`). `resilience` classifies as `None` (pure: no OS resource — metrics/health only *render*; the server mounts them). The new module must be added to `STD_MODULES` (`mod.rs:221`), both `std_module_exports` arms, and the completeness enumeration. |

**Pre-existing defects found while grounding this spec (Gate 14 — fixed in the RESIL branch
with failing-test-first guards, they sit on RESIL's edit sites):**

1. **VM async spawn sites do not propagate the SP12 telemetry task-local.** The tree-walker
   captures + scopes `TELEMETRY_CURRENT` at its three async spawn sites (`interp.rs:5320/5907/
   5981`), but the VM's `Op::Call` async-closure arm (`vm/run.rs:1747`) and static-async arm
   (`vm/run.rs:5709`) `spawn_local` WITHOUT `telemetry_scope` — a VM-mode async fn body loses
   its parent-span lineage (latent: span lineage isn't differential-asserted). RESIL must wrap
   these same sites for the deadline local (where the asymmetry WOULD be observable), so the
   telemetry wrap is fixed in the same edit.
2. **Stale doc-comment:** `src/stdlib/telemetry/mod.rs` says telemetry is "opt-in at build
   (… not in `default`)" but `Cargo.toml:168` includes `telemetry` in `default`. One-line fix.

## 2. Module surface & the tagged-object + call-site-hook model

### 2.1 Exports

```text
import * as resilience from "std/resilience"

resilience.breaker(opts)        -> breaker policy        (§3.1)
resilience.limiter(opts)        -> token-bucket limiter  (§3.2.1)
resilience.keyedLimiter(opts)   -> per-key limiter       (§3.2.2)
resilience.bulkhead(opts)       -> bulkhead policy       (§3.3)
resilience.retry(opts)          -> reusable retry policy (§3.4)   // task.retry also extended
resilience.fallback(fn, fb)     -> [value, err]          (§3.5)
resilience.singleflight(key, fn)-> future                (§3.6)
resilience.memoize(opts)        -> memoize cache         (§3.7)
resilience.deadline(ms, fn)     -> [value, err]          (§5.2)
resilience.deadlineRemaining()  -> number | nil          (§5.2)
resilience.withTrace(id, fn)    -> fn's result           (§5.5)
resilience.traceId()            -> string | nil          (§5.5)
resilience.metricsHandler()     -> handler               (§6.2)
resilience.health(opts)         -> handler               (§6.3)
resilience.handler(policies, fn)-> wrapped handler       (§6.4)
```

`src/stdlib/resilience.rs` exposes `exports()` + `Interp::call_resilience(...)`, registered in
both `mod.rs` match arms, `STD_MODULES`, the `pub mod` declaration behind
`#[cfg(feature = "resilience")]`, and the `required_cap` completeness enumeration (verdict:
`None`). Cargo: `resilience = []` in `[features]`, added to `default`. The module's substrates
(`sync`, `lru`, `task`) are core, so `resilience` needs no feature deps; the log/telemetry
mirrors are `#[cfg]`'d *inside* the module (§6.1).

### 2.2 Policies are tagged Objects (schema precedent — verbatim)

A policy is `Value::Object` with:

- `__resil: "<kind>"` — kind ∈ `RESIL_KINDS = ["breaker", "limiter", "keyedLimiter",
  "bulkhead", "retry", "memoize"]`. The receiver predicate `is_resilience_value(v)` matches
  ONLY these (the same narrowness argument as `SCHEMA_KINDS`, `schema.rs:160-181` — a user
  object that happens to carry `__resil` with an unknown kind is never hijacked).
- **Config fields** readable as bare members (`breaker.failureRate`) — schema's "bare member
  reads still read fields" contract, kept verbatim.
- **Mutable state fields** (`__state`, `__failures`, `__tokens`, …) — plain `Value`s mutated
  under short borrows by `call_resilience` (Objects are `ObjectCell`-mutable; no borrow ever
  spans an `.await` — state is read/written in synchronous sections around await points,
  exactly the `sync.rs` discipline). Double-underscore fields are documented as
  introspectable-but-private (same posture as schema's internals).
- **Native sub-handle fields** where parking or shipped state machines are needed:
  the bulkhead's `__sem` (a `sync.semaphore` handle), the keyed limiter's and memoize's
  `__store` (a `std/lru` handle). `Value::Native` traces nothing (the GC native-resource rule,
  `src/gc.rs`) — no new GC surface.
- **`__local`: a non-sendable marker** — a `Value::Native` of the new `NativeKind::Resilience`
  with no resource-table entry (id-only, the `noop_handle` precedent,
  `src/stdlib/telemetry/mod.rs:113`). Purpose: a policy crossing the worker airlock fails
  **loudly** with the existing non-sendable field-path panic instead of silently deep-copying
  its counters into a divergent twin (§7.1). Policies are explicitly isolate-local values.
  `NativeKind::Resilience::governing_cap()` returns `None` (no OS resource; the per-handle cap
  re-check stays zero-cost for it).

### 2.3 The call-site hook (added the schema way, both engines, call-position only)

`src/stdlib/resilience.rs` exposes:

```rust
pub(crate) fn is_resilience_value(v: &Value) -> bool   // __resil ∈ RESIL_KINDS
pub(crate) fn is_resilience_method(name: &str) -> bool // the union method set below
pub(crate) async fn call_resilience_method(            // on Interp
    &self, name: &str, args: &[Value], span: Span) -> Result<Value, Control>
```

Method set (union over kinds; `call_resilience_method` validates kind×name and raises a Tier-2
`<kind> policy has no method '<name>'` on a mismatch — schema's `InvalidSchema` escalation
posture): `call`, `state`, `stats`, `reset`, `acquire`, `tryAcquire`, `run`, `get`, `delete`,
`clear`. None of these collide with config-field names (config fields use full words like
`failureRate`, `capacity`; the differential corpus + unit tests pin that bare reads still read
fields).

Hook wiring — **two sites, mirrored exactly like schema's** (this is the entire engine diff
besides §5):

- Tree-walker: in the `ExprKind::Call`-with-`Member`-callee arm of `eval_chain`
  (`src/interp.rs:4135-4216`), a new branch **after** the schema branch (`:4140-4151`) and
  before the workflow-ctx branch, same shape: `is_resilience_value(&recv) &&
  is_resilience_method(name)` → eval args → `call_resilience_method(name, [recv, ...args])`.
  `#[cfg(feature = "resilience")]`, like the workflow branch (`:4156`).
- VM: in `dispatch_method` (`src/vm/run.rs:4859`), a new step after the schema hook
  (`:4873-4882`), same shape, same `cfg`.

Ordering note: the hook tests a tagged Object; schema's predicate can never also match (a
`__resil` object has no `__kind` ∈ `SCHEMA_KINDS`), so relative order between the two hooks is
behaviorally irrelevant — but it is pinned (resilience after schema) so both engines read
identically. `OptMember` (`a?.b(...)`) deliberately does NOT route (schema parity).

### 2.4 Error model (Tier-1 codes — stable, documented)

Policy rejections are **Tier-1 `[nil, err]` pairs** (expected operational outcomes, not bugs).
The `err` object is `{message, code}` with stable codes the HTTP helper (§6.4) maps:

| code | raised by | message shape |
|---|---|---|
| `"breaker-open"` | `breaker.call` while open / half-open budget exhausted | `circuit breaker '<name>' is open` |
| `"rate-limited"` | `limiter.tryAcquire` exhaustion is a `false` return; the **wrapped-handler** path surfaces this code | `rate limit exceeded` (keyed: `… for key '<k>'`) |
| `"bulkhead-full"` | `bulkhead.run` when queue is full | `bulkhead '<name>' queue is full` |
| `"deadline-exceeded"` | `resilience.deadline` expiry; deadline-aware I/O ops (§5.4) | `deadline exceeded` |
| `"retries-exhausted"` | retry-v2 only when retrying **pairs** (`retryOn:"error"`/`"both"`, §3.4) — panic exhaustion re-raises the last panic, unchanged | last err's message |

Misuse (wrong arg types, non-positive capacities, unknown methods) stays **Tier-2 panic** —
the stdlib-wide rule.

## 3. The policies

### 3.1 Circuit breaker

```text
let b = resilience.breaker({
    name: "payments",        // optional; metrics label + messages (default "default")
    failureRate: 0.5,        // open when window failure fraction ≥ this (0,1]
    window: 20,              // sliding window SIZE — last N calls (count-based, §3.1.1)
    minCalls: 10,            // no verdict before this many calls in the window
    cooldownMs: 30000,       // open → halfOpen after this long
    halfOpenMax: 3,          // probe budget while halfOpen
})
let [v, err] = b.call(fetchPayments)     // err.code == "breaker-open" when rejected
b.state()                                 // "closed" | "open" | "halfOpen"
b.stats()                                 // {state, calls, failures, rejected, windowFailureRate, ...}
b.reset()                                 // back to closed, window cleared (ops/test hook)
```

**§3.1.1 Window: COUNT-based, size tunable (decision + justification).** The sliding window is
the last `window` call outcomes in a fixed ring (two ints + a bitset-as-int-array in the policy
object — O(1), bounded memory). Justification: (a) it is **deterministic** — the open/closed
verdict depends only on the call sequence, so the differential corpus and the seeded tests are
exact, and Record/Replay carries no per-call clock event for window maintenance; (b) it is
resilience4j's default (`COUNT_BASED`) for the same reasons; (c) a TIME-based window needs a
clock read per call — under SP9 Record that is one `DetEvent` per breaker call (log bloat) for
no v1 benefit. Time-based windows are **parked** (§9.3 sketch) — the config key is `window`
(count) so a future `windowMs` is additive. Tunable: `window` size, plus every threshold above.

**§3.1.2 State machine.** `closed` —(window has ≥ `minCalls` and failureRate ≥ threshold)→
`open` (records `openedAtMs` via `clock_monotonic_ms` — det-routed, §8) —(a `call` arrives and
`now - openedAtMs ≥ cooldownMs`)→ `halfOpen` (probe counter = 0) —(probe succeeds ×
`halfOpenMax`… actually: each probe outcome is decisive: any probe **failure** → `open` again
(fresh cooldown); `halfOpenMax` consecutive probe **successes** → `closed` (window cleared)).
While `halfOpen`, at most `halfOpenMax` calls are admitted **concurrently-in-flight**; further
calls are rejected with `breaker-open` (the probe budget — prevents a thundering herd from
re-killing the dependency; the half-open race tests pin this, §10). Cooldown expiry is
evaluated lazily on the next `call` (no background task — nothing to leak, nothing to cancel).

**§3.1.3 Failure classification.** `b.call(fn)` calls `fn()` (0-arg; an async fn's returned
future is driven — the `task_retry` pattern, `task_mod.rs:275-279`). Outcome classes:
- plain value `v` → success, returns `[v, nil]`;
- a Result pair `[v, e]` (the 2-element array shape `make_pair` produces) with `e != nil` →
  **failure recorded**, pair passed through unchanged;
- with `e == nil` → success, pair passed through;
- `Control::Panic` → **failure recorded**, panic **re-raised** (never swallowed — `recover` is
  the user's catch, the task.retry posture);
- `Propagate`/`Exit` → passed through, **not recorded** (they are control flow, not dependency
  health).
A rejected call (open) records `rejected` (its own counter) but does NOT enter the window (a
breaker must not feed on its own rejections). Pair detection uses the same shape test the `?`
operator uses (2-element array) — shared helper, both engines identical by construction (one
implementation on `Interp`).

**§3.1.4 Events.** Every transition: a `log.debug`-level structured record (soft, §6.1), a
registry counter bump `…_breaker_transitions_total{name, to}`, and the state gauge update.

### 3.2 Rate limiter (token bucket; plain + keyed)

**§3.2.1 Plain.**

```text
let lim = resilience.limiter({ capacity: 100, refillPerSec: 50, name: "api" })
await lim.acquire()        // parks until a token is available (FIFO-ish, see below), takes one
lim.tryAcquire()           // true (token taken) | false — never parks
lim.tryAcquire(5)          // take n tokens atomically, or none
```

Continuous refill: state = `{__tokens: float, __lastMs: float}`. On every acquire/tryAcquire:
`now = clock_monotonic_ms(...)` (det-routed); `__tokens = min(capacity, __tokens +
(now - __lastMs) / 1000 * refillPerSec); __lastMs = now`; then take if `__tokens ≥ n`.
`acquire` with no token computes the exact deficit sleep `(n - __tokens) / refillPerSec * 1000`
ms, `tokio::time::sleep`s it, and loops (re-deriving from the clock — concurrent acquirers
re-race after each sleep; no `Notify` needed because the wake condition is purely time, not an
event). The sleep duration is **timing-only** (the `task.retry` jitter exemption class,
`task_mod.rs:444-452`); the token *verdict* reads only the det-routed clock, so Record/Replay
reproduces every `tryAcquire` result exactly (§8). Validation: `capacity ≥ 1` int-ish,
`refillPerSec ≥ 0` finite (0 = never refills — a draining bucket, valid for fixed-quota demos),
Tier-2 on misuse. If a deadline local is set and the deficit sleep exceeds the remaining
budget, `acquire` fails fast with `deadline-exceeded` instead of parking past its budget
(§5.4 — the "never wait for something you can't use" rule).

**§3.2.2 Keyed.**

```text
let perClient = resilience.keyedLimiter({ capacity: 20, refillPerSec: 10, maxKeys: 10000 })
perClient.tryAcquire(req.headers["x-api-key"])    // per-key bucket
await perClient.acquire(key)
```

Bucket store = a **`std/lru` Native handle** in `__store`, created with capacity `maxKeys`
(default 10\_000) — composing the shipped, tested eviction (`lru.rs:151-154`: front = LRU
evicted on insert-at-capacity; touched buckets move to MRU). Each entry: key (string — Tier-2
on non-string, keys become metrics labels and airlock-safe values) → a small Object
`{tokens, lastMs}`. **Eviction semantics (documented):** evicting a bucket forgets that key's
spend history — the next request from an evicted key starts a FULL bucket. That is the standard
bounded-memory trade (Envoy's `local_ratelimit` descriptors behave the same); size `maxKeys`
above your active-client cardinality. An eviction bumps `…_limiter_evictions_total{name}`, so
the operator can SEE undersizing.

**§3.2.3 Relationship to `sync.rateLimiter` (pre-existing).** `sync.rateLimiter(count,
windowMs)` (`sync.rs:83-111`) is a fixed-window limiter and **stays unchanged** (removing or
rerouting it would be a silent behavior change). Docs position them: `sync.rateLimiter` =
simple fixed window for scripts; `resilience.limiter` = smooth token bucket + keyed variant +
metrics + deadline awareness for servers.

### 3.3 Bulkhead + load shedding

```text
let bh = resilience.bulkhead({ limit: 8, queue: 16, name: "db" })
let [v, err] = bh.run(fetchFromDb)     // err.code == "bulkhead-full" when shed
```

`limit` = max concurrent executions — backed by a real `sync.semaphore` handle in `__sem`
(created via the same `Semaphore::new` path; the `sync.rs` acquire/release internals get
`pub(crate)` visibility so `resilience.rs` calls them directly rather than re-implementing the
lost-wakeup dance). `queue` = max callers parked waiting (an int counter field `__waiting`):

1. fast path: permit available → acquire (sync decrement), run, release (all-paths, the
   `sync_with_permit` pattern `sync.rs:563-583`: result captured, release before re-raise);
2. no permit and `__waiting >= queue` → **immediate** `[nil, {code:"bulkhead-full"}]` — the
   shed/503 path; never parks (sheds in O(1) — that is the point);
3. else `__waiting += 1` → `acquire` (parks on the semaphore's Notify) → `__waiting -= 1`
   (decrement on ALL exits incl. panic — same all-paths discipline) → run → release.

Like the limiter, a parked `run` respects a deadline local: it parks on the semaphore **raced
against the remaining budget**; expiry → `__waiting -= 1` + `[nil, {code:"deadline-exceeded"}]`
(§5.4). Counters: `…_bulkhead_in_flight` (gauge), `…_bulkhead_shed_total`. Validation: ints ≥ 1
/ ≥ 0, Tier-2 misuse. Panics from `fn` re-raise after release (never swallowed).

### 3.4 Retry v2 — extending `task.retry` COMPATIBLY + a reusable policy object

**Current contract preserved bit-for-bit** (§1 table row): same arity, same defaults, same
panic-only default retry class, same exponential `baseMs*2^i` ∧ `maxMs` schedule, same bool
`jitter` meaning (+0..50%), same exhaustion re-raise. The three shipped `task_mod.rs` retry
tests must pass UNCHANGED. New, all additive, accepted by **both** `task.retry(fn, opts)` and
the reusable `resilience.retry(opts)` policy (`p.call(fn)`; the policy form also carries the
stateful `budget`):

| key | values | semantics |
|---|---|---|
| `backoff` | `"exponential"` (default — current behavior) \| `"fixed"` | fixed = every delay is `baseMs` |
| `jitter` | `true`/`false` (current) \| `"full"` \| `"none"` | `"full"` = delay drawn uniformly from `[0, computedDelay]` (AWS full-jitter); `"none"` ≡ `false`; `true` keeps the shipped +0..50% exactly |
| `maxMs` | number | unchanged (the brief's `maxDelay` — the shipped key name wins, no rename) |
| `retryOn` | `"panic"` (default — current) \| `"error"` \| `"both"` | `"error"`: a returned `[_, err≠nil]` pair is retried (exhaustion → the LAST pair, with `code:"retries-exhausted"` folded into its err if absent); `"both"`: either class |
| `retryIf` | `fn(err) -> bool` | predicate over the panic's error value / the pair's err; `false` → return/raise immediately without further attempts. A panic INSIDE `retryIf` re-raises (programmer error). |
| `budget` | number in (0, 1] — **policy object only** | retry-budget ratio: a token bucket of retry credits refilled at `budget × first-attempt rate` (implementation: two counters `__attemptsSeen`/`__retriesSpent`; a retry is permitted only while `__retriesSpent < budget * __attemptsSeen`; over budget → behave as exhausted immediately). Prevents retry storms from multiplying load during an outage (the Google SRE-book retry-budget pattern). Passing `budget` to the **stateless** `task.retry` is a Tier-2 panic (`task.retry: budget requires a resilience.retry policy`) — never a silent ignore (Gate 6). |

Backoff sleeps remain real `tokio::time::sleep`s; jitter randomness keeps the documented SP9
**timing-only exemption** (it perturbs durations, never values — `task_mod.rs:444-452`; the
brief asked for det-routed jitter, but the shipped, reviewed exemption is the stronger
precedent and replay fidelity is unaffected; recorded as a brief-vs-code delta, §11). The
budget/`retryOn` *verdicts* are count-based — deterministic with no clock interaction at all.

### 3.5 Fallback + composition (decision: nested calls; `compose()` parked)

```text
let [v, err] = resilience.fallback(primary, (e) => cachedDefault)
```

`fallback(fn, fb)`: run `fn()` (futures driven); on a success value/ok-pair → pass through;
on an err pair → `fb(err)`; on a `Control::Panic` → `fb(errObjectOf(panic))` (the panic is
*consumed* — fallback is the terminal "always answer something" layer; this is `recover` +
default in one step and is documented as such). `fb`'s own panic re-raises (no fallback chains
hiding bugs). `fb`'s result is normalized to a pair.

**Composition = plain nested calls** (the simpler option, chosen): every policy's `call`/`run`
takes a 0-arg callable and AScript closures make nesting natural — `compose([...])` would add
an ordering DSL for something the language already expresses. Parked sketch in §9.3.
**Wrap-order semantics, precisely** (docs carry this verbatim with both diagrams):

```text
// TIMEOUT INSIDE RETRY — each attempt gets its own budget; total worst-case ≈ attempts × ms.
await retryP.call(() => resilience.deadline(200, callBackend))   // re-attempts a slow call

// TIMEOUT OUTSIDE RETRY — one budget for ALL attempts; expiry mid-backoff cancels retrying.
await resilience.deadline(500, () => retryP.call(callBackend))   // bounds the whole operation
```

General rule documented: *the outermost policy sees the inner composite as one operation* —
breaker-outside-retry counts an exhausted retry sequence as ONE failure (usually right);
retry-outside-breaker hammers an open breaker with rejections (usually wrong, and `retryIf:
(e) => e.code != "breaker-open"` is the published idiom). The gateway example (§10) composes
`deadline(outer) → breaker → retry(deadline-aware) → fetch` and explains each layer.

### 3.6 Singleflight

```text
let f1 = resilience.singleflight("user:42", fetchUser)   // starts ONE flight
let f2 = resilience.singleflight("user:42", fetchUser)   // joins it — fetchUser NOT called again
print(await f1 == await f2)                              // same result, one execution
```

Per-isolate flight table on the module's `Interp` side-state (§6.1): `IndexMap<String,
SharedFuture>`. `singleflight(key, fn)` (key: string, Tier-2 otherwise; fn: 0-arg callable):

1. key present → return `Value::Future(existing.clone())` — **verified sound:** N awaiters on
   one `SharedFuture` all park on `ResultCell::get`'s `notify_waiters` loop and clone the same
   stored result (`src/task.rs:63-81`); a stored `Control::Panic` re-raises in EVERY awaiter
   (`task.rs:190-197` + the `task_mod.rs:10-11` doc) — the brief's "same panic to all awaiters"
   requirement holds by construction and is pinned by a test anyway (§10).
2. absent → create `SharedFuture::new()` (taskless), insert, then `spawn_local` a driver task
   that runs `fn()` (driving a returned future), `resolve`s the cell with the `Result<Value,
   Control>`, and removes the table entry (entry removal **in the driver, after resolve** — so
   a key is re-flyable the moment its result is delivered; **results are NOT cached** — that is
   memoize's job, §3.7). Return the handle.

Lifecycle: the driver holds the `ResultCell` (not the handle — the `task.rs` split); the table
holds the handle, so the flight is NOT cancelled when callers drop their futures mid-flight
(semantics: a flight completes for whoever joins next; brief-consistent and herd-safe). The
driver task is aborted only at isolate teardown (it is short-lived by nature). The table entry
is removed on resolve in all paths (success, panic, propagate) — no leak; a test asserts the
table is empty after each scenario. Re-entrancy (fn itself singleflights the same key) joins
its own flight's future and would deadlock — documented as misuse, exactly like awaiting your
own future (not detectable in general; same posture as the actor non-reentrancy doc).

### 3.7 Stampede-protected memoize (lru + singleflight + TTL)

```text
let cache = resilience.memoize({ max: 1000, ttlMs: 5000 })
let [v, err] = cache.get("user:42", fetchUser)   // hit → cached; miss → ONE flight per key
cache.delete("user:42"); cache.clear()
```

`__store` = a `std/lru` handle (`max` entries — shipped recency + eviction); entries are
`{value, atMs}`. `get(key, fn)`: lru hit AND (`ttlMs` absent OR `now - atMs < ttlMs`, `now` =
det-routed `clock_monotonic_ms`) → `[value, nil]` (counter `…_memoize_hits_total`); else
singleflight on a cache-scoped key (an internal `__sfPrefix` ensures two caches never collide
on the global flight table): the flight runs `fn()`, and **only a success** (plain value or
ok-pair) is stored; err pairs and panics pass through uncached (negative caching is parked,
§9.3). Concurrent misses on one key → one `fn()` run, every caller gets the flight's result —
the stampede protection. TTL expiry is lazy (checked on read; lru capacity bounds memory — no
sweeper task).

## 4. (reserved — folded into §3.5; section numbering kept stable for review cross-refs)

## 5. Deadline propagation — the ONE runtime seam (task-locals)

### 5.1 Placement decision (designed against `src/task.rs` + the spawn-site reality)

**Decision: a `tokio::task_local!` cell in `src/interp.rs`, CORE (not feature-gated), holding
`Cell<Option<Rc<TaskLocals>>>` — the SP12 `TELEMETRY_CURRENT` pattern made engine-complete.**

```rust
/// RESIL §5: the current task's ambient locals (deadline, trace id). An immutable,
/// Rc-shared record: setting a value builds a NEW Rc (copy-on-write), so a child task
/// captured at spawn time is forever isolated from the parent's later scopes — the
/// same isolation argument as TELEMETRY_CURRENT (interp.rs:55-67).
pub(crate) struct TaskLocals {
    /// Absolute monotonic deadline (ms, the clock_monotonic_ms domain), if any.
    pub deadline_at_ms: Option<f64>,
    /// Ambient trace/request id, if any.
    pub trace_id: Option<Rc<str>>,
}
tokio::task_local! {
    pub(crate) static TASK_LOCALS: Cell<Option<Rc<TaskLocals>>>;
}
```

Alternatives considered and rejected **against reality**:
- **On `SharedFuture` / the task cell (`src/task.rs`):** the `SharedFuture` is the *result
  handle*, deliberately not held by the running task (`task.rs:13-19` — the cancel-on-drop
  split depends on the task NOT holding its handle). Locals must be readable from *inside* the
  body with no handle in scope, and synchronous code (no spawn at all) needs them too — the
  handle is the wrong home by the architecture's own design.
- **An `Interp` side-table keyed by task identity:** there is no task identity in the runtime
  today (no field carries `tokio::task::Id`); adding one means a `RefCell<HashMap>` probe on
  EVERY consult (a measurable cost where the task-local read is a TLS hit + `Cell` copy), plus
  explicit cleanup on abort (cancel-on-drop kills tasks without notice — entries would leak).
  The task-local's storage dies with the task by construction.
- **A plain `Interp` cell (no task-local):** concurrent sibling tasks would observe each
  other's deadlines — exactly the cross-task leak SP12 documents the task-local exists to
  prevent (`interp.rs:56-60`).

**Copy-on-spawn inheritance:** at every user-code async spawn site, capture
`task_locals_capture()` (a `try_with` clone of the `Rc` — one refcount bump) and wrap the
spawned body in `task_locals_scope(captured, fut)` (`.scope(Cell::new(parent), fut)`).
The complete spawn-site inventory (verified §1; the same sites get the telemetry fix #1):

| site | engine |
|---|---|
| `interp.rs:5321` (async fn) | tree-walker — already telemetry-wrapped; add locals |
| `interp.rs:5908` (async method) | tree-walker — ditto |
| `interp.rs:5982` (static async) | tree-walker — ditto |
| `vm/run.rs:1747` (`Op::Call` async closure) | VM — **gets BOTH wraps (fix #1)** |
| `vm/run.rs:5709` (static async closure) | VM — ditto |
| entry points | `telemetry_root_scope` (`interp.rs:91-103`) generalizes to `ambient_root_scope` (root telemetry scope + root `TASK_LOCALS` scope) — one rename, all entry points get the cell in scope so `try_with` never errs on the main task |

NOT wrapped (with reasons, documented in the module doc): `task_mod.rs` race-resolver tasks
(`:148` — they only await existing futures; no user code runs in them), worker isolates (locals
do NOT cross the airlock — §7.1; a worker body starts with empty locals, honest and documented),
generators (`coro.rs` bodies are lazily polled INSIDE the resuming caller's task —
`gen.next()` therefore sees the *resumer's* current locals ambiently; resume-time semantics,
documented; correct for deadlines: the budget that matters is the resuming request's),
http_server connection tasks (`http_server.rs:1801` — each request **starts fresh** by design;
a serve-level inherited deadline would be wrong; §6.4's `{deadlineMs}` is the per-request way),
and the internal bridges (`interp.rs:2369/2468`, postgres connection driver — no user code).

**Zero-cost when unused (Gate 12):** the consult is `TASK_LOCALS.try_with(|c| …)` returning
`None` fast (TLS lookup + `Cell` read of an `Option<Rc>` — the clone happens only when `Some`);
the spawn-site capture is the same `try_with` + an `Option<Rc>` clone (`None` → nothing). The
required deliverable is a `tests/vm_bench.rs`-style A/B (the `dbg_zero_cost_gate` precedent,
goal.md DBG entry): async-spawn-heavy corpus (`async_inline`-shaped), locals never set, geomean
≈1.0× vs the pre-RESIL baseline, same-session (Gate 16).

### 5.2 `resilience.deadline(ms, fn)` and `deadlineRemaining()`

```text
let [v, err] = resilience.deadline(250, () => handle(req))   // err.code "deadline-exceeded"
resilience.deadlineRemaining()    // ms left (number ≥ 0) | nil when no deadline is set
```

`deadline(ms, fn)`: compute `newAt = clock_monotonic_ms(real) + ms`; effective deadline =
`min(existing, newAt)` (**nested deadlines only shrink** — a callee can never extend its
caller's budget; the gRPC deadline rule); build a new `Rc<TaskLocals>`; **save → set → run →
restore** around the call (the `telemetry.span` cell discipline, `interp.rs:64-66`), restore on
ALL exits. Enforcement is both-ways: (a) the body's result is raced against
`tokio::time::sleep(remaining)` (the `task_timeout` select shape, `task_mod.rs:181-189`) — on
expiry, return `[nil, {code:"deadline-exceeded"}]` and drop the body future (cancel-on-drop
cancels eagerly-spawned async work; a synchronous body cannot be preempted — documented, same
truth `task.timeout` lives with); (b) the local lets downstream ops fail fast / clamp (§5.4).
If already expired on entry (`remaining ≤ 0`): immediate err pair, `fn` never runs.

### 5.3 Public generic task-locals? — NO in v1 (decision + justification)

`task.local(key)` / `task.withLocal(key, v, fn)` are **not** exposed in v1. Justification:
generic ambient storage is dynamic scoping — invisible coupling that the language's design
("no hidden control flow") deliberately avoids; it immediately raises worker-boundary
questions (ship the map? which values are sendable?), GC questions (arbitrary `Value`s in a
non-traced TLS cell could *hide* cycle participants from the collector — `TaskLocals` avoids
this by holding only `f64`/`Rc<str>`, acyclic by type), and det/replay questions (user-visible
ambient state mutated off the event log). The internal record carries exactly the two fields
backend hosting needs (`deadline_at_ms`, `trace_id`). Public generic locals are **recorded as
a parked future** (§9.3) behind those three questions — additive whenever answered.

### 5.4 Deadline-aware I/O (the consult sites — designed against what exists)

A core helper on `Interp` (beside the clock seams):

```rust
/// Remaining deadline budget in ms: Some(0.0_max'd) when a deadline local is set, else None.
/// Cost when unused: one TLS try_with + Cell read (§5.1; Gate-12 benched).
pub(crate) fn deadline_remaining_ms(&self) -> Option<f64>
```

| op | shipped reality | RESIL behavior |
|---|---|---|
| `std/http` client request | per-request `timeout {connect, read, total}` → reqwest builder (`net_http.rs:572-592`) | effective total = `min(requested-or-none, remaining)`; `remaining ≤ 0` → immediate `[nil, {code:"deadline-exceeded"}]` before any connect. Expiry message keeps the deadline code (distinct from a plain timeout). |
| `std/postgres` / `std/redis` ops | **no per-op timeout exists** (verified by grep — nothing to `min()` with) | pre-check (`remaining ≤ 0` → immediate err pair) + wrap the op's await in a remaining-budget `tokio::select!` (the `task_timeout` shape) → `deadline-exceeded` err pair on expiry. The underlying connection's fate on a mid-op abandon follows each driver's existing cancellation story — the plan verifies per driver and the docs state it. |
| `std/sqlite` | **synchronous** rusqlite calls | pre-check ONLY (an in-progress sync query cannot be preempted) — documented honestly. |
| `resilience.limiter.acquire` / `bulkhead.run` parking | §3.2.1/§3.3 | park raced against remaining; expiry → `deadline-exceeded` (don't queue past your budget). |

All consult sites are inside their modules' existing `#[cfg(feature)]`s; the helper itself is
core. Programs that never set a deadline take the `None` branch everywhere — byte-identical.

### 5.5 Trace/request id

`resilience.withTrace(id, fn)` (save→set→restore, same cell discipline; id: string) and
`resilience.traceId() -> string | nil`. v1 integrations: `std/log` records gain a `traceId`
field when one is set (one `try_with` in the record builder — zero-cost `None` path), and the
SP12 spans likewise attach it as an attribute when telemetry is active. The gateway example's
middleware shows the idiom: `server.use((req, next) => resilience.withTrace(req.headers
["x-request-id"] ?? uuid.v4(), () => next(req)))`.

### 5.6 Determinism interaction (verified)

The locals themselves are per-task plain values set/read entirely by script flow — **no det
interaction** (nothing random, nothing wall-clock-stored; `deadline_at_ms` is in the
`clock_monotonic_ms` domain). The *expiry decisions* read the det-routed monotonic seam
(`interp.rs:1393`), so under SP9 Record/Replay every `deadlineRemaining()`/fail-fast verdict is
event-sourced and replays exactly. The race-based enforcement sleep is timing-only (the
documented exemption class). `workflow` bodies composing RESIL policies get the
`workflow-determinism` lint story unchanged (policies use the same seams the lint reasons
about).

## 6. Observability

### 6.1 Metrics: own minimal per-isolate registry; telemetry MIRRORED, not required (decision)

What exists (verified): `std/telemetry` keeps real cumulative local instruments
(`MetricInstrument`/`MetricPoint`, `telemetry/model.rs:155-186`) — but it is (a) a no-op until
`telemetry.init(exporter)` runs (the runtime-opt-in design, `telemetry/mod.rs` module doc),
(b) a **push** pipeline (OTLP/Sentry/PostHog exporters), and (c) behind the `telemetry`
feature while RESIL must work in any `resilience` build. Requiring `telemetry.init` for
`/metrics` to show breaker state would be wrong-by-default for the headline use case.

**Decision:** `std/resilience` owns a minimal always-on per-isolate registry —
`RefCell<ResilRegistry>` on a new `#[cfg(feature = "resilience")]` `Interp` field (the
`std/log`-style Interp-stateful singleton; also home of the singleflight table §3.6):
counters and gauges only, `IndexMap<(name, sorted-label-key), f64>` per metric (the
`attr_key` canonicalization trick, `telemetry/model.rs:388`). Histograms are NOT in the
internal registry v1 — RESIL's own signals are counts/gauges; request-latency histograms are
telemetry's job (`telemetry.histogram`), and `metricsHandler` documents that split. Policies
ADDITIONALLY mirror their signals through the SP12 soft hook
(`Interp::telemetry_register_instrument`/`telemetry_record_metric`, `interp.rs:2023/2061`,
`#[cfg(feature = "telemetry")]`, no-op until init — the `std/ai` consumption pattern,
`stdlib/ai/mod.rs:13`) and emit transition events via `log.debug` when the `log` feature is
present. Nothing is duplicated: the registry is ~60 lines over an IndexMap; the OTLP pipeline
is reused, not rebuilt.

Metric set (Prometheus naming, `ascript_resilience_` prefix, `{name}` label from each policy's
`name` opt, keyed-limiter evictions per §3.2.2):
`breaker_state{name}` (gauge 0/1/2 = closed/open/halfOpen), `breaker_calls_total{name,result=
success|failure|rejected}`, `breaker_transitions_total{name,to}`, `limiter_acquired_total
{name}`, `limiter_rejected_total{name}`, `limiter_evictions_total{name}`, `bulkhead_in_flight
{name}` (gauge), `bulkhead_shed_total{name}`, `retry_attempts_total{name,outcome}`,
`retry_budget_exhausted_total{name}`, `singleflight_joins_total`, `memoize_{hits,misses}_total
{name}`, `deadline_exceeded_total`.

### 6.2 `resilience.metricsHandler()` — the Prometheus text exporter

Returns a **callable handler** the server mounts directly:
`server.get("/metrics", resilience.metricsHandler())`. Mechanism: a `Value::NativeMethod`
(callable — `task_mod.rs:87-91` lists it in the callable set; `register_route` accepts any
`is_callable`, `http_server.rs:1046`) whose receiver is a fields-only `NativeKind::Resilience`
object and whose dispatch arm renders the registry as **Prometheus text exposition format
0.0.4**: `# TYPE` lines, deterministic ordering (registry IndexMap insertion order; labels
pre-sorted by `attr_key`), label values escaped (`\\`, `\"`, `\n`), returning
`{status: 200, headers: {"content-type": "text/plain; version=0.0.4"}, body}` — the
`value_to_response` object shape (`http_server.rs:835`). Per-isolate truth: under
`workers: N`, each isolate exports ITS OWN registry; Prometheus aggregates across scrapes of
distinct targets — with SO_REUSEPORT the scrape lands on ONE isolate per request, which the
docs call out loudly (§7 — and the honest guidance: per-isolate metrics + the actor pattern,
or scrape-level aggregation acceptance).

### 6.3 `resilience.health({checks})` — health/readiness handlers

```text
server.get("/healthz", resilience.health({}))                     // liveness: always 200
server.get("/readyz", resilience.health({ checks: { db: pingDb, cache: pingCache },
                                           timeoutMs: 1000 }))
```

Same NativeMethod mechanism; the checks object lives in the handle's `NativeObject.fields`.
On request: run each check fn (futures driven) under a per-check `timeoutMs` deadline (reusing
§5.2); a check passes on a truthy return / ok-pair, fails on falsy, err pair, panic
(`recover`-style containment — one bad check never 500s the endpoint), or timeout. Response:
all pass → `200 {"status":"ok","checks":{...}}`; any fail → `503 {"status":"degraded",
"checks":{...}}` with per-check `{ok, error?}` detail, JSON via the same serialization the
server already uses. Checks run sequentially in registration order (deterministic; readiness
endpoints are low-QPS — parallel checks are a parked nicety).

### 6.4 `resilience.handler(policies, fn)` — the HTTP wrapper helper

The "helpers for http_server handlers" deliverable: wraps a route handler with policies and
maps rejection codes to HTTP statuses so the 503-path is one line:

```text
server.get("/quote", resilience.handler({
    limiter: perClient, key: (req) => req.headers["x-api-key"] ?? "anon",
    bulkhead: bh, breaker: b, deadlineMs: 500,
}, handleQuote))
```

Fixed, documented order (outermost first): **keyed/plain limiter → bulkhead → breaker →
deadline → fn** — shed the cheapest checks first, give the breaker visibility into real
attempts only, set the budget innermost so it covers exactly the handler. Mapping:
`rate-limited` → 429 (+ `retry-after` from the bucket's deficit), `bulkhead-full` → 503,
`breaker-open` → 503 (+ `retry-after` from cooldown remaining), `deadline-exceeded` → 504;
any other `[v, err]` from `fn` passes through to the server's existing semantics
(`value_to_response` → 500 path). Every key optional; retry is deliberately NOT in the wrapper
(server-side self-retry is an anti-pattern; compose explicitly if wanted). Returns the same
NativeMethod-backed callable shape as §6.2.

## 7. Per-isolate honesty

### 7.1 The model, stated without hedging

Every policy's state — breaker windows, buckets, bulkhead permits, flight tables, memoize
entries, the metrics registry, task-locals — is **per-isolate**. Under `server.serve({workers:
N})` there are N independent copies of everything. This is usually CORRECT (each Envoy sidecar
/ each service replica runs its own breaker; per-replica limiting is how local rate limiting
deploys in practice) and it is the only model consistent with shared-nothing isolation. The
`__local` Native marker (§2.2) makes the boundary LOUD: shipping a policy to a worker raises
the existing non-sendable field-path panic instead of silently forking its state. Task-locals
likewise do not cross the airlock (a `worker fn` body starts with empty locals); the docs show
passing an explicit deadline-ms argument when budget must cross.

### 7.2 Global state = a `worker class` actor (shipped pattern, shipped example)

When state genuinely must be process-global (a strict global rate limit, a cluster-wide
breaker), the documented pattern is a dedicated actor — one isolate OWNS the policy, everyone
else asks it (FIFO mailbox, async methods returning `future<T>`):

```text
worker class GlobalLimiter {
    init() { self.lim = resilience.limiter({capacity: 1000, refillPerSec: 500}) }
    async fn tryAcquire() { return self.lim.tryAcquire() }
}
let gl = await GlobalLimiter.spawn()
if (await gl.tryAcquire()) { ... }
```

`examples/advanced/resilient_gateway.as` includes this (with the honest note: every check is a
mailbox round-trip — use it for low-QPS global decisions, per-isolate policies for the hot
path). No new machinery — this is exactly Workers Spec B.

## 8. Determinism & four-mode identity (Gates 1, SP9)

- **Pure stdlib + the §5 seam:** no grammar, no opcode, no `Value` kind, no `.aso` change
  (`ASO_FORMAT_VERSION` stays 27 — negative-space-pinned). The call-site hooks are mirrored in
  both engines from ONE `call_resilience_method` implementation; the spawn-site wraps are
  added to both engines in the same task — four-mode identity is structural, then proven by
  the differential over the new examples in both feature configs.
- **Observable time/random → det-routed (verified seams):** breaker cooldown
  (`clock_monotonic_ms`), limiter refill + retry-after, memoize TTL, deadline set/remaining/
  expiry verdicts — all through `interp.rs:1382/1393` (the same seams `time.now`/
  `time.monotonic` use). Nothing in RESIL draws observable randomness; if a future policy does,
  it must use `next_seeded_f64` (`interp.rs:1403`, the `math.random` seam).
- **Timing-only sleeps exempt:** backoff/jitter/pacing sleep DURATIONS follow the shipped
  `task_mod.rs:444-452` exemption (perturb wall pacing, never values).
- **Seeded/virtual-clock tests:** policy unit tests that depend on elapsed time run under
  `run_source_deterministic` (`lib.rs:678`) and/or drive `DeterminismContext`/`VirtualClock`
  directly (`det.rs:149`), so breaker-cooldown and TTL boundaries are EXACT, not sleep-flaky.
- **Gate-15 posture:** no new engine configuration → no new differential mode or kill switch;
  the new examples join the existing corpus; the worker-serialize fuzz target already covers
  the `__local` refusal byte-path (Native = non-sendable, unchanged).

## 9. Scope & rejected / parked

### 9.1 In scope (v1)
Everything in §§2–6: six policy kinds + fallback + singleflight + memoize + deadline/trace
locals + the I/O consult sites + registry/metrics/health/handler + the worker-actor pattern
example + the two Gate-14 fixes from §1.

### 9.2 Rejected (with reasons)
- **A new `Value` kind or `ResourceState` policy objects** — the schema tagged-Object model is
  shipped, hook-compatible, introspectable, and GC-trivial; Native-handle policies would add
  resource-lifecycle and serializer arms for zero capability gain.
- **`resilience.compose([...])`** — nested calls already express composition in the language;
  a combinator adds an ordering DSL and a second place wrap-order semantics can be wrong.
  (Sketch: `compose(list) -> {__resil:"composed", list}` whose `call` folds right-to-left;
  additive later.)
- **Time-based breaker window** — per-call clock reads = per-call det events + flaky tests;
  count-based is deterministic and the resilience4j default. (Additive later as `windowMs`.)
- **Public generic task-locals** — §5.3 (dynamic-scoping footgun; worker/GC/det questions).
- **Background sweeper tasks** (breaker cooldown timers, memoize TTL reapers) — lazy
  evaluation on access has nothing to leak/cancel and is deterministic.
- **A separate histogram implementation in the registry** — telemetry owns histograms; don't
  duplicate (§6.1).
- **Touching `sync.rateLimiter`** — shipped semantics stay (§3.2.3).

### 9.3 Parked WITH sketches (recorded, not dropped)
- **Hedged requests:** `resilience.hedge({delayMs, max}, fn)` — fire `fn` again after
  `delayMs` without cancelling the first; first settle wins via a `SharedFuture` rendezvous
  (the `task.race` winner cell, `task_mod.rs:142-162`); losers cancelled by handle-drop.
  Blockers: needs idempotency guidance + per-attempt deadline interplay; wants the
  tail-latency bench story first.
- **AIMD adaptive concurrency:** a bulkhead whose `limit` self-tunes — additive increase per
  success window, multiplicative decrease on shed/deadline signals (Netflix
  concurrency-limits/Gradient). Builds on §3.3's permit machinery + a latency EWMA; needs the
  det story for the EWMA clock reads and a load-test harness to validate gain.
- **`std/k8s`:** downward-API helpers (`k8s.inCluster()`, namespace/pod metadata, graceful
  preStop coordination with CNTR's `onShutdown`, probe wiring sugar over §6.3). Mostly fs/env
  reads + conventions; waits on CNTR's signal/drain plumbing landing first.
- **Time-based breaker window, `compose`, negative caching for memoize, parallel health
  checks, public task-locals** — each noted at its section.

## 10. Testing & deliverables (Gates 9/10/13 mapping)

- **Unit tests (happy + edge, both feature configs)** per policy: breaker — window-boundary
  exactness (`minCalls-1` never opens; rate exactly at threshold opens), half-open probe-budget
  race (N concurrent probes, exactly `halfOpenMax` admitted), cooldown boundary under
  `VirtualClock`, rejected-calls-don't-feed-window, reset; limiter — refill math at the clock
  seam, `tryAcquire(n)` atomicity, capacity clamp, zero-refill bucket, deadline-aware park;
  keyed — per-key isolation, **eviction** (key evicted → fresh bucket; eviction counter),
  non-string key panic; bulkhead — cap honored under concurrency, queue boundary
  (`queue`th waiter parks, `queue+1`th sheds immediately), all-paths release on panic,
  waiting-counter decrement on deadline expiry; retry v2 — the three SHIPPED tests untouched,
  fixed backoff, full jitter bounds, `retryOn:"error"` pair retry + exhaustion code, `retryIf`
  short-circuit, **budget exhaustion** (counters, not clocks), budget-on-`task.retry` panic;
  fallback — pair/panic/fb-panic matrix; singleflight — N concurrent callers / one execution,
  **panicking flight delivers the SAME panic to all awaiters** (the §3.6 SharedFuture
  argument, pinned), table emptied after success AND failure, key reusable after settle;
  memoize — stampede (one fn run under N concurrent gets), TTL boundary under virtual clock,
  eviction via `max`, errors-not-cached; deadline — nesting-shrinks, restore-on-panic,
  expired-fast-fail, http/postgres/redis/sqlite consult behaviors, `deadlineRemaining`
  det-replay; hook — call-position-only (bare member reads fields), unknown-method panic,
  `OptMember` not routed; airlock — policy-to-worker = field-path panic (`__local`).
- **Examples:** `examples/resilience.as` (intro: breaker trip/recover via count-based
  determinism, limiter `tryAcquire` with near-zero refill, bulkhead shed, retry v2,
  singleflight, memoize, deadline — output four-mode byte-identical) +
  `examples/advanced/resilient_gateway.as` (production-shaped: trace middleware, keyed
  limiter, `resilience.handler` route wrapping, breaker+retry+deadline composed per §3.5's
  documented order, `/metrics` + `/readyz` mounted, the §7.2 actor, fully error-handled,
  deterministic output, `maxRequests`-bounded so it runs to completion in the corpus).
- **Negative space:** `tests/resil_negative_space.rs` — `ASO_FORMAT_VERSION == 27`, no new
  opcode, hook-ordering pin, `task.retry` legacy-contract pins.
- **Benches:** the §5.1 zero-cost task-local gate + the Gate-17 `vm_bench` geomean re-run
  (RESIL touches the async spawn sites — the floor must be re-proven, not assumed) + RSS
  (Gate 18).
- **Docs:** `docs/content/stdlib/resilience.md` (NEW page → **`NAV` entry in
  `docs/assets/app.js`** — the orphan gotcha), cross-link from
  `docs/content/language/workers.md` (per-isolate state + the actor pattern §7.2),
  `stdlib/async.md` (task.retry v2 keys), `README.md` stdlib table, `CLAUDE.md` subsystem
  note, `superpowers/roadmap.md`, `goal-perf.md` status flip.

## 11. Brief-vs-code deltas (recorded so review sees them)

1. **`sync.rateLimiter` already exists** — RESIL adds the token-bucket/keyed limiter beside
   it; relationship documented (§3.2.3), nothing removed.
2. **Retry jitter det-routing:** the brief asked for det-routed jitter; the shipped
   `task.retry` jitter carries a reviewed SP9 **timing-only exemption** (`task_mod.rs:
   444-452`). RESIL follows the shipped exemption for sleep durations and det-routes every
   *observable* verdict instead (§8) — stronger precedent, identical replay fidelity.
3. **`min(theirTimeout, remaining)` for sql/redis:** postgres/redis have NO per-op timeout to
   min with (verified); the design pre-checks + budget-wraps instead; sqlite (synchronous)
   pre-checks only (§5.4).
4. **`maxDelay`** → the shipped key is `maxMs`; kept (compat over brief naming).
5. **Found defects** (§1): VM spawn sites missing `telemetry_scope`; stale telemetry
   feature-default doc-comment — both fixed in-branch per Gate 14.

## 12. Grounding (every claim verified in source, 2026-06-12)

`src/task.rs:33-160` (ResultCell/SharedFuture multi-awaiter + panic delivery + cancel-on-drop
split + `detach`) · `src/stdlib/task_mod.rs:75-110` (spawn/detach), `:170-190` (timeout
select), `:203-318` (retry v1 contract), `:444-472` (jitter exemption), tests `:484-549` ·
`src/stdlib/sync.rs:51-70/:483-530/:563-583` (semaphore + lost-wakeup acquire + withPermit),
`:83-111` (existing rateLimiter) · `src/stdlib/lru.rs:33-58/:103-204` (LruState + methods +
eviction) · `src/stdlib/schema.rs:131-135/:160-212` (tagged objects, SCHEMA_KINDS,
is_schema_value/method) · `src/interp.rs:4122-4216` (the call-site hook ladder: schema 4140,
workflow 4156, worker-spawn 4174, shared 4186, actor 4197, fallback ordering note 4208-4216),
`:55-113` (TELEMETRY_CURRENT + scope/capture + root scope), `:1376-1406` (det seams),
`:1690-2110` (telemetry state access + soft hook + instruments), `:5306-5341/:5898-5925/
:5975-6005` (tree-walker async spawn sites) · `src/vm/run.rs:4838-4925` (dispatch_method hook
ladder), `:1728-1756/:5704-5717` (VM async spawn sites — no telemetry wrap, fix #1) ·
`src/stdlib/telemetry/mod.rs:40-120` (exports, runtime-opt-in, noop_handle) ·
`src/stdlib/telemetry/model.rs:155-186/:388` (MetricInstrument/MetricPoint/attr_key) ·
`src/stdlib/http_server.rs:1036-1110` (register_route/is_callable/verbs), `:835`
(value_to_response), `:1801` (per-connection task), middleware `run_chain:2042` ·
`src/stdlib/net_http.rs:572-592` (timeout opts) · postgres/redis: no `timeout` hits (grep) ·
`src/stdlib/mod.rs:114-211/:221-279/:325-374/:991/:1086` (registration, STD_MODULES,
required_cap + completeness tests) · `src/det.rs:149-360` (VirtualClock/SeededRng/events) ·
`src/lib.rs:556` (entry-point root scope), `:678` (run_source_deterministic) ·
`Cargo.toml:168` (default features incl. `telemetry`) · `src/vm/aso.rs:167`
(`ASO_FORMAT_VERSION = 27`). External precedents: resilience4j (count-based default window,
half-open permitted-calls), Envoy local rate limiting (per-descriptor bounded buckets),
AWS full-jitter backoff, the SRE-book retry budget, gRPC deadline propagation (min-rule).
