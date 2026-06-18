# Workers & parallelism

AScript is **single-threaded per isolate** — within any one runtime there is exactly one
thread, so there are no data races to reason about (see [Modules & async](modules-async)).
Multi-core parallelism is achieved by **isolation**, not shared memory: a `worker` runs in a
separate **shared-nothing isolate** — a complete, independent AScript runtime on another OS
thread that shares no memory with the caller. Only **data** crosses the boundary, deep-copied
through a serializer airlock; live references never do.

There are three worker forms, all introduced by the `worker` keyword:

- `worker fn` / `static worker fn` — **stateless** work dispatched to a pool (returns `future<T>`).
- `worker class` — a **stateful actor** in its own dedicated isolate (an async proxy handle).
- `worker fn*` — a **streaming generator** running its producer body in a dedicated isolate.

## The model: two lifecycles

Workers come in two lifecycle shapes that differ in where the isolate lives and how long it
lasts.

- **Pooled / stateless** (`worker fn`, `static worker fn`). Each call grabs an isolate from a
  shared, lazily-grown **pool**, runs the body once, and returns it. No state survives between
  calls. Ideal for embarrassingly-parallel `map`/`gather` workloads.
- **Dedicated isolate** (`worker class`, `worker fn*`). A long-lived isolate is created for the
  lifetime of one actor handle or one generator. State persists across messages (actors) or
  across `yield`s (streams). A **stream** is torn down on `close()` or when its generator handle is
  dropped. An **actor** is reclaimed on `close()` or at program exit — like other native resources
  (sockets, DB connections), the handle lives in the runtime's resource table, so call `close()`
  when you're done with it rather than relying on the value going out of scope.

### The sendability line

Whatever lifecycle you use, the same rule governs what may cross the boundary. Values are
**structured-cloned** (deep-copied) when they cross — the same deep-copy rules as
JSON-serializable data, extended to cover AScript's `array`, `object`, `map`, `set`, `bytes`,
`number`, `string`, `bool`, `nil`, and class instances.

**Not sendable:** function/closure values, native resource handles (open files, sockets, DB
connections), generator handles, and actor proxy handles. Attempting to send one produces a
**recoverable panic whose message names the exact field path** of the offending value:

```ascript
worker fn takesObj(o): number { return 1 }

// A closure cannot cross the isolate boundary:
let [_, err] = recover(() => await takesObj({ cb: () => 1 }))
print(err.message)
// "value of kind function cannot be sent to a worker at .cb"
```

## `worker fn` — pooled, stateless

Calling a `worker fn` returns a `future<T>` — just like an `async fn` — so you `await` the
result the same way. `task.gather` preserves input order, so the output is deterministic
regardless of which isolate finishes first.

```ascript
import * as task from "std/task"
import * as array from "std/array"
import * as math from "std/math"

worker fn square(n: number): number {
  return n * n
}

fn main() {
  let inputs = [1, 2, 3, 4, 5, 6, 7, 8]
  let futures = array.map(inputs, square)
  let results = await task.gather(futures)
  print(results)                   // [1, 4, 9, 16, 25, 36, 49, 64]
  print(math.sum(results))         // 204.0
}

await main()
```

