:::eyebrow Language

# Modules & async

## Modules

One file is one module. Use `export` to expose bindings and `import` to pull them in. There are no
default exports.

```ascript
// util.as
export const PI = 3.14159
export fn double(x) { return x * 2 }
fn secret() { return 99 }            // not exported — private to this module
```

```ascript
// main.as
import { PI, double } from "./util"     // named import
import * as util from "./util"          // namespace import

print(double(21))      // 42
print(util.PI)         // 3.14159
```

- **Relative paths** (`"./util"`, `"../lib/helpers"`) resolve against the importing file's directory.
  The `.as` extension is implied.
- **Standard-library paths** (`"std/json"`, `"std/net/http"`) resolve to built-in modules.
- **Package specifiers** (bare names like `"mylib"` or scoped names like `"@org/mylib"`) resolve
  through the package manager. See [Packages](../packages) for how to add and install dependencies.
- Importing a name a module does not export is an error.

```ascript
import { get, post } from "std/net/http"
import * as json from "std/json"
```

Each module is evaluated **once** and cached. A circular import resolves to the partially-initialized
module — using a binding before it has been initialized is a load-order error.

### The always-global core

A handful of builtins need no import and are available everywhere: `print`, `len`, `type`, `assert`,
`range`, `Ok`, `Err`, `recover`, `exit`. Everything else lives in a `std/*` module.

## Async

AScript supports `async fn` and `await` on a **single-threaded event loop** — a single-threaded Tokio
runtime that *is* the loop. There is no second thread, so there are no data races to reason about.

```ascript
async fn fetchUser(id: number): Result<object> {
  let [resp, err] = await get(`https://api.example.com/users/${id}`)
  if (err != nil) { return Err(err.message) }
  return await resp.json()
}

let [user, err] = await fetchUser(42)
```

- `await expr` suspends until `expr` resolves. You can `await` any value — `await 5` is just `5`.
- Async standard-library functions (timers, sockets, HTTP, WebSockets, subprocess I/O) return
  awaitables driven by the runtime.
- Purely synchronous programs never touch the executor and pay no async cost.
- Async composes with [results](errors): `await someCall()?` awaits, then propagates on error.

### Async model — `future<T>` and cancel-on-drop

Calling an `async fn` returns a **`future<T>`** value and **eagerly schedules** the body as a task
on the event loop. The body starts running concurrently with the caller; `await` suspends the caller
until the task completes.

`future<T>` is a **first-class value** — you can store it in a variable, pass it to functions, and
annotate bindings with it as a [contract type](type-contracts):

```ascript
async fn compute(): number { return 42 }

let f: future<number> = compute()   // task is already running
let result = await f                 // 42
```

**Cancel-on-drop:** dropping the last handle to a `future<T>` **cancels the underlying task**. An
un-awaited, un-held async call is therefore cancelled, not orphaned. Use `task.spawn` from `std/task`
to explicitly detach a task so it keeps running after the handle is dropped.

### Top-level await

The top level of a program may use `await` directly, or you can define and await a `main`:

```ascript
import { listen } from "std/net/tcp"

async fn main() {
  let [server, err] = await listen("127.0.0.1", 8080)
  // …
}

await main()
```

### Structured concurrency — `std/task`

`std/task` provides primitives for running async work concurrently:

```ascript
import * as task from "std/task"
import * as time from "std/time"

async fn work(ms, value) {
  await time.sleep(ms)
  return value
}

// gather: run several futures in parallel, collect results in input order
let results = await task.gather([work(40, "a"), work(10, "b"), work(20, "c")])
// results == ["a", "b", "c"]

// race: return the first future to finish, cancel the others
let winner = await task.race([work(60, "slow"), work(5, "fast")])
// winner == "fast"

// timeout: fail if the future does not complete in time
let [val, err] = await task.timeout(100, work(10, "done"))
// val == "done", err == nil

