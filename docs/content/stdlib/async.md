# Async, Concurrency & Generators

AScript runs on a **single-threaded cooperative event loop per isolate** (a current-thread Tokio
`LocalSet`). Within an isolate there is no shared-memory parallelism and no data races, so
concurrent code needs no locks — values are plain `Rc`/`RefCell` under the hood. Concurrency comes
from *interleaving at `await` points*, and lazy iteration comes from *generators*. For multi-core
parallelism, run work in separate shared-nothing isolates — see
[Workers & parallelism](../language/workers).

## async / await

Calling an `async fn` returns a **`future<T>`** immediately and schedules the body to run.
`await` drives a future to completion and yields its value. `await` on a non-future is the
identity (so `await 5` is `5`).

```ascript
async fn fetchUser(id) {
  let resp = await http.get(`https://api.example.com/users/${id}`)?
  return json.parse(resp.body)
}

let user = await fetchUser(1)
```

Because calling an `async fn` schedules eagerly, two calls made before either is awaited run
**concurrently**:

```ascript
let a = fetchUser(1)     // starts running
let b = fetchUser(2)     // starts running too
let ua = await a         // total wait ≈ max(a, b), not a + b
let ub = await b
```

## Structured concurrency: cancel-on-drop

A task's lifetime is **bound to its future handle**. When the last reference to a future is
dropped, the task is **cancelled** — work without an owner does not linger.

```ascript
async fn log(msg) { await db.write(msg) }

log("hello")             // future created then immediately dropped -> CANCELLED, never runs
let f = log("kept")      // held -> runs; `await f` would also keep it alive
```

If you want fire-and-forget work that *must* run even though you do not keep the handle, use
`task.spawn` (below) — it is the explicit opt-out of cancel-on-drop. This makes memory
bounded by construction: a server loop that fires un-awaited async calls cannot pile up
orphaned tasks.

## The `future<T>` type

`future<T>` is a first-class type usable in contracts. It describes the *unawaited handle*;
the `async fn`'s own return annotation describes the **resolved** type:

```ascript
async fn compute(): number { return 41 }   // resolves to a number
let pending: future<number> = compute()     // the handle
let value: number = await pending            // 41
```

## `std/task` — combinators over futures

```ascript
import * as task from "std/task"
```

| Function | Behavior |
|---|---|
| `spawn(futureOr0ArgFn) -> future` | **Detach** a task (opt out of cancel-on-drop) so it runs to completion even if the handle is dropped. Accepts a future or a 0-arg function. |
| `gather([futures]) -> [values]` | Run all concurrently; return results **in input order**. The first error short-circuits. |
| `race([futures]) -> value` | Resolve to the first to finish; **the losers are cancelled**. |
| `timeout(ms, future) -> [value, err]` | Result pair: `[value, nil]` if it finishes in time, else `[nil, err]`. On timeout the **timed-out work is cancelled**. |
| `retry(fn, opts?) -> value` | Call `fn()` up to `opts.attempts` times (default `3`). Retries only on a **Tier-2 panic** — a returned `[nil, err]` pair is passed through immediately without retry. Uses exponential back-off; re-raises the last panic when all attempts are exhausted. |

```ascript
let [x, y, z] = await task.gather([compute(), compute(), compute()])
let first      = await task.race([slow(), fast()])    // slow() is cancelled when fast() wins
let [val, err] = await task.timeout(500, slow())      // slow() is cancelled if it overruns
if (err != nil) { print("timed out") }

task.spawn(log("audit"))   // fire-and-forget: runs to completion despite the dropped handle
```

### `task.retry`

```ascript
let result = await task.retry(fn, {
  attempts: 5,     // max calls (default 3, must be a positive integer)
  baseMs:   100,   // first backoff window in ms (default 100)
  maxMs:    2000,  // optional cap on any single backoff delay
  jitter:   true,  // add up to +50 % random jitter to each delay
})
```

`retry` retries on **panics only** (Tier-2). A returned `[nil, err]` Result pair (Tier-1) is
returned immediately — it is the caller's job to distinguish retriable from non-retriable
failures. The back-off follows `baseMs * 2^attemptIndex`, capped by `maxMs` if supplied.

```ascript
import * as task from "std/task"

let tries = 0
async fn flaky() {
  tries = tries + 1
  if (tries < 3) { assert(false, "not yet") }
  return "ok"
}