> Note the division of labor: `array.map`/`task.gather`/`math.sum` run on the **caller**
> thread. A `worker fn` body **may** also call imported stdlib modules directly (e.g.
> `math.max(...)`) — the code slice ships the top-level imports into the isolate. See
> [Worker-body limitations](#worker-body-limitations) below for what is and isn't shipped.

### `static worker fn`

A class method can also be a worker. The method body is shipped as a standalone function and
the class name is preserved across the boundary for reconstruction.

```ascript
import * as task from "std/task"
import * as array from "std/array"

class Img {
  static worker fn encode(px: number): number {
    return px * 2 + 1
  }
}

let futures = array.map([10, 20, 30], Img.encode)
print(await task.gather(futures))   // [21, 41, 61]
```

### Cost model

> Parallelize **coarse, CPU-bound work** — not tight inner loops.

The serialization round-trip costs roughly 0.2–1.3 ms per call depending on payload size
(measured on Apple M4, 10 logical cores). For fine-grained work this overhead dominates. For
coarse work the pool delivers real speedups: on the same machine, 8 workers processing 32
CPU-bound chunks yields ~5× wall-clock speedup (2 182 ms → 439 ms).

The pool is **lazy and demand-grown**, bounded to `num_cpus` threads (override with the
`ASCRIPT_WORKERS` environment variable). It is created on first use and threads are reused
across calls; FIFO backpressure prevents unbounded queuing. Pool warmup adds ~80 ms on the
first call; steady-state per-call overhead is ~60–250 ms depending on payload size.

### Capture & sendability rules

Worker functions run in an isolated scope, so the compiler enforces these capture rules:

- **Allowed:** function parameters; other top-level `worker fn` and regular `fn` definitions
  (shipped transitively); top-level `const`/`enum` bindings (literal consts copied by value,
  computed consts re-run on the isolate); top-level `class` definitions a worker constructs or
  returns; and top-level `import`s (shipped wholesale, re-run on the isolate).
- **Not allowed:** capturing a mutable outer `let`, or reading or writing a top-level mutable
  global. Violations are `worker-capture` **compile errors** — caught before the program runs.

```ascript
const FACTOR = 10           // ✓ top-level const — copied into the isolate

worker fn scale(n: number): number {
  return n * FACTOR         // ✓ fine
}
```

### Inline nesting

A `worker fn` called from *inside* another worker isolate runs **inline** — no re-dispatch, no
extra thread, no deadlock:

```ascript
worker fn inner(n: number): number { return n + 1 }
worker fn outer(n: number): number {
  let bumped = await inner(n)   // inline when already in an isolate
  return bumped * 10
}

print(await outer(4))   // 50
```

### Error handling

A worker panic (e.g. a failing `assert`) is recovered on the calling side as a `[nil, err]`
pair, just like any other recoverable panic. Use `assert(false, msg)` to raise a named error
inside a worker body — there is no `panic` builtin.

```ascript
worker fn risky(n: number): number {
  if (n < 0) { assert(false, "negative input") }
  return n * n
}

let [_, err] = recover(() => await risky(-1))
print(err.message)   // "negative input"
```

> **Syntax note:** `recover()` requires the **arrow** form for its argument —
> `recover(() => ...)`, *not* `recover(fn() { ... })`. (The anonymous-`fn`-expression-as-call-arg
> form trips a known parser limitation; the arrow form is the supported idiom.)

## `worker class` — stateful actors

A `worker class` is an **actor**: an instance that lives in its own dedicated isolate, owns its
mutable state, and processes messages one at a time. You don't construct it locally — you
`spawn` it, which returns a `future<handle>`:

```ascript
worker class Counter {
  n: number = 0
  cache: any? = nil
  fn init() {
    self.cache = {}
  }
  fn inc(): number {
    self.n = self.n + 1
    return self.n
  }
  fn remember(k: string, v: number): number {
    self.cache[k] = v
    return self.cache[k]
  }
  fn lookup(k: string): any {
    return self.cache[k]
  }
}

async fn main() {
  let c = await Counter.spawn()
  print(await c.inc())               // 1
  print(await c.inc())               // 2
  print(await c.remember("x", 42))   // 42
  print(await c.lookup("x"))         // 42
  print(await c.inc())               // 3 — state persisted across all calls
  c.close()
}

await main()
```

The semantics that make actors safe:

- **Proxy handle.** `spawn()` returns a `Value::Native` proxy, not the instance. The real
  instance lives entirely inside the isolate.
- **`spawn` vs local construction.** `Counter.spawn()` creates the actor in its own isolate.
  Calling the constructor locally (`Counter()`) would build an ordinary in-isolate instance, not
  an actor — actors are always `spawn`ed.
- **Methods are async-only.** Every method call is a message; calling `c.inc()` returns a
  `future<T>` you `await`. There is no synchronous method dispatch across the boundary.
- **No field access across the boundary.** You cannot read or write `c.n` from the caller — only
  call methods. State is touched solely by the actor's own code, inside the isolate.
- **FIFO, one message at a time, non-reentrant.** Messages queue in arrival order and run to
  completion one at a time. An actor never processes a second message while one is in flight,
  so its state mutations are never interleaved — the classic actor invariant.
- **Owns in-isolate resources.** A resource (a file, a connection, an in-memory store) opened in
  `init` or a method lives and dies inside the isolate; it never crosses the boundary. Only data
  returned from methods crosses.
- **Methods may use other top-level classes/enums.** An actor method can construct or reference
  any other top-level `class` (its full method table + superclass chain) or `enum` — the actor
  code slice ships those definitions into the isolate (transitively: a shipped class whose own
  method constructs yet another class pulls that one in too), exactly like a `worker fn` body.
- **`close()` / program-exit teardown.** Call `close()` to shut the actor down explicitly and
  reclaim its isolate thread. Like other native resources (sockets, DB connections), an actor whose
  handle simply goes out of scope is *not* eagerly reclaimed — it lives in the runtime resource
  table until `close()` or program exit. Always `close()` long-lived actors. No zombie threads are
  left behind (every isolate is joined on `close()` or at exit).

Actors survive panics. A method that asserts produces a recoverable `[nil, err]` on the caller;
the actor keeps running and answers subsequent messages correctly:

```ascript
worker class Service {
  store: any? = nil
  total_sum: number = 0
  fn init() { self.store = {} }
  fn put(k: string, v: number): number {
    assert(len(k) > 0, "key must be non-empty")
    self.store[k] = v
    self.total_sum = self.total_sum + v
    return v
  }
  fn total(): number { return self.total_sum }
}

async fn main() {
  let s = await Service.spawn()
  print(await s.put("a", 10))                  // 10
  print(await s.put("b", 32))                  // 32
  print(await s.total())                       // 42

  let caught = recover(() => await s.put("", 99))
  if (caught[1] != nil) {
    print("caught panic: " + caught[1].message) // caught panic: key must be non-empty
  }
  print(await s.total() == 42)                  // true — actor survived the panic
  s.close()
}

await main()
```

> A class field **requires a type annotation** (`n: number = 0`, `cache: any? = nil`); a bare
> `n = 0` is not a valid field declaration.

## `worker fn*` — streaming generators

A `worker fn*` runs its producer body in a **dedicated isolate** and streams values back,
consumed transparently via `for await (x in gen)`. Each yielded value crosses the boundary via
structured clone:

```ascript
worker fn* records(n: number) {
  let i = 1
  while (i <= n) {
    yield {id: i, label: `rec-${i}`}
    i = i + 1
  }
}

async fn main() {
  for await (r in records(4)) {
    print(`${r.id}:${r.label}`)   // 1:rec-1, 2:rec-2, 3:rec-3, 4:rec-4
  }
}

await main()
```

Streaming semantics:

- **Demand-driven pull.** The producer advances only when the consumer asks for the next value.
  Nothing is computed ahead of demand.
- **Bounded buffer / backpressure.** A small bounded buffer sits between producer and consumer;
  when it fills the producer parks until the consumer drains it. Backpressure threads all the way
  back across the isolate boundary.
- **Bidirectional `next(v)`.** `gen.next(v)` injects a value back into the producer — it becomes
  the result of the suspended `yield` expression — enabling request/response steering.
- **Close / drop.** `gen.close()` (or dropping the handle) stops the producer and tears down the
  isolate.

Bidirectional steering — the value passed to `next(v)` is what the matching `yield` evaluates to
inside the producer:

```ascript
worker fn* accumulate() {
  let total = 0
  let a = yield "start"
  total = total + a
  let b = yield `got: ${a}`
  total = total + b
  yield `got: ${b}`
  yield `total: ${total}`
}

async fn main() {
  let g = accumulate()
  print(await g.next())     // "start"   (first next() has no input)
  print(await g.next(5))    // "got: 5"  (a = 5)
  print(await g.next(12))   // "got: 12" (b = 12)
  print(await g.next())     // "total: 17"
  g.close()
}

await main()
```

> **Yield chunks, not elements.** Each `yield` pays one serialization round-trip across the
> boundary. For high-volume streams, yield *batches* (arrays of records) rather than one element
> at a time, so the per-crossing overhead is amortized.

> `worker async fn` is **rejected** — workers are already awaitable, so the `async` modifier is
> redundant. `worker fn*` (a streaming generator), however, **is** valid.

## Workers and the event bus

The two worker forms layer cleanly with [`std/events`](../stdlib/utilities):

- **Intra-isolate** events (`std/events`) stay within one runtime — listeners and emitters share
  memory and fire synchronously. Use them for in-process pub/sub.
- **Inter-isolate** data flow is the worker boundary itself — only deep-copied data crosses.

`task.pipe(gen, bus)` is the **bridge** between the two: it consumes a worker generator stream
and re-emits each yielded item onto a local events bus, routing by the item's `kind` field. Many
listeners can fan out from one worker stream, with backpressure threading back to the producer.

```ascript
import { pipe } from "std/task"
import * as events from "std/events"

worker fn* source(n: number) {
  let i = 1
  while (i <= n) {
    yield {kind: "item", value: i}
    i = i + 1
  }
  yield {kind: "end", count: n}
}

async fn main() {
  let bus = events.new()

  bus.on("item", (e) => { print(`listenerA saw: ${e.value}`) })
  bus.on("item", (e) => { print(`listenerB saw: ${e.value}`) })
  bus.on("end",  (e) => { print(`done: received ${e.count} items`) })

  await pipe(source(3), bus)   // resolves after all events are delivered
}

await main()
```

A note on what may NOT cross: an actor method that directly returns a `worker fn*` generator is
not supported — generator handles and actor proxy handles are not sendable. The idiom is to have
the actor return a plain sendable **snapshot** (e.g. an array), then feed that snapshot to a
separate `worker fn*` for streaming.

## Worker-body limitations

A worker body runs in a fresh isolate that receives only a **code slice** — the transitive
top-level dependency closure of the worker entry. The closure ships, automatically:

- **top-level `fn`s** it (transitively) calls;
- **top-level `import`s** (shipped wholesale, so `math.max(...)`, `array.sort(...)`,
  `json.parse(...)` work *inside* the worker body — std imports are side-effect-free, a file
  import re-runs its module on the isolate);
- **`enum`s and literal `const`s** (copied by value);
- **computed-initializer `const`s** (`const K = expensive()`) — the initializer is re-run on the
  isolate, so the worker sees the recomputed value;
- **`class`es** a worker constructs or returns (the full class + its superclass chain), so a
  worker fn can `return Point(3, 4)` and the instance round-trips back via structured clone.

```ascript
import * as math from "std/math"
import * as task from "std/task"

class Stats {
  n: number
  mean: number
  fn init(n, mean) { self.n = n; self.mean = mean }
}

worker fn summarize(xs: array<number>): Stats {
  let total = math.sum(xs)            // ✓ imported stdlib module — shipped
  return Stats(len(xs), total / len(xs))  // ✓ class constructed in the isolate
}

fn main() {
  let s = await summarize([2, 4, 6, 8])
  print(s.mean)                       // 5.0
}

await main()
```

What is still **not** shipped:

1. **Mutable shared state.** A worker body cannot capture an outer mutable `let` or read/write a
   mutable top-level global — those are `worker-capture` compile errors. Workers are
   shared-nothing: pass data in, return data out.

2. **Non-top-level deps.** A `class`/`fn` nested inside another function whose members capture an
   enclosing local cannot be shipped. Keep worker-referenced classes and helpers at the top
   level.

3. **Handles are not sendable.** Generator handles and actor proxy handles cannot be passed
   across the boundary. Pass plain data (snapshots) instead. (Returned class instances carry
   their fields, but not their methods — reconstruct behavior on the caller side if needed.)

---

## Multi-core servers & the shared heap

The HTTP server (`std/http/server`) can spread its accept loop across **N isolates** that
each bind the same port via `SO_REUSEPORT`, so the kernel load-balances incoming connections
across cores. This is the **server tier**: the nginx / Envoy / Node-`cluster` worker model,
applied to AScript.

```ascript
import * as server from "std/http/server"
import * as shared from "std/shared"
import * as postgres from "std/postgres"

// Build the big read-only state ONCE, on the main isolate, then freeze it.
let routes = shared.freeze(loadRouteTable())     // immutable, Send, zero-copy

// The per-isolate setup runs IN each isolate at boot: open this isolate's OWN
// connection pool, register handlers. `routes` crosses as an Arc pointer bump.
worker fn boot(routes) {
  let app = server.create()
  let db = postgres.connect(env.get("DATABASE_URL"))   // per-isolate, never crosses
  app.route("GET", "/users/:id", worker fn (req) {
    let route = routes[req.path]    // zero-copy read of the shared table
    return db.query("select ...", [req.params.id])
  })
  return app                        // this isolate's OWN server handle
}

// Spread the accept loop across N isolates, each binding the same port.
await server.serve({ port: 8080, workers: 0, setup: boot, args: [routes] })
//                                       ^ 0 = num_cpus
```

How it works:

- **`workers` absent or `1`** → today's single-isolate accept loop, unchanged.
- **`workers: N` (N>1, or `0` = `num_cpus`)** → spawn N shared-nothing isolates. Each runs
  `setup(...args)` at boot to build its **own** server handle and open its **own** per-isolate
  resources (a DB pool, prepared statements — these never cross the airlock), then accepts on
  its own `SO_REUSEPORT` socket. A connection that lands on isolate *k* is accepted, dispatched,
  and answered entirely on isolate *k*'s core — **no cross-isolate hop per request**.
- **`setup`** is a `worker fn`; its `args` are sendable (typically the frozen
  [`shared`](../stdlib/shared) state). Handlers are `worker fn`s too.
- **`maxRequests`** across N isolates bounds the **total** number of connections served (a
  shared budget + coordinated stop); the per-isolate split is OS scheduling and is not
  asserted.

### The shared config pattern

Per-isolate `setup` opens this isolate's *mutable* resources (a DB pool). But large
**read-only** state — a routing table, a feature-flag snapshot, a geo-IP database — should
**not** be re-opened or re-copied per isolate. Build it once on the main isolate,
[`shared.freeze`](../stdlib/shared) it, and pass the `Shared` as a `setup` argument: it crosses
to each isolate as one `Arc` pointer bump, read zero-copy, never duplicated. This is what makes
multi-core serving practical — see [Shared read-only heap](../stdlib/shared).

### The Windows caveat

`SO_REUSEPORT` (kernel connection load-balancing) is available on **Linux, macOS, and the
BSDs**. On **Windows** there is no equivalent, so `workers: N > 1` transparently **falls back to
a single isolate** and emits a one-time `warn`: *"workers: N requested but SO_REUSEPORT is
unavailable on this platform; serving single-isolate."* This is honest degradation — correct,
just single-core on Windows — never a silent drop.

---

## Data parallelism: `task.pmap`

The hand-rolled pattern for parallel map is:

```ascript
import * as task from "std/task"
import * as array from "std/array"

worker fn score(row) { return row.weight * row.hits }

// Hand-rolled: one pool round-trip per element (~0.23 ms each — swamps small per-element work).
let futures = array.map(rows, score)
let results = await task.gather(futures)
```

`task.pmap` replaces that with a single call that **chunks** the input across the pool (one
round-trip per chunk, not per element) and merges results **in input order**:

```ascript
import * as task from "std/task"
import * as shared from "std/shared"

worker fn score(row) { return row.weight * row.hits }
worker fn add(a, b)  { return a + b }

// Freeze large read-only input once — crosses each chunk dispatch as one Arc pointer bump.
let rows  = shared.freeze(loadRows())
let out   = await task.pmap(rows, score)           // one line; input order guaranteed
let total = await task.preduce(out, add, 0)        // parallel fold with associative combiner
```

The upgrade is one line; the result is byte-identical to the sequential version for an associative
combiner. For large datasets the `shared.freeze` step (see [Shared read-only heap](../stdlib/shared))
eliminates per-chunk deep copies — the frozen array crosses each dispatch as a single `Arc` pointer
bump at flat cost regardless of size.

See the full [`std/task` reference](../stdlib/async#data-parallelism----taskpmap--taskpreduce) for
the chunk-plan formula, the `preduce` associativity contract, the frozen-vs-plain input table,
error/cancel semantics, and break-even guidance.

---

See also: [Modules & async](modules-async) for the single-isolate `async`/`await`/generator
model, [`std/task`](../stdlib/async) for `gather`/`race`/`pipe`, [Shared read-only
heap](../stdlib/shared) for `shared.freeze`, and [Resilience policies](../stdlib/resilience) for
per-isolate circuit breakers, rate limiters, and the `worker class` actor pattern for
process-global policy state (§7.2 `GlobalLimiter`).
