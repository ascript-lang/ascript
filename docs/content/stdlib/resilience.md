::: eyebrow Standard library

# Resilience policies

`std/resilience` is AScript's backend-hosting policy kit: **circuit breaker, token-bucket rate
limiting (plain + keyed), bulkhead + load shedding, retry v2, fallback, singleflight,
stampede-protected memoize, deadline propagation, Prometheus metrics, and health handlers** —
all per-isolate, all composable by plain nested calls.

```ascript
import * as resilience from "std/resilience"

let b = resilience.breaker({ name: "payments", window: 20, failureRate: 0.5 })
let [v, err] = b.call(fetchData)    // err.code == "breaker-open" when rejected
print(b.state())                     // "closed" | "open" | "halfOpen"
```

## Call-position-only hook

Policies are plain tagged `Object`s (`{ __resil: "..." }`), the same model as `std/schema`.
Method-style calls (`b.call(fn)`, `lim.tryAcquire()`) are routed through a **call-site hook**
in both engines — **call-position only**. A bare member read (`b.failureRate`, `lim.capacity`)
returns the stored config field; only a parenthesised call (`b.state()`) invokes the method.

`OptMember` (`b?.call(fn)`) is **not** routed through the hook (schema parity — use regular
member calls with null-checks when needed).

---

## Circuit breaker

`resilience.breaker(opts) -> breaker policy`

Protects a dependency by tracking call outcomes in a sliding **count-based** window (last N
calls) and opening the circuit when the failure fraction reaches the threshold. The window is
count-based — deterministic, no per-call clock reads, reproducible under SP9 Record/Replay.

### Options

| Option | Default | Description |
|---|---|---|
| `name` | `"default"` | Label for metrics and error messages |
| `failureRate` | `0.5` | Open when window failure fraction ≥ this; must be in (0, 1] |
| `window` | `20` | Sliding window size — last N calls |
| `minCalls` | `10` | Minimum calls in window before any verdict |
| `cooldownMs` | `30000` | Open → halfOpen after this many ms |
| `halfOpenMax` | `3` | Max concurrent probe calls while halfOpen |

All integer options must be ≥ 1; a non-positive value is a Tier-2 panic at construction.

### Methods

| Method | Returns | Description |
|---|---|---|
| `b.call(fn)` | `[value, err]` | Run `fn()`, recording success/failure; `err.code == "breaker-open"` when rejected |
| `b.state()` | `string` | `"closed"` \| `"open"` \| `"halfOpen"` |
| `b.stats()` | `object` | `{ state, calls, failures, rejected, windowFailureRate }` |
| `b.reset()` | `nil` | Back to closed, window cleared (ops/test hook) |

### State machine

`closed` — (window ≥ `minCalls` AND failure fraction ≥ `failureRate`) → `open` (records
`openedAtMs` via the monotonic clock) — (next `call` after `cooldownMs` elapsed) → `halfOpen`
— (probe success × `halfOpenMax`) → `closed` (window cleared). Any probe **failure** while
halfOpen reopens with a fresh cooldown immediately. While halfOpen, at most `halfOpenMax` calls
are admitted concurrently; further calls are rejected with `"breaker-open"`.

### Failure classification

`b.call(fn)` calls `fn()` (0-arg; a returned future is driven to completion). Outcome:
- Plain value → **success**, returned as `[v, nil]`.
- Result pair `[v, err]` with `err != nil` → **failure recorded**, pair passed through unchanged.
- Result pair with `err == nil` → **success**, pair passed through.
- `Control::Panic` → **failure recorded**, panic **re-raised** (never swallowed — use `recover`).
- `?`-propagation / `exit()` → passed through **unrecorded** (control flow, not dependency health).

Rejected calls (open/halfOpen budget exhausted) record a `rejected` counter but do **not** enter
the window — a breaker must not feed on its own rejections.

```ascript
import * as resilience from "std/resilience"

// failureRate 0.5, window 4, minCalls 4: 2 ok + 2 fail = exactly 50% → open.
let b = resilience.breaker({
  name: "payments", failureRate: 0.5, window: 4, minCalls: 4,
  cooldownMs: 999999, halfOpenMax: 1,
})
fn ok()   { return 42 }
fn fail() { return [nil, {message: "upstream down", code: "err"}] }

b.call(ok); b.call(ok); b.call(fail); b.call(fail)
print(b.state())           // "open"

let [_v, re] = b.call(ok)
print(re.code)             // "breaker-open" — rejected, not recorded in window
print(b.stats().rejected)  // 1

b.reset()                  // closed, window cleared
print(b.state())           // "closed"
```