let result = await task.retry(flaky, {attempts: 5, baseMs: 1})
print(result)   // "ok"  — succeeded on attempt 3
```

See also: [`std/resilience`](resilience) ships `resilience.retry` — a richer policy object
(`retryOn`, `retryIf`, `budget`, stats/reset) that composes with breakers and deadlines. Use
`task.retry` for simple panic-only retry; use `resilience.retry` when you need Tier-1 error retry,
`retryIf` guards, or explicit policy composition.

### `task.pipe`

```ascript
await task.pipe(gen, bus)
```

Bridges a **worker generator stream** onto a local `std/events` bus. Drives the generator
`gen` one step at a time and fans each yielded event object onto `bus` by its `kind` field.
Returns when the generator is exhausted. Use this to connect a `worker fn*` streaming generator
(which runs on a separate isolate) to local event-based consumers without manually driving the
pull loop. See [Workers & parallelism](../language/workers) for the full inter-isolate streaming
pattern.

## Data parallelism — `task.pmap` / `task.preduce`

```ascript
import * as task from "std/task"
import * as shared from "std/shared"

worker fn score(row) { return row.weight * row.hits }
worker fn add(a, b)  { return a + b }

let rows  = shared.freeze(loadRows())              // happy path: frozen = zero-copy hand-off
let out   = await task.pmap(rows, score)           // future<array>, results in INPUT order
let total = await task.preduce(out, add, 0)        // future<T>
```

`task.pmap` and `task.preduce` distribute an array across the existing worker pool, running the
callback inside isolates **one chunk per dispatch** (amortising the per-round-trip overhead over
many elements) and merging results **in input order**.

| Function | Signature | Behavior |
|---|---|---|
| `pmap` | `pmap(data, f, opts?) -> future<array>` | Apply `f` to every element of `data`; return results **in input order**, never completion order. |
| `preduce` | `preduce(data, f, init, opts?) -> future<T>` | Parallel reduction. Each chunk is folded with `f` seeded by the chunk's first element; partials are combined with one final fold `f(…f(f(init, p0), p1)…)`. `init` participates **exactly once**. |

Both return an ordinary `future<…>` and compose with `await`, `task.timeout`, `task.race`,
`task.spawn`, and structured cancel-on-drop.

**Empty input:** `pmap([])` resolves to `[]`; `preduce([], f, init)` resolves to `init` — in
both cases without touching the pool.

**The callback `f`** must be a **named, top-level `worker fn`** — the same rule as
`run_in_worker`. An arrow, anonymous fn, plain (non-`worker`) fn, or builtin is a Tier-2 panic:

```
task.pmap expects a named `worker fn` as its callback (got function)
```

**Options** (both functions):

```ascript
{ chunks?: int >= 1,    // number of chunks to dispatch (default: pool size = num_cpus / $ASCRIPT_WORKERS)
  minChunk?: int >= 1 } // minimum elements per chunk (default: 1)
```

### Chunk plan formula

Chunk boundaries are **deterministic** given `(len, cap, minChunk)` and are a **documented part
of the contract** (makes `preduce` reproducible across runs):

```
cap        = opts.chunks   if given (int ≥ 1)
             else pool cap                      // $ASCRIPT_WORKERS if set, else num_cpus
