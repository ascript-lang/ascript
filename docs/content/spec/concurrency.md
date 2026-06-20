# Concurrency: async, generators & workers

This chapter specifies AScript's three concurrency facilities: **async tasks**
(`async fn` / `await`), **generators** (`fn*` / `async fn*`), and **workers**
(shared-nothing parallelism). The syntactic forms are in the
[grammar chapter](grammar); this chapter gives their runtime meaning. The error
model they ride is in the [errors chapter](errors).

Two background facts from the [notation chapter](intro) govern the whole chapter:
the **interleaving of concurrently scheduled tasks** and the **OS scheduling of
worker isolates** are **unspecified** — a conforming program does not depend on a
particular ordering between independent tasks or isolates.

## Tasks & eager scheduling

Calling an `async fn` returns a **future** (annotated `future<T>`) and schedules
the body **immediately**. The future is a handle to in-flight work, not a deferred
thunk: the body begins running as soon as the call evaluates, and continues
concurrently with the caller.

`await` **drives** a future to completion and yields its result. `await` on a
**non-future** is the **identity**: `await 5` is `5`. This makes `await` safe to
write uniformly over a value that may or may not be a future.

```as
async fn fetch(x) { return x * 2 }
let r = await fetch(21)     // 42
print(await 5)              // 5 — identity on a non-future
```

Arrow functions may be `async` too (`async (n) => n + 1`, `async x => x - 1`),
producing futures identically.

The runtime is **single-threaded per isolate**: tasks run cooperatively on one OS
thread, suspending at `await` points. There is no preemption and no data race
within an isolate. Parallelism across cores is provided by **workers** (below),
not by async tasks.

## Structured concurrency — cancel-on-drop

A task's lifetime is **bound to its future handle**. When the **last handle** to a
future is dropped, the task is **cancelled** (aborted at its next suspension
point). Consequently:

- An **un-awaited, un-held** async call does **not** run to completion — it is
  cancelled when its future is dropped, not orphaned to run in the background.
- `race([…])` returns the first future to finish and **cancels the losers**.
- `timeout(ms, fut)` cancels the awaited work if it does not finish in time,
  returning a Tier-1 `[nil, err]` past the deadline.
- At program exit the runtime **drains** all still-owned tasks.

```as
let dropLog = []
async fn orphan() {
  await time.sleep(10)
  array.push(dropLog, "ran")
}
orphan()                    // future dropped immediately -> cancelled
await time.sleep(60)
print(len(dropLog))         // 0 — the body never ran to completion
```

The **explicit detach** is `task.spawn(fut)`: it takes ownership of the task so it
keeps running even though the caller does not hold the original future, and it
returns a handle that MAY be awaited later for the result.

```as
task.spawn(background())            // detached: runs to completion
let h = task.spawn(work(5, "x"))    // detached AND tracked
print(await h)                      // x
```

> Cleanup that **must** survive cancellation belongs on a resource's deterministic
> `Drop`, not in code after an `await` that may be cancelled — a cancelled task's
> remaining body, including a bare `defer`, does not run (see the
> [errors chapter](errors) and the `defer` cancellation rule).

## `std/task` combinators

The `std/task` module exposes the structured-concurrency contracts:

- `spawn(fut) -> future<T>` — detach as above.
- `gather([futs]) -> future<array>` — await all; the result array is in **input
  order** regardless of completion order; the first error (by input order)
  propagates.
- `race([futs]) -> future<T>` — the first to finish wins; losers are cancelled.
- `timeout(ms, fut) -> future<Result<T>>` — `[value, nil]` if it finishes in time,
  `[nil, err]` (and the work cancelled) past the deadline.
- `retry(fn, opts) -> future<T>` — re-invoke a fallible async producer per a retry
  policy.

## Generators

`fn*` and `async fn*` declare **generators**. Calling one returns a **generator
handle** without running the body. A generator is **consumer-driven**: the body is
a lazily-polled coroutine advanced one step at a time, **not** a spawned task.

- `gen.next(v)` resumes the body until the next `yield`, returning the yielded
  value; the argument `v` becomes the **result of the `yield` expression** the body
  is parked on (bidirectional flow). The first `next()` starts the body; its
  argument is ignored.
- A `yield` **parks** the body, surfacing its operand to the consumer.
- After the body returns, `next()` yields `nil` and keeps yielding `nil`.
- `gen.close()` drops the body, releasing its in-progress resources.
- `for await (x in gen) { … }` iterates a generator (and any async iterable),
  driving it to exhaustion.

```as
fn* echo() {
  let a = yield "q1"
  let b = yield "q2"
}
let g = echo()
print(g.next())      // q1   (starts the body)
print(g.next("a"))   // q2   ("a" becomes the value of the first yield)
g.next("b")          // "b" becomes the value of the second yield; body returns
```

An `async fn*` MAY `await` between yields; `for await` drives an async generator
the same way, suspending at the awaits inside it.

## Workers — parallelism by isolation

Workers provide **multi-core parallelism** by **isolation**: each worker runs a
**complete, independent runtime** (its own `Interp`, heap, and event loop) on its
**own OS thread**, sharing **no memory** with any other isolate. There are no data
races because there is nothing shared to race on. The `worker` keyword fronts three
forms over two lifecycles.

### Pooled stateless — `worker fn`

`worker fn f(…)` (and `static worker fn`) declares a function each call of which
runs **once** on a lazily-grown, demand-driven worker **pool** (bounded by the host
parallelism, configurable via `$ASCRIPT_WORKERS`). A call returns `future<T>`; the
arguments are sent into a pooled isolate, the body runs there, and the result is
sent back.

