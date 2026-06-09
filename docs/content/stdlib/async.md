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
