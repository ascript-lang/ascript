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
  across `yield`s (streams). Torn down on `close()` or when the last handle is dropped.

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
// "cannot be sent to a worker at field path [.cb]: function values are not sendable"
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
  print(math.sum(results))         // 204
}

await main()
```

> Note the division of labor: `array.map`/`task.gather`/`math.sum` run on the **caller**
> thread; the `worker fn` body itself uses only pure arithmetic. Worker bodies cannot call
> imported stdlib modules — see [Worker-body limitations](#worker-body-limitations) below.

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
  (shipped transitively); top-level `const` bindings with literal initializers (copied by value).
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
- **`close()` / last-drop teardown.** Call `close()` to shut the actor down explicitly; dropping
  the last handle also tears down the isolate. No zombie threads are left behind.

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

A worker body runs in a fresh isolate that receives only a **code slice** — the top-level `fn`
and literal-`const` definitions it transitively references. Three things this does *not* include:

1. **No stdlib module imports inside the worker body.** The code slice does not ship `import`
   bindings. Calling `math.sum(...)`, `array.sort(...)`, or `json.parse(...)` *inside* a worker
   body fails at runtime with `undefined variable`. Only bare builtins (`len`, `print`, `assert`,
   `Ok`, `Err`, …) and your own top-level helper `fn`s are available. The idiom: do the
   stdlib-dependent work on the caller side, pass only data into the worker, and write any
   in-worker computation as a pure top-level `fn`.

   ```ascript
   import * as math from "std/math"
   import * as array from "std/array"
   import * as task from "std/task"

   // Pure arithmetic helper — no stdlib imports; shipped transitively.
   fn lcg(state: number): number {
     return (state * 1103515245 + 12345) % 2147483648
   }

   worker fn countHits(seed: number): number {
     let state = seed
     let hits = 0
     let i = 0
     while (i < 1000) {
       state = lcg(state)              // ✓ top-level fn, shipped transitively
       let x = state / 2147483648.0
       state = lcg(state)
       let y = state / 2147483648.0
       if (x * x + y * y <= 1.0) { hits = hits + 1 }
       i = i + 1
     }
     return hits
   }

   fn main() {
     let futures = array.map([1, 2, 3, 4, 5, 6, 7, 8], countHits)
     let hitCounts = await task.gather(futures)
     print(math.sum(hitCounts))        // stdlib work on the caller side
   }

   await main()
   ```

2. **Computed-initializer consts and `class`/`enum` deps are not shipped.** Only top-level
   `const` bindings whose initializer is a literal (number, string, bool, nil, or a literal
   array/object of those) are copied into the isolate. A `const` initialized from a function
   call, and any `class`/`enum` definition a worker body references directly, fails loudly at
   runtime with `undefined variable`. Reconstruct class instances on the caller side.

3. **Handles are not sendable.** Generator handles and actor proxy handles cannot be passed
   across the boundary. Pass plain data (snapshots) instead.

---

See also: [Modules & async](modules-async) for the single-isolate `async`/`await`/generator
model and [`std/task`](../stdlib/async) for `gather`/`race`/`pipe`.