// spawn: detach a task (fire-and-forget); the returned handle can be awaited later
let handle = task.spawn(work(5, "bg"))
let bg = await handle   // "bg"
```

- `task.gather(futures)` — awaits all and returns an array of results in input order.
- `task.race(futures)` — returns the first to finish; cancels the rest.
- `task.timeout(ms, future)` — returns `[value, nil]` on time, or `[nil, err]` past the deadline;
  the timed-out work is cancelled.
- `task.spawn(future)` — detaches the task; returns an awaitable handle. The task keeps running
  even if the handle is dropped.

See [Async & concurrency](../stdlib/async) for the full API reference.

## Generators

A **generator function** is declared with `fn*` (or `async fn*` for async generators). Calling it
does **not** run the body — it returns a lazy **generator** value driven by the caller:

```ascript
fn* count(n) {
  let i = 1
  while (i <= n) {
    yield i
    i = i + 1
  }
}

for await (x in count(3)) {
  print(x)   // 1, then 2, then 3
}
```

Generators are **consumer-driven** (lazily polled) — the body runs only when the caller pulls the
next value. They are **not** spawned tasks.

### Driving a generator manually

`gen.next(value?)` advances the body to the next `yield` and returns its yielded value (or `nil`
when the generator is exhausted). You can send a value back into the body: `yield` evaluates to the
argument of the resuming `next` call.

```ascript
fn* echo() {
  let a = yield "ready"    // a receives the argument of the second next()
  let b = yield "more"
}

let g = echo()
print(g.next())          // "ready"   — starts the body
print(g.next("one"))     // "more"    — resumes; "one" becomes a
g.next("two")            // exhausted; "two" becomes b
```

`gen.close()` stops the generator; any subsequent `next()` returns `nil`.

### `for await` — the primary consumption form

`for await (x in gen)` drives the generator with successive `next()` calls and binds each yielded
value to `x`. `break` exits early (the remaining body is abandoned). It works on both sync and async
generators:

```ascript
for await (x in count(5)) {
  if (x > 3) { break }   // pulls only 1, 2, 3
}
```

### Async generators

`async fn*` may both `await` and `yield`. Compose them via `for await`:

```ascript
import * as time from "std/time"

async fn* ticks() {
  yield 1
  await time.sleep(1)
  yield 2
  yield 3
}

async fn* doubled(src) {
  for await (n in src) {
    yield n * 2
  }
}

for await (v in doubled(ticks())) {
  print(v)   // 2, 4, 6
}
```

### `type()` of a generator

`type(gen)` returns `"generator"`. The `fn*` / `async fn*` declaration itself is a `"function"`.

## Workers

A **worker function** runs in a **shared-nothing isolate** on a background thread, enabling
multi-core parallelism without shared memory or data races. Calling a `worker fn` returns a
`future<T>` — just like an `async fn` — so you `await` the result the same way.

```ascript
import * as task from "std/task"
import * as array from "std/array"

worker fn square(n: number): number {
  return n * n
}

fn main() {
  let inputs = [1, 2, 3, 4, 5, 6, 7, 8]
  let futures = array.map(inputs, square)
  let results = await task.gather(futures)
  print(results)          // [1, 4, 9, 16, 25, 36, 49, 64]
}

await main()
```

### `static worker fn`

A class method can also be a worker:

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

### The shared-nothing model

Each worker invocation runs in an **isolate**: a fresh AScript interpreter on a pooled OS thread,
sharing no memory with the caller. All arguments and return values are **structured-cloned** across
the boundary — the same deep-copy rules as JSON-serializable data, extended to cover AScript's
`array`, `object`, `map`, `set`, `bytes`, `number`, `string`, `bool`, `nil`, and class instances.

Values that cannot cross the boundary produce a clear runtime error with the exact field path of
the offending value:

```ascript
worker fn takesObj(o): number { return 1 }