---

## Rate limiter (token bucket)

### Plain limiter

`resilience.limiter(opts) -> limiter policy`

Smooth token-bucket: tokens refill continuously at `refillPerSec` tokens per second, capped at
`capacity`. Distinct from `sync.rateLimiter` (which is a fixed-window counter — see
[Async & concurrency](async#std-sync)).

#### Options

| Option | Required | Default | Description |
|---|---|---|---|
| `capacity` | yes | — | Bucket size (positive number ≥ 1) |
| `refillPerSec` | yes | — | Refill rate (non-negative finite number; 0 = draining bucket) |
| `name` | no | `"default"` | Label for metrics |

#### Methods

| Method | Returns | Description |
|---|---|---|
| `lim.tryAcquire(n?)` | `bool` | Atomically take `n` tokens (default 1); `false` if unavailable — never parks |
| `await lim.acquire(n?)` | `nil` | Park until `n` tokens available; respects deadline local (§ Deadline propagation) |

`tryAcquire` is atomic (all-or-nothing under one synchronous borrow). `acquire` computes the
exact deficit sleep and loops, re-checking after each sleep. When a deadline is set and the
refill wait would exceed the remaining budget, `acquire` returns `[nil, {code:"deadline-exceeded"}]`
instead of parking past the budget. A `refillPerSec: 0` limiter rejects `acquire` with a Tier-2
panic (never refills — use `tryAcquire` for draining buckets).

```ascript
import * as resilience from "std/resilience"

// capacity 2, near-zero refill: deterministic — 2 trues then false.
let lim = resilience.limiter({ name: "api", capacity: 2, refillPerSec: 0.001 })
print(lim.tryAcquire())   // true
print(lim.tryAcquire())   // true
print(lim.tryAcquire())   // false — exhausted
```

### Keyed limiter (per-key token bucket)

`resilience.keyedLimiter(opts) -> keyedLimiter policy`

One independent token bucket per string key, backed by an LRU store bounded to `maxKeys` entries.

#### Options

All `limiter` options, plus:

| Option | Default | Description |
|---|---|---|
| `maxKeys` | `10000` | Max number of per-key buckets to track (positive int ≥ 1) |

#### Methods

| Method | Returns | Description |
|---|---|---|
| `lim.tryAcquire(key, n?)` | `bool` | Atomic per-key check; key must be a string (Tier-2 otherwise) |
| `await lim.acquire(key, n?)` | `nil` | Park per-key; deadline-aware (same as plain limiter) |
| `lim.stats()` | `object` | `{ keys, evictions }` — active key count + total evictions |

**Eviction semantics:** when the LRU store reaches `maxKeys`, inserting a new key evicts the
least-recently-used bucket. The evicted key's spend history is **forgotten** — its next request
starts with a full bucket. Set `maxKeys` above your active-client cardinality; watch
`ascript_resilience_limiter_evictions_total` for undersizing. This is the standard
bounded-memory trade — Envoy's `local_ratelimit` descriptors behave the same way.

```ascript
import * as resilience from "std/resilience"

let perClient = resilience.keyedLimiter({ capacity: 2, refillPerSec: 0.001, maxKeys: 100 })
print(perClient.tryAcquire("A"))   // true
print(perClient.tryAcquire("A"))   // true
print(perClient.tryAcquire("A"))   // false — key A exhausted
print(perClient.tryAcquire("B"))   // true  — key B has a full bucket
```

---

## Bulkhead (concurrency cap + load shedding)

`resilience.bulkhead(opts) -> bulkhead policy`

Bounds concurrent executions (`limit`) and queues a bounded number of waiters (`queue`). When
the queue is full, callers are shed **immediately** (O(1)) rather than queueing unboundedly.

### Options

| Option | Required | Default | Description |
|---|---|---|---|
| `limit` | yes | — | Max concurrent executions (positive int ≥ 1) |
| `queue` | no | `0` | Max parked waiters (non-negative int ≥ 0); 0 = no queueing |
| `name` | no | `"default"` | Label for metrics and error messages |

### Methods

| Method | Returns | Description |
|---|---|---|
| `await bh.run(fn)` | `[value, err]` | Run `fn()` under the bulkhead; `err.code == "bulkhead-full"` when shed |

**Three paths:** (1) permit immediately available → acquire, run, release (all paths including panic);
(2) no permit AND `waiting >= queue` → immediate `[nil, {code:"bulkhead-full"}]` — never parks;
(3) no permit AND `waiting < queue` → park on the semaphore, run after admission, release.

The permit is released on **all** exit paths — success, error pair, panic, propagation. When a
deadline is set, parking races against the remaining budget; expiry → `deadline-exceeded` pair.

```ascript
import * as resilience from "std/resilience"

let bh = resilience.bulkhead({ name: "db", limit: 1, queue: 0 })
fn boom()   { assert(false, "kaboom") }
fn okWork() { return 42 }

// Even when fn panics, the permit is released — the next run still gets in.
let [_bv, berr] = recover(() => bh.run(boom))
print(berr != nil)          // true — panic propagated; permit released
let [bv2, berr2] = bh.run(okWork)
print(bv2)                  // 42 — permit available
print(berr2)                // nil
```

---

## Retry v2

### `task.retry` — stateless retry (v2 keys)

The existing `task.retry(fn, opts?)` is extended with new option keys (all additive, backward-compatible):

| Key | Default | Description |
|---|---|---|
| `attempts` | `3` | Max calls (positive int) |
| `baseMs` | `100` | First backoff window (ms) |
| `maxMs` | — | Optional cap on any single backoff delay |
| `jitter` | `false` | `true` adds up to +50% random jitter; `"full"` = uniform from `[0, delay]`; `"none"` = no jitter |
| `backoff` | `"exponential"` | `"exponential"` (current: `baseMs * 2^i`) \| `"fixed"` (every delay is `baseMs`) |
| `retryOn` | `"panic"` | `"panic"` (default, unchanged) \| `"error"` (retry `[nil, err]` pairs) \| `"both"` |
| `retryIf` | — | `fn(err) -> bool` predicate; `false` → stop retrying immediately |
| `budget` | — | **Policy object only** — not accepted by `task.retry` (Tier-2 panic if passed) |

`task.retry` retries **panics only** by default (`retryOn:"panic"` — the shipped behavior,
unchanged). With `retryOn:"error"`, a returned `[nil, err]` pair is also retried; exhaustion
yields the last pair with `code:"retries-exhausted"` folded in if not already present.
With `retryOn:"both"`, either class is retried. `retryIf` is called with the panic's error
object or the pair's `err`; returning `false` stops retrying immediately; a panic inside
`retryIf` re-raises. After all attempts are exhausted on panics, the **last panic is re-raised**
(unchanged from v1).

```ascript
import * as task from "std/task"

let attempts = [0]
async fn flaky() {
  attempts[0] = attempts[0] + 1
  if (attempts[0] < 3) { return [nil, {message: "transient", code: "err"}] }
  return "recovered"
}

// retryOn:"error" + retryIf: retry only retriable errors.
let v = await task.retry(flaky, {
  attempts: 5, baseMs: 1,
  retryOn: "error",
  retryIf: (e) => e.code == "err",
})
print(v)            // "recovered"
print(attempts[0])  // 3
```

See also [Resilience policies](resilience) for `resilience.retry` — the reusable policy form
with budget control.

### `resilience.retry` — reusable policy with budget

`resilience.retry(opts) -> retry policy`

A policy object that wraps the same retry engine with stateful **retry budget** tracking.

#### Options

All `task.retry` v2 options, plus:

| Key | Default | Description |
|---|---|---|
| `name` | `"default"` | Label for metrics |
| `budget` | `1.0` | Retry budget ratio in (0, 1]: permits a retry only while `retriesSpent < budget × attemptsSeen`. Prevents retry storms during outages. |

#### Methods

| Method | Returns | Description |
|---|---|---|
| `await p.call(fn)` | value / pair | Run `fn()` with all configured retry semantics + budget |
| `p.stats()` | `object` | `{ attemptsSeen, retriesSpent, budget }` |
| `p.reset()` | `nil` | Zero the budget counters |

The budget is count-based (no clock interaction) — deterministic. Passing `budget` to the
stateless `task.retry` is a Tier-2 panic: `task.retry: budget requires a resilience.retry policy`.

```ascript
import * as resilience from "std/resilience"

// budget 0.5: at most half as many retries as attempts seen.
let p = resilience.retry({ attempts: 10, baseMs: 1, retryOn: "error", budget: 0.5 })
```

---

## Fallback

`resilience.fallback(fn, fb) -> [value, err]`

Run `fn()` (futures driven). On success → pass through as `[v, nil]`. On an err pair → call
`fb(err)`. On a `Control::Panic` → call `fb({message: …})` (the panic is **consumed** — this is
the one documented place RESIL swallows a panic; `fallback` is the terminal "always answer
something" layer). `fb`'s own panic re-raises. `fb`'s result is normalized to a pair.

`?`-propagation and `exit()` pass through unchanged (not a fn-level panic or error).

```ascript
import * as resilience from "std/resilience"

fn primary() { return [nil, {message: "no data", code: "err"}] }
let [fv, ferr] = resilience.fallback(primary, (e) => `fallback(${e.code})`)
print(fv)    // "fallback(err)"
print(ferr)  // nil
```

---

## Singleflight

`resilience.singleflight(key, fn) -> future<value>`

Collapse concurrent same-key calls to **one** execution of `fn`. All concurrent callers with the
same key receive a clone of the same `SharedFuture` — when `fn` settles, every awaiter observes
the same value (or the same panic, via `SharedFuture`'s fan-out). Key must be a string (Tier-2
otherwise).

Results are **not cached** — the table entry is removed the moment the flight settles. A
sequential `singleflight("k", fn)` after the first one has settled starts a fresh execution.
For caching, see `resilience.memoize`.

```ascript
import * as resilience from "std/resilience"

async fn singleflightDemo() {
  let calls = [0]
  async fn fetchUser() { calls[0] = calls[0] + 1; return "user:42" }

  let f1 = resilience.singleflight("user:42", fetchUser)
  let f2 = resilience.singleflight("user:42", fetchUser)
  print(await f1 == await f2)  // true — same result
  print(calls[0])              // 1 — fetchUser ran exactly once
}
await singleflightDemo()
```

Callers dropping their futures mid-flight do **not** cancel the flight — the driver holds only
the `ResultCell`, not the handle, so the flight completes for whoever joins next (herd-safe).

---

## Memoize (stampede-protected cache)

`resilience.memoize(opts) -> memoize policy`

LRU cache with singleflight stampede protection and optional TTL. Concurrent misses on the same
key collapse to one `fn` execution; a cache hit skips the call entirely.

### Options

| Option | Default | Description |
|---|---|---|
| `max` | `1024` | Max LRU entries (positive int ≥ 1) |
| `ttlMs` | — | Optional TTL in ms; absent/nil = entries never expire (lazy expiry on read) |
| `name` | `"default"` | Label for metrics |

### Methods

| Method | Returns | Description |
|---|---|---|
| `await cache.get(key, fn)` | `[value, err]` | Hit → `[cached, nil]`; miss → one flight per key |
| `cache.delete(key)` | `nil` | Remove one entry |
| `cache.clear()` | `nil` | Remove all entries |
| `cache.len()` | `int` | Current entry count |
| `cache.stats()` | `object` | `{ hits, misses }` counters |

Keys must be strings (Tier-2 otherwise). Error pairs and panics are **not cached** (negative
caching is not in scope). TTL expiry is lazy — checked on read; no background sweeper task.

```ascript
import * as resilience from "std/resilience"

async fn memoizeDemo() {
  let runs = [0]
  async fn load() { runs[0] = runs[0] + 1; return "value" }
  let cache = resilience.memoize({ max: 100 })

  let g1 = cache.get("k", load)
  let g2 = cache.get("k", load)
  await g1; await g2
  print(runs[0])             // 1 — stampede collapsed to one run

  let [hit, _] = await cache.get("k", load)
  print(hit)                 // "value" — served from cache
  print(runs[0])             // 1 — hit did not re-run load
}
await memoizeDemo()
```

---

## Composition and wrap order

Every policy's `call`/`run` takes a 0-arg callable. AScript closures make nesting natural — no
combinator DSL is needed. The outermost policy sees the inner composite as one operation.

**Both diagrams verbatim from the spec:**

```ascript
// TIMEOUT INSIDE RETRY — each attempt gets its own budget.
// Total worst-case ≈ attempts × ms. Re-attempts a slow call.
await retryP.call(() => resilience.deadline(200, callBackend))

// TIMEOUT OUTSIDE RETRY — one budget for ALL attempts.
// Expiry mid-backoff cancels retrying.
await resilience.deadline(500, () => retryP.call(callBackend))
```

**General rule:** the outermost policy sees the inner composite as one operation. Breaker-outside-retry
counts an exhausted retry sequence as ONE failure (usually correct). Retry-outside-breaker
hammers an open breaker with rejections (usually wrong — use `retryIf: (e) => e.code !=
"breaker-open"` to skip retrying on breaker rejections).

The gateway example (`examples/advanced/resilient_gateway.as`) composes
`deadline (outer) → breaker → retry → fetch` and explains each layer.

---

## Error codes

Policy rejections are **Tier-1 `[nil, err]` pairs** — expected operational outcomes, not bugs.
The `err` object carries `{ message, code }` with stable, documented codes:

| `code` | Raised by | Message shape |
|---|---|---|
| `"breaker-open"` | `b.call` while open / halfOpen budget exhausted | `circuit breaker '<name>' is open` |
| `"rate-limited"` | handler wrapper (§ HTTP wrapper) on a full limiter | `rate limit exceeded` (keyed: `… for key '<k>'`) |
| `"bulkhead-full"` | `bh.run` when queue is full | `bulkhead '<name>' queue is full` |
| `"deadline-exceeded"` | `resilience.deadline` expiry; deadline-aware I/O ops | `deadline exceeded` |
| `"retries-exhausted"` | `resilience.retry` (v2 `retryOn:"error"/"both"`) on pair exhaustion | last err's message |

Misuse (wrong arg types, non-positive capacities, unknown methods, non-string singleflight keys)
is a **Tier-2 panic** — the stdlib-wide rule.

---

## Deadline propagation

### `resilience.deadline(ms, fn) -> [value, err]`

Set a total time budget for `fn` and everything it awaits. Nested deadlines **only shrink** — a
callee can never extend its caller's budget (the gRPC deadline rule).

```ascript
import * as resilience from "std/resilience"
import * as time from "std/time"

async fn deadlineDemo() {
  let ran = [false]
  let [v, err] = await resilience.deadline(50, async () => {
    await time.sleep(500)   // cancelled — body exceeds budget
    ran[0] = true
    return "done"
  })
  print(v)          // nil
  print(err.code)   // "deadline-exceeded"
  print(ran[0])     // false

  // Nested deadlines: inner is clamped to outer budget.
  resilience.deadline(60000, () => {
    let outer = resilience.deadlineRemaining()
    resilience.deadline(120000, () => {
      let inner = resilience.deadlineRemaining()
      print(inner <= outer)   // true — inner clamped to the outer budget
      return nil
    })
    return nil
  })

  // Already-expired deadline fast-fails WITHOUT running the body.
  let bodyRan = [0]
  let [_zv, zerr] = resilience.deadline(0, () => {
    bodyRan[0] = bodyRan[0] + 1
    return 7
  })
  print(zerr.code)    // "deadline-exceeded"
  print(bodyRan[0])   // 0 — fn never called
}
await deadlineDemo()
```

### `resilience.deadlineRemaining() -> number | nil`

Returns the remaining budget in ms (≥ 0) when a deadline is active, `nil` otherwise.

### Honesty note — what is and is not preemptible

The deadline body is raced against a sleep equal to the remaining budget. This cancels **async**
work (any suspension point under `await`). A **synchronous** body (no `await`) cannot be
preempted mid-execution — the deadline applies at the entry check and at the next async
suspension point. An in-flight synchronous SQLite query is covered by a **pre-check only** (if
the budget is already exhausted when the query starts, it returns `deadline-exceeded` immediately;
a running synchronous query cannot be interrupted).

### Deadline-aware I/O

When a deadline is active, key I/O operations respect the budget automatically:

| Operation | Behavior |
|---|---|
| `std/net/http` client requests | Effective total timeout = `min(requested, remaining)`; already-expired → immediate `deadline-exceeded` pair before any connect |
| `std/postgres` / `std/redis` async ops | Pre-check + await raced against remaining budget → `deadline-exceeded` pair on expiry |
| `std/sqlite` synchronous queries | Pre-check only (synchronous — cannot be preempted mid-query) |
| `lim.acquire` / `bh.run` parking | Park raced against remaining budget → `deadline-exceeded` on expiry |

Programs that never call `resilience.deadline` take the `nil` fast path everywhere — zero cost.

---

## Trace / request ID

### `resilience.withTrace(id, fn) -> fn's result`

Scope a trace/request ID for `fn` and everything it calls. Sets the ambient `traceId` for log
records and telemetry spans within the scope (save → set → restore on all exits, including panic).

### `resilience.traceId() -> string | nil`

Return the current ambient trace ID, or `nil` if none is set.

The gateway example's middleware idiom:

```ascript
app.use((req, next) => {
  let id = req.headers["x-request-id"] ?? "fixed-trace"
  return resilience.withTrace(id, () => next(req))
})
```

When `std/log` is active, log records gain a `traceId` field automatically when one is set. When
`std/telemetry` is initialized, the ambient ID is attached as a span attribute.

---

## HTTP helpers

### `resilience.metricsHandler() -> handler`

Returns a callable handler that mounts directly on a server route:

```ascript
app.route("GET", "/metrics", resilience.metricsHandler())
```

Renders the per-isolate metrics registry as **Prometheus text exposition format 0.0.4**
(`# TYPE` lines, deterministic label order, proper escaping). Response: status 200,
`content-type: text/plain; version=0.0.4`.

### `resilience.health(opts) -> handler`

Health/readiness handler. Liveness (no checks) always returns 200. Readiness runs each check
function and reports `200 {"status":"ok","checks":{…}}` if all pass, or
`503 {"status":"degraded","checks":{…}}` with per-check `{ok, error?}` detail.

| Option | Description |
|---|---|
| `checks` | `object` mapping check names to 0-arg callables; each runs under `timeoutMs` |
| `timeoutMs` | Per-check timeout; checks that don't settle in time are treated as failed |

A check passes on a truthy return or ok-pair; fails on falsy, err pair, panic, or timeout. Each
check is contained — one bad check never 500s the endpoint. Checks run in registration order.

```ascript
app.route("GET", "/healthz", resilience.health({}))
app.route("GET", "/readyz", resilience.health({ checks: { db: pingDb }, timeoutMs: 1000 }))
```

### `resilience.handler(policies, fn) -> handler`

Wrap a route handler with policies, mapping rejection codes to HTTP statuses:

```ascript
app.route("GET", "/quote", resilience.handler({
  limiter:    perClient,
  key:        (req) => req.headers["x-api-key"] ?? "anon",
  bulkhead:   bh,
  breaker:    b,
  deadlineMs: 500,
}, handleQuote))
```

Fixed, documented execution order (outermost first): **keyed/plain limiter → bulkhead → breaker
→ deadline → fn** — shed the cheapest checks first, give the breaker visibility into real
attempts only, set the budget innermost so it covers exactly the handler.

| Code | HTTP status |
|---|---|
| `"rate-limited"` | 429 + `retry-after` header (seconds until next token refills) |
| `"bulkhead-full"` | 503 |
| `"breaker-open"` | 503 + `retry-after` header (seconds until cooldown expires) |
| `"deadline-exceeded"` | 504 |

Any other `[v, err]` from `fn` passes through to the server's existing semantics. Retry is
deliberately **not** in the wrapper — server-side self-retry is an anti-pattern; compose
explicitly if needed. All keys are optional.

---

## Per-isolate honesty

### The model, stated without hedging

Every policy's state — breaker windows, token buckets, bulkhead permits, singleflight table,
memoize entries, the metrics registry, task-local deadlines and trace IDs — is **per-isolate**.
Under `server.serve({ workers: N })` there are N independent copies of everything.

This is usually **correct**: each Envoy sidecar / each service replica runs its own breaker;
per-replica limiting is how local rate limiting deploys in practice (see
[Workers & parallelism](../language/workers) for the isolation model). It is the only model
consistent with shared-nothing isolation.

The `__local` Native marker makes the boundary **loud**: shipping a policy to a `worker fn` or
`worker class` raises the existing non-sendable field-path panic instead of silently forking its
state into a divergent twin. Task-local deadlines and trace IDs likewise do not cross the airlock
— a `worker fn` body starts with empty locals. When a budget must cross, pass an explicit
deadline-ms argument.

### Global state via a `worker class` actor

When state genuinely must be process-global (a strict global rate limit, a cluster-wide breaker),
the documented pattern is a dedicated actor — one isolate **owns** the policy, everyone else asks
it over a FIFO mailbox:

```ascript
import * as resilience from "std/resilience"

worker class GlobalLimiter {
  lim: any? = nil
  fn init() { self.lim = resilience.limiter({ capacity: 1000, refillPerSec: 500 }) }
  async fn tryAcquire(): bool { return self.lim.tryAcquire() }
}

let gl = await GlobalLimiter.spawn()
print(await gl.tryAcquire())   // true
print(await gl.tryAcquire())   // true
gl.close()
```

**Honest trade-off:** every check is a mailbox round-trip — use the actor pattern for low-QPS
global decisions (e.g. a process-wide strict cap). For the hot path per-isolate policies are the
right tool: they run synchronously within the isolate with no cross-thread communication.

See [Workers & parallelism](../language/workers) for the full actor (`worker class`) model.

---

## Metrics reference

`std/resilience` maintains a minimal per-isolate registry (always on, no `telemetry.init`
required). Policies additionally mirror their signals through the SP12 telemetry soft hook when
`std/telemetry` is initialized, and emit `log.debug` transition breadcrumbs when `std/log` is
active.

### Metric set

All metric names carry the `ascript_resilience_` prefix. Counters are monotonically increasing;
gauges reflect current state.

| Metric | Type | Labels | Description |
|---|---|---|---|
| `breaker_state` | gauge | `name` | 0 = closed, 1 = open, 2 = halfOpen |
| `breaker_calls_total` | counter | `name`, `result` (success\|failure\|rejected) | Calls through the breaker |
| `breaker_transitions_total` | counter | `name`, `to` | State-machine edge count |
| `limiter_acquired_total` | counter | `name` | Successful token acquisitions |
| `limiter_rejected_total` | counter | `name` | Failed `tryAcquire` calls |
| `limiter_evictions_total` | counter | `name` | Keyed-limiter LRU bucket evictions |
| `bulkhead_in_flight` | gauge | `name` | Current concurrent executions |
| `bulkhead_shed_total` | counter | `name` | Calls shed (queue full) |
| `retry_attempts_total` | counter | `name`, `outcome` | Retry engine attempt outcomes |
| `retry_budget_exhausted_total` | counter | `name` | Times a retry policy budget blocked a retry |
| `singleflight_joins_total` | counter | — | Concurrent same-key joins (de-duped calls) |
| `memoize_hits_total` | counter | `name` | Cache hits |
| `memoize_misses_total` | counter | `name` | Cache misses (including expired entries) |
| `deadline_exceeded_total` | counter | — | Deadline-exceeded outcomes across all sites |

### Per-isolate scrape caveat

Under `server.serve({ workers: N })` with `SO_REUSEPORT`, a Prometheus scrape hits **one
isolate** per request (kernel-balanced). Each isolate exports only its own registry — the scrape
sees a fraction of the total traffic. Options: (a) accept per-isolate metrics + scrape-level
aggregation in your query; (b) expose a per-isolate `/metrics/N` path; (c) use the
`worker class` actor pattern to funnel counter updates into a single registry isolate. Histograms
(request latency) belong to `std/telemetry` — the resilience registry tracks counts and gauges only.

---

## Determinism

All policy verdicts use deterministic inputs:

- Breaker window is count-based (no clock reads per call).
- Limiter refill and retry-after use the SP9 monotonic clock seam (`clock_monotonic_ms`).
- Memoize TTL uses the same monotonic seam.
- Deadline set/remaining/expiry verdicts use the monotonic seam.
- Retry/backoff sleep **durations** follow the shipped SP9 timing-only exemption (perturb wall
  pacing, never observable values — the same exemption `task.retry` carries).
- Nothing in RESIL draws observable randomness.

Under SP9 Record/Replay every `deadlineRemaining()` call and fail-fast verdict is event-sourced.
Programs that never call `resilience.deadline` take the `nil` fast path everywhere — byte-identical
to the pre-RESIL path across all four engines (tree-walker, specialized VM, generic VM, `.aso`).

---

## Examples

- `examples/resilience.as` — intro: breaker trip/recover, token-bucket limiter, keyed limiter,
  bulkhead, retry v2, fallback, singleflight, memoize, deadline. Byte-identical across all four
  engines.
- `examples/advanced/resilient_gateway.as` — production-shaped: trace middleware, keyed limiter,
  `resilience.handler` route wrapping, breaker + retry + deadline composition, `/metrics` +
  `/readyz` mounted, the global-state actor pattern, fully error-handled.

Run with:

```bash
ascript run examples/resilience.as
ascript run examples/advanced/resilient_gateway.as
```