```as
worker fn square(n: number): number { return n * n }
let results = await task.gather([square(2), square(3), square(4)])  // [4, 9, 16]
```

### Dedicated actor — `worker class`

`worker class C { … }` declares an **actor**. `C.spawn(…)` (note: **`spawn()`**,
not local construction) creates a **dedicated isolate** holding one long-lived
instance and returns `future<handle>`. Calls on the handle are messages:

- Methods are **async-only**: each call sends a message and returns `future<T>`.
- The mailbox is **FIFO, one message at a time** — messages are processed in send
  order, never concurrently within the actor.
- The actor is **non-reentrant**: a method may not re-enter the actor while a
  message is in flight.
- There is **no cross-boundary field access** — state is reached only through
  method messages.

The actor and its in-isolate resources are torn down on `close()` or last-drop; no
zombie threads remain.

### Dedicated stream — `worker fn*`

`worker fn* g(…)` declares a **streaming generator** running in a dedicated
isolate. Consumption is **demand-driven pull** with a **bounded buffer**, giving
backpressure across the boundary; `gen.next(v)` is bidirectional as for local
generators. The stream is torn down on `close()` / last-drop.

## The sendability rules (the airlock)

Values cross a worker boundary only through the **serializer airlock**, by
**structured deep copy** of bytes — the runtime itself stays per-isolate and is
never shared. A value is either **sendable** or it is not:

**Sendable** (deep-copied across the airlock):
`nil`, `bool`, `int`, `float`, `decimal`, `string`, `bytes`, `array`, `object`,
`map`, `set`, `regex`, enum variants (including constructed payloads), and class
**instances** (the class *code* is shipped with the value). A **frozen `shared`
value** is also sendable and crosses **by reference** (an `Arc` bump), not by copy
— it is the one `Send`-carrying value (below).

**Non-sendable** (a **recoverable field-path panic** at the boundary):
closures and plain functions, native resource handles (files, sockets, FFI
handles, …), futures, generator handles, actor handles, and interface values.
Attempting to send one raises:

```
value of kind <kind> cannot be sent to a worker at <path>
```

where `<path>` is the dotted/indexed selector locating the offending value (e.g.
`.a.b` for a closure nested two objects deep). This is a **recoverable** Tier-2
panic — a host may `recover` it — never a silent drop or a corrupted copy.

```as
worker fn run(o) { return 1 }
let [v, e] = recover(() => run({ a: { b: () => 5 } }))
print(e.message)   // value of kind function cannot be sent to a worker at .a.b
```

## Frozen shared values

`shared.freeze(v)` deep-converts a value into an **immutable, reference-counted**
shared value — AScript's **only** `Send`-carrying value. The freeze walk is
**acyclic by construction**: an on-stack cycle is **rejected**, while a diamond
(shared sub-structure reached two ways) is **preserved** as one shared node.

A frozen value **reads exactly like its underlying kind** — a frozen array indexes
and iterates as an array, a frozen object reads members as an object — with
zero-copy iteration. **Any mutation** is a panic reusing the underlying-kind
message:

```as
let f = shared.freeze([1, 2, 3])
let [v, e] = recover(() => { f[0] = 9 })
print(e.message)   // cannot mutate a frozen array
```

Because a frozen value is `Send`, it crosses the worker airlock **by reference**
(an `Arc` bump), making it the mechanism for sharing large read-only data across
isolates without per-message copies (cross-link the shared-heap reference,
[stdlib/shared](../stdlib/shared)).

## Determinism

Task **interleaving** and worker **isolate scheduling** are **unspecified** (per
the [notation chapter](intro)): a program MUST NOT depend on which of two
independent tasks runs first, nor on which isolate handles a given pooled call.
Three deeper guarantees are explicit **non-goals** of the async engine and are
**not** provided: durable/serializable task continuations, robust unbounded deep
recursion across `await`, and deterministic/replayable task scheduling. Programs
that need replayable execution use the durable-workflow facility, which records an
event log rather than relying on scheduler determinism.

## Conformance

The concurrency model in this chapter is exercised by:

- `examples/async.as` — `async fn`, `await`, the non-future identity, and async
  arrows.
- `examples/structured_concurrency.as` — `gather` input-order, `race`/`timeout`
  loser cancellation, `task.spawn` detach, and cancel-on-drop of an un-awaited
  call.
- `examples/generators.as` + `examples/generators_test.as` — `fn*`/`async fn*`,
  bidirectional `next(v)`, `for await`, and post-exhaustion `nil`.
- `examples/workers_parallel_map.as`, `examples/workers_errors.as` — pooled
  `worker fn` calls and worker error propagation.
- `examples/advanced/workers_actor_counter.as` — a `worker class` actor with FIFO
  message processing.
- `examples/advanced/workers_stream_bidirectional.as` — a `worker fn*` stream with
  bidirectional pull.
- `examples/shared_config.as` — `shared.freeze`, frozen-mutation rejection, and a
  frozen value crossing the airlock.
- `tests/m17_structured_concurrency.rs`, `tests/m17_generator_regressions.rs` — the
  task/generator differential batteries (tree-walker == VM).
- `tests/workers_stateful.rs` — actor + streaming worker contracts and the
  sendability airlock.

Run the examples with `target/release/ascript run examples/async.as` (and
likewise); each matches its recorded golden. The non-sendable boundary is
reproduced directly: sending a closure nested in an object into a `worker fn`
prints `value of kind function cannot be sent to a worker at .a.b`, and mutating a
`shared.freeze`d array prints `cannot mutate a frozen array`.