chunk_size = max(opts.minChunk ?? 1, ceil(len / cap))
chunks     = ceil(len / chunk_size)
chunk i    = [i * chunk_size, min((i+1) * chunk_size, len))    // i = 0 .. chunks-1
```

The default chunk count equals the pool size (≈ core count) so total dispatch overhead is bounded
at ≈ `cores × 0.23 ms` warm regardless of array length.

### `preduce` contract

> **`preduce` contract.** `f` must be **associative** for `preduce(data, f, init)` to equal
> the sequential `reduce`. Chunk boundaries are deterministic given the input length and the
> chunk count (the published formula above), so even a non-associative `f` is
> **reproducible** — byte-identical across runs and across all engine modes on the same
> machine/configuration — it is just not equal to the sequential fold. The default chunk
> count is the machine's worker-pool size; pass `{chunks: N}` to pin results across machines.

### Frozen vs plain input

The input form determines crossing semantics. You choose explicitly:

| Input | How it crosses | Semantics inside `f` | Performance |
|---|---|---|---|
| `shared.freeze(arr)` — `Value::Shared` array | One `Arc` pointer bump per chunk dispatch (O(1), size-independent, ~0.15 ms flat) | Read-only frozen view. Elements are zero-copy. Mutation is a panic: `cannot mutate a frozen <kind>`. | Best for large, read-only element data. |
| Plain `Value::Array` | Per-chunk structured-clone of the element slice (total cost = one full copy of the input, same class as one freeze walk) | Each chunk isolate owns a **mutable private copy**. Element-local mutation is legal. | Fine for small arrays or when elements are mutated per-call. |
| Frozen instance element (from a frozen array) | Arc bump | Fields readable zero-copy. Methods are **not** available: `method '<name>' is not available on a frozen instance …` | Use plain array if the callback calls instance methods. |
| Plain instance element (from a plain array) | Structured clone (fields only — a documented Spec A limitation of the worker airlock) | Fields readable. Methods are **not** available across the boundary: `value is not callable`. | Fields-only access on either path; methods require the caller side. |
| Cyclic plain array | Works — `TAG_REF` airlock copy preserves cycles | Per-chunk mutable copy | — |
| Attempt to `shared.freeze` a cyclic value | `shared.freeze` rejects cycles at the freeze call | — | Freeze before passing to `pmap`. |
| Non-array or `Shared` non-array | Tier-2 panic: `task.pmap expects an array or a frozen array (got <kind>)` | — | — |

**Guidance:** for large read-only element data, `shared.freeze` first (pays ~0.52 ms / 10k entries
once, then flat per dispatch). For small arrays or elements that `f` mutates, pass the plain array.

### Error and cancellation semantics

- **Callback panic:** the orchestrator awaits chunks in **input order**, so the reported panic
  is always the first failing chunk **by input order**, never completion order. On the first panic
  all remaining chunk futures are dropped.
- **Dropped chunk futures:** a chunk still queued on an isolate is cancelled before it runs. A
  chunk **already executing** runs to completion — it is CPU-bound and never yields; its reply is
  discarded. This is today's pooled-worker cancellation semantics, inherited unchanged.
- **`?` inside `f`:** a `?`-propagation inside the callback body is converted to `Ok([nil, err])`
  by the isolate before `call_value` returns, so that element's result in the `pmap` output is the
  `[nil, err]` pair — identical to a direct `worker fn` call. The docs recommend returning
  `[value, err]` pairs as data when per-element fallibility matters; they merge in order like any
  value.
- **Timeout / race / detach:** `task.timeout(ms, task.pmap(...))` drops the pmap future on
  timeout → cancel-on-drop aborts the orchestrator → chunk futures drop → the cancellation
  semantics above. `task.spawn(task.pmap(...))` detaches.
- **Pool exhaustion:** graceful degradation — a chunk that cannot be dispatched to the pool runs
  inline on the caller; the result is identical.

### Capabilities

`pmap`/`preduce` run under the **pooled** `worker fn` capability model: each chunk inherits the
dispatching isolate's `CapSet` as a read-only floor, and `caps.drop` is **refused** inside the
callback (pooled-worker rule). `pmap` takes no `caps` option and does not create a sandbox. For a
cap-reduced parallel job, use `run_in_worker(f, input, {caps})` per item — see
[`std/caps`](caps).

### Break-even and performance guidance

`pmap` pays a fixed overhead of approximately `chunks × 0.23 ms` warm dispatch cost (from the
worker pool bench) plus the input copy/freeze cost, regardless of array length. For small or
trivial per-element work this overhead dominates and a sequential `for` loop wins. The measured
break-even (per-element duration below which sequential is faster) is published in
`bench/DATA_PARALLEL_RESULTS.md`. On an Apple M4 with W=4 and 32 chunks, pmap wins
starting somewhere between 0 and ~1 000 tight-loop iterations per element (≈20 µs per
element total sequential time); at 10 000 iterations it is already 3.4× faster than
sequential. Rule of thumb: if each element takes at least ~1 ms of CPU work, pmap pays
off at W≥2.

```ascript
import * as task from "std/task"
import * as shared from "std/shared"

worker fn score(row) { return row.weight * row.hits }
worker fn add(a, b)  { return a + b }

// Freeze once, reuse across multiple pmap calls.
let dataset = shared.freeze(loadDataset())

// Parallel map — results in input order.
let scores = await task.pmap(dataset, score)

// Parallel reduction — associative combiner equals sequential reduce.
let total = await task.preduce(scores, add, 0)

