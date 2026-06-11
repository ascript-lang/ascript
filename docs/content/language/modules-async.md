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

AScript supports `async fn` and `await` on a **single-threaded event loop per isolate** — a
single-threaded Tokio runtime that *is* the loop. Within an isolate there is no second thread, so
there are no data races to reason about. (Multi-core parallelism comes from running work in
separate shared-nothing isolates — see [Workers & parallelism](workers).)

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

## Parallelism: workers

The `async`/`await` model above is **single-threaded per isolate** — one thread, no data races.
For multi-core parallelism, AScript provides **shared-nothing workers**: `worker fn` (pooled,
stateless), `worker class` actors, and `worker fn*` streaming generators, each running in its own
isolate with only deep-copied data crossing the boundary.

See **[Workers & parallelism](workers)** for the full story — the model, the two lifecycles, the
sendability line, actors, streaming, and the `task.pipe` event-bus bridge.

## Native binaries

`ascript build --native` produces a **self-contained native executable** — the whole runtime plus
your compiled program in one file, with no `ascript` (or any other toolchain) needed on the target
machine:

```bash
ascript build --native app.as -o app   # → a standalone `app` executable
./app arg1 arg2                          # runs your program; args reach env.args()
```

This is **bundling, not ahead-of-time compilation** — the embedded bytecode VM still interprets, so
runtime behavior is byte-for-byte identical to `ascript run app.as`. Mechanically, the output is a
copy of the running runtime with the **verified** `.aso` bytecode (the exact bytes `ascript build`
produces) plus a small trailing footer appended; at startup the binary reads its own footer and runs
the payload through the **same verified load path** as `run app.aso`. A tampered or corrupt embedded
program is rejected with a clean error, never a crash — it crosses the same hardened trust boundary.

A few things to know:

- **Size.** A native binary is **tens of MB** — that is the *runtime*, not your program (your bytecode
  is typically a few KB). Every native binary is roughly the size of `ascript` itself.
- **Host-only in v1.** `--target <triple>` is accepted but **rejected** with a clear error:
  cross-compilation is a staged follow-up. Build on the platform you want to run on.
- **Workers work unchanged.** A `worker fn`/`worker class`/`worker fn*` program bundles and runs with
  full multi-core parallelism — isolates get their code from the embedded payload, no source required.
- **macOS.** Native binaries are **ad-hoc code-signed** so the kernel will exec them (required on
  Apple silicon). Ad-hoc signing establishes integrity, not authenticity — distributing to other users
  without a Gatekeeper prompt still needs Developer-ID signing + notarization, which is out of scope
  for `--native`.
- **Atomic output.** The bundle is assembled on a temporary sibling file (write the stub → make it
  executable → sign → append the payload) and only **renamed onto the output path as the very last
  step**, so a failed build never leaves a partial or unsigned binary in place, and the output appears
  atomically.
- **No double-bundles.** If the `ascript` doing the build is *itself* already a native bundle, the
  existing payload overlay is **stripped first**, so the new binary carries exactly one program and is
  not doubled in size.