// A closure cannot cross the isolate boundary:
let [_, err] = recover(() => await takesObj({ cb: () => 1 }))
print(err.message)
// "cannot be sent to a worker at field path [.cb]: function values are not sendable"
```

### Capture rules

Worker functions run in an isolated scope, so the compiler enforces these capture rules:

- **Allowed:** function parameters; other top-level `worker fn` and regular `fn` definitions
  (shipped transitively); top-level `const` bindings with literal initializers (copied by value).
- **Not allowed:** capturing a mutable outer `let`, or reading or writing a top-level mutable
  global. Violations are `worker-capture` **compile errors** — they are caught before the program runs.

```ascript
const FACTOR = 10           // ✓ top-level const — copied into the isolate

worker fn scale(n: number): number {
  return n * FACTOR         // ✓ fine
}
```

### Error handling

A worker panic (e.g. a failing `assert`) is recovered on the calling side as a `[nil, err]` pair,
just like any other recoverable panic:

```ascript
worker fn risky(n: number): number {
  if (n < 0) { assert(false, "negative input") }
  return n * n
}

let [_, err] = recover(() => await risky(-1))
print(err.message)   // "negative input"
```

Note: use `assert(false, msg)` to raise a named error inside a worker body. The `recover()`
built-in requires arrow syntax for its argument — `recover(() => ...)` — not `recover(fn() {...})`.

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

### The worker pool

Workers are executed on a **lazy, demand-grown pool** bounded to `num_cpus` threads (override with
the `ASCRIPT_WORKERS` environment variable). The pool is created on first use and threads are
reused across calls; FIFO backpressure prevents unbounded queuing. Pool warmup adds ~80 ms on the
first call; steady-state per-call overhead is ~60–250 ms depending on payload size.

### Cost model

> Parallelize **coarse, CPU-bound work** — not tight inner loops.

The serialization round-trip costs roughly 0.2–1.3 ms per call depending on payload size
(measured on Apple M4, 10 logical cores). For fine-grained work this overhead dominates. For
coarse work the pool delivers real speedups: on the same machine, 8 workers processing 32
CPU-bound chunks yields ~5× wall-clock speedup (2 182 ms → 439 ms).

### Known limitations

**1. No stdlib module imports inside the worker body.**
The code slice shipped to the isolate includes top-level `fn` and `const` definitions; it does
*not* include `import` bindings. Calling `math.sum(...)`, `array.sort(...)`, or any other stdlib
function *inside* a worker body will fail at runtime with `undefined variable`.

The idiom: do the stdlib-dependent work on the caller side and pass only data into the worker. If
you need a helper computation, write it as a plain top-level `fn` (no imports) and call that:

```ascript
import * as math from "std/math"
import * as array from "std/array"
import * as task from "std/task"

// Pure arithmetic helper — no stdlib imports needed
fn lcg(state: number): number {
  return (state * 1103515245 + 12345) % 2147483648
}

worker fn countHits(seed: number): number {
  let state = seed
  let hits = 0
  let i = 0
  while (i < 1000) {
    state = lcg(state)          // ✓ top-level fn, shipped transitively
    let x = state / 2147483648.0
    state = lcg(state)
    let y = state / 2147483648.0
    if (x * x + y * y <= 1.0) { hits = hits + 1 }
    i = i + 1
  }
  return hits
}

fn main() {
  let seeds = [1, 2, 3, 4, 5, 6, 7, 8]
  let futures = array.map(seeds, countHits)
  let hitCounts = await task.gather(futures)
  // stdlib work on the caller side:
  print(math.sum(hitCounts))
}

await main()
```

**2. Computed-initializer consts and `class`/`enum` definitions are not shipped.**
Only top-level `const` bindings whose initializer is a literal (number, string, bool, nil, or a
literal array/object of those) are copied into the isolate. A `const` initialized from a
function call, and any `class` or `enum` definition that the worker body references directly,
will fail at runtime with `undefined variable`. Keep worker bodies purely arithmetic and
data-transforming; reconstruct class instances on the caller side.