// Pin chunks for cross-machine reproducibility with a non-associative combiner.
let pinned = await task.preduce(scores, add, 0, { chunks: 4 })

// Compose with timeout — cancels the whole pipeline if it overruns.
let [result, err] = await task.timeout(5000, task.pmap(dataset, score))
```

## `std/sync` — channels, semaphores, and rate limiters

`std/sync` provides primitives for coordinating between concurrent tasks. No feature gate —
always available, even in `--no-default-features` builds.

```ascript
import * as sync from "std/sync"
```

### Channels

A **channel** is a FIFO queue that lets one task send values and another receive them.

```ascript
let ch = sync.channel()      // unbounded (send never blocks)
let ch = sync.channel(10)    // bounded — send awaits when the queue has 10 items
```

| Function | Behavior |
|---|---|
| `channel(capacity?) -> ch` | Create a channel. Omit `capacity` (or pass `0`) for unbounded. |
| `send(ch, value) -> [ok, err]` | **async.** Push a value. On a bounded channel, awaits until space is available. Returns `[true, nil]` on success, `[false, err]` if the channel is closed. |
| `recv(ch) -> value \| nil` | **async.** Pop the next value. Awaits if the queue is empty. Returns `nil` when the channel is closed **and** fully drained. |
| `tryRecv(ch) -> [value, ok]` | **Sync, non-blocking.** Returns `[value, true]` if a value was available, `[nil, false]` otherwise. Cannot distinguish an empty-open channel from a closed-drained one. |
| `close(ch) -> nil` | Close the sending side. Parked `recv` callers are woken and will drain remaining values before seeing `nil`. |

```ascript
import * as sync from "std/sync"
import * as task from "std/task"

let ch = sync.channel()

async fn producer() {
  let i = 1
  while (i <= 3) {
    await sync.send(ch, i)
    i = i + 1
  }
  sync.close(ch)
}

task.spawn(producer())

let v = await sync.recv(ch)
while (v != nil) {
  print(v)           // 1, 2, 3
  v = await sync.recv(ch)
}
```

### Semaphores

A **semaphore** is a counting permit pool for limiting concurrency.

```ascript
let s = sync.semaphore(3)   // 3 concurrent slots
```

| Function | Behavior |
|---|---|
| `semaphore(n) -> s` | Create a semaphore with `n` permits (`n` must be a positive integer). |
| `acquire(s) -> nil` | **async.** Wait until a permit is available, then take one. |
| `release(s) -> nil` | Return one permit. Extra releases beyond the initial count are no-ops — the pool never inflates past its declared size. |
| `withPermit(s, fn) -> value` | **async.** `acquire` → `await fn()` → `release` on **all** paths (including panics). Returns `fn`'s result. |
| `available(s) -> number` | Current free permit count. |

```ascript
let s = sync.semaphore(2)   // at most 2 concurrent workers

async fn worker(id) {
  return await sync.withPermit(s, async () => {
    await time.sleep(10)
    return id
  })
}

let results = await task.gather([worker(1), worker(2), worker(3), worker(4)])
// workers 1 & 2 run together, then 3 & 4 — results in input order
```

### Rate limiters

A **rate limiter** is a token-bucket that controls how often work can proceed.

```ascript
let lim = sync.rateLimiter({perSecond: 10})
// or: sync.rateLimiter({count: 5, windowMs: 200})
```

| Option | Meaning |
|---|---|
| `{perSecond: N}` | `N` tokens per second (sugar for `{count: N, windowMs: 1000}`). |
| `{count: N, windowMs: M}` | `N` tokens per `M`-millisecond window. |

The returned handle exposes one async method:

- `limiter.acquire()` — **async.** Wait until a token is available, then consume one.

```ascript
import * as sync from "std/sync"

let lim = sync.rateLimiter({perSecond: 5})

