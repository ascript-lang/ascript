# Async, Concurrency & Generators

AScript runs on a **single-threaded cooperative event loop** (a current-thread Tokio
`LocalSet`). There is no parallelism and no shared-memory data races, so concurrent code
needs no locks — values are plain `Rc`/`RefCell` under the hood. Concurrency comes from
*interleaving at `await` points*, and lazy iteration comes from *generators*.

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

Side effects of an un-awaited future still complete: the top-level program drains all
spawned tasks before it exits (structured shutdown).

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
| `spawn(futureOr0ArgFn) -> future` | Schedule a task and get its `future`. Accepts a future or a 0-arg function. |
| `gather([futures]) -> [values]` | Run all concurrently; return results **in input order**. The first error short-circuits. |
| `race([futures]) -> value` | Resolve to the first to finish; the losers are dropped. |
| `timeout(ms, future) -> [value, err]` | Result pair: `[value, nil]` if it finishes in time, else `[nil, err]`. |

```ascript
let [x, y, z] = await task.gather([compute(), compute(), compute()])
let first      = await task.race([slow(), fast()])
let [val, err] = await task.timeout(500, slow())
if (err != nil) { print("timed out") }
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

- **Single-threaded:** no data races; shared mutable state needs no locks. CPU-bound work in a
  handler blocks the loop — offload heavy work, keep handlers I/O-bound.
- **HTTP server** handles each connection on its own task with a bounded concurrency cap, so a
  slow handler does not block other clients.
- **Not provided** (deliberate architectural non-goals — see the design spec §7): durable /
  serializable continuations, robust unbounded deep recursion, and deterministic / replayable
  scheduling.