let i = 0
while (i < 5) {
  await lim.acquire()
  print("tick " + i)
  i = i + 1
}
```

## `std/time` timer utilities

In addition to `now`, `monotonic`, `sleep`, and unit helpers, `std/time` provides three
timer primitives.

```ascript
import * as time from "std/time"
```

### `time.interval`

```ascript
let iv = time.interval(ms)
await iv.tick()
```

Creates a repeating timer with the given period in milliseconds. Each call to `iv.tick()`
**async**-waits until the next tick fires. Tokio fires the first tick immediately (period 0);
subsequent ticks fire at `period * n`. Drop the handle to stop the timer.

```ascript
let iv = time.interval(100)   // fires every 100 ms
let i = 0
while (i < 5) {
  await iv.tick()
  print("tick " + i)
  i = i + 1
}
```

### `time.debounce`

```ascript
let wrapped = time.debounce(fn, ms)
wrapped(arg1, arg2, ...)
```

Returns a **callable wrapper** that implements trailing-edge debouncing. Each call resets the
timer: any previously-pending delayed call is **cancelled**, and a new fire-and-forget task is
scheduled to call `fn(args)` after `ms` milliseconds. Only the most-recent call in a burst
survives.

> **Note:** `debounce` works with synchronous wrapper functions. The wrapped `fn` is called
> from a detached task after the window expires, so its return value is discarded.

```ascript
let debouncedSave = time.debounce(() => {
  print("saved!")
}, 300)

// Burst of calls — only the last one triggers the save.
debouncedSave()
debouncedSave()
debouncedSave()
await time.sleep(400)   // "saved!" printed once
```

### `time.throttle`

```ascript
let wrapped = time.throttle(fn, ms)
wrapped(arg1, arg2, ...)
```

Returns a **callable wrapper** that implements leading-edge throttling. The first call in any
`ms`-millisecond window invokes `fn(args)` immediately; subsequent calls within the same window
are **silently dropped**. The next call after the window expires starts a new window.

```ascript
let throttledLog = time.throttle((msg) => {
  print(msg)
}, 200)

// Burst — only the first fires.
throttledLog("a")   // prints "a"
throttledLog("b")   // dropped
throttledLog("c")   // dropped
```

## Generators (`fn*`) and coroutines

A generator function (`fn*`) returns a **generator** object. Calling it does *not* run the
body — the body advances only when the consumer pulls a value. Generators are
*consumer-driven*: an abandoned generator is simply dropped (no leaked task).

```ascript
fn* count(n) {
  let i = 1
  while (i <= n) {
    yield i
    i = i + 1
  }
}

for await (x in count(3)) { print(x) }   // 1, 2, 3
```

`yield` is **bidirectional** — the value passed to `next(v)` becomes the result of the
`yield` expression, so generators double as coroutines:

```ascript
fn* echo() {
  let a = yield "ready"    // hands "ready" out; resumes with next(v)
  print(a)
}
let g = echo()
print(g.next(nil))   // "ready"  (the first next starts the body; its arg is ignored)
g.next("hello")      // prints "hello"
```

Generator methods:

- `gen.next(v) -> value` — resume, returning the next yielded value or `nil` when done.
- `gen.close() -> nil` — finalize early; the body is dropped and later `next()` returns `nil`.

## Async generators & `for await`

`async fn*` generators may `await` between yields. `for await (x in src)` consumes any async
iterable — a generator **or** a native stream handle (e.g. a WebSocket via `recv`, an SSE
stream via `next`). Generators compose like a Unix pipe:

```ascript
async fn* tokens() { yield "Hello"; yield " world." }
async fn* doubled(src) {
  for await (t in src) { yield t + t }
}

for await (s in doubled(tokens())) { print(s) }
```

This is the shape used to re-stream LLM/SSE tokens through transformations to a client — see
`examples/advanced/stream_pipeline.as` (network-free), plus `examples/generators.as` and
`examples/concurrency.as`.

## Model notes & limits

- **Single-threaded per isolate:** no data races; shared mutable state needs no locks. CPU-bound
  work in a handler blocks the loop — offload heavy work to a [worker](../language/workers) (a
  shared-nothing isolate on another core), keep handlers I/O-bound.
- **Cancel-on-drop:** an un-awaited, un-held async call is cancelled; use `task.spawn` for
  fire-and-forget. `race` cancels losers; `timeout` cancels the timed-out work. Memory is
  bounded by construction.
- **HTTP server** handles each connection on its own task with a bounded concurrency cap, so a
  slow handler does not block other clients. For **multi-core** serving, `server.serve({ workers:
  N })` spreads the accept loop across N `SO_REUSEPORT` isolates — see [Multi-core servers & the
  shared heap](../language/workers) and the zero-copy [shared read-only heap](shared).
- **Not provided** (deliberate architectural non-goals — see the design spec §7): durable /
  serializable continuations, robust unbounded deep recursion, and deterministic / replayable
  scheduling.
