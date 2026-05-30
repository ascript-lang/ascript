# ADR: Async Concurrency + Generators/Coroutines on the Existing Async Engine

- **Status:** Accepted
- **Date:** 2026-05-30
- **Milestone:** M17
- **Spec:** `specs/2026-05-29-ascript-design.md` §7
- **Plan:** `plans/2026-05-30-async-generators-coroutines.md`

---

## Context

AScript's interpreter (`src/interp.rs`) is an `async` tree-walker — `eval_expr`/`exec`
are `#[async_recursion]` — running on a single-threaded Tokio runtime
(`#[tokio::main(flavor = "current_thread")]`). The whole runtime is `Rc`/`RefCell`-based
and therefore `!Send`.

The async *surface* (parse/AST/fmt/grammar for `async fn`/`await`) shipped in M14, but the
*semantics* are inert:

- `await e` is the **identity function** (`ExprKind::Await(inner) => eval_expr(inner)`).
- Calling an `async fn` runs it **inline to completion** — there is no future value.
- There is **no user-level concurrency**: a server handles one request at a time, strictly
  sequentially. There is no way to run two awaits at once, fan out, or stream.
- There are **no generators/coroutines** — no `yield`, no lazy/streaming producers.

The target use cases that motivate fixing this are: serving multiple clients concurrently
from one process, composing AI/SSE token streams, and writing coroutine-style handlers and
pipelines.

## Decision

Adopt **Architecture A**: turn the interpreter's already-async core into a real
cooperative-concurrency engine, **and** expose its existing stackless-coroutine nature as
script-level generators and bidirectional coroutines.

Concretely:

1. Move interpreter state behind interior mutability so multiple `eval` futures can be live
   simultaneously.
2. Make calling an `async fn` return an **eagerly-scheduled `Value::Future`**, make
   `await` actually drive a future (identity on non-futures, for back-compat), and add a
   `std/task` module: `spawn`, `gather`, `race`, `timeout`. Concurrency rides
   `tokio::task::LocalSet` + `spawn_local`, which accept `!Send` futures.
3. Implement `yield` as a real Rust `.await` on an internal single-consumer **rendezvous
   channel**, yielding generators (`fn*`), async generators (`async fn*`), bidirectional
   resume (`gen.next(v)`), and `for await` — all on the same engine.

The runtime joins all spawned tasks before exit (structured concurrency: no detached task
outlives `main`). The single new interpreter-internal invariant is **"never hold a
`RefCell` borrow across an `.await`"**, enforced by clippy `await_holding_refcell_ref`.

### Options considered

| Option | What it is | Tradeoffs |
| --- | --- | --- |
| **A — real futures + LocalSet tasks (chosen)** | Make `await` real; calling an `async fn` returns an eagerly-scheduled future on a `tokio::task::LocalSet` (`spawn_local` accepts `!Send` futures). `yield` = an internal `.await` on a rendezvous. | **+** One engine — reuses the `async` tree-walker we already have. **+** No `unsafe`, no new coroutine crate (rendezvous hand-rolled on `tokio::sync`). **+** `!Send` `Rc`/`RefCell` model preserved as-is. **+** Generators are *free* — they expose the suspension the engine already performs. **−** Async is stackless: deep non-yielding recursion still uses the native stack; no serializable/replayable continuations (see deferrals). |
| **B1 — stackful coroutines (`corosensei`)** | Keep the tree-walker, run each coroutine on its own switchable native stack via an `unsafe` niche crate. | **+** Solves deep recursion (real separate stacks) and is a small surface change to the walker. **−** Adds an `unsafe`, lower-level dependency. **−** Still not serializable/replayable. **−** Two suspension mechanisms (Rust async I/O *and* stackful switch) to reconcile. |
| **B2 — explicit-stack / CPS rewrite** | Rewrite the interpreter as an explicit-stack VM with reified, first-class continuations (continuation-passing style). | **+** Strictly the most powerful: durable/serializable continuations, deterministic/replayable scheduling, no native-stack recursion limit. **−** A *full rewrite* of the evaluator — by far the largest cost/risk. **−** Throws away the working async tree-walker and its I/O integration. |

### Rationale

- **Single engine.** `eval` is *already* `async`; Rust `async`/`.await` *is* a stackless
  coroutine transform. Approach A reuses that one mechanism for both concurrency and
  generators instead of bolting on a second suspension model (B1) or rewriting the
  evaluator (B2).
- **No `unsafe`.** The rendezvous is hand-rolled on `tokio::sync`; no `corosensei`-style
  niche crate. This is the same proven path `genawaiter` and `async-stream` take to build
  coroutines on stable async, and the direction of Rust's own `gen`/`async gen` blocks.
- **`!Send` preserved.** `LocalSet`/`spawn_local` accept `!Send` futures, so the
  `Rc<RefCell<…>>` value model and the current-thread runtime stay exactly as they are — no
  `Arc`/`Mutex` churn, no data races introduced.
- **Generators for free.** A script `yield` is just an internal `.await`; exposing the
  engine's existing suspension point gives generators, bidirectional coroutines, and
  `for await` without new machinery.

## Deferrals (require a different engine)

These are deliberate, documented architectural boundaries of Architecture A — not TODOs.
Each is impossible on a stackless-async tree-walker and is tagged with the engine that
would be required.

1. **Durable / serialize-to-disk continuations** — checkpoint a paused workflow to disk and
   resume it after a process restart. Async suspension state lives in compiler-generated
   Rust stackframes, not a reified serializable object. **Requires an explicit-stack VM with
   reified continuations (B2).**
2. **Robust unbounded recursion over very deep data** — deep *non-yielding* script recursion
   still consumes the native call stack and can overflow it, because stackless async does not
   move recursion off the host stack. **Requires stackful coroutines (B1) or an
   explicit-stack VM (B2).**
3. **Deterministic / replayable task scheduling** — Tokio owns task interleaving, so runs are
   not bit-for-bit reproducible or deterministically replayable. **Requires a custom
   scheduler over an explicit-stack VM (B2).**

## Refinement (post-implementation): structured concurrency / cancel-on-drop

The first implementation spawned each `async fn` body as a detached `spawn_local` task and
reaped them only at the top-level drain. A memory scan found un-awaited async calls in a loop
grew RSS without bound (one live task per call: ~39/69/130 MB at 50k/100k/200k iterations),
and `race`/`timeout` left losing/timed-out work running to program exit. Both are the same
defect: a task could outlive every handle to it.

Fix: a task's lifetime is **bound to its `Value::Future` handle** (cancel-on-drop). The
handle owns the task's `AbortHandle` and aborts on `Drop`; the spawned task holds only the
result cell, never the handle, so there is no `Rc` cycle and last-handle-drop genuinely
cancels. `task.spawn` is the explicit detach (fire-and-forget); `race` cancels losers;
`timeout` cancels the timed-out work. A cooperative yield above an in-flight cap reaps
finished/cancelled tasks so a tight un-awaited loop stays bounded (RSS now flat ~8 MB). This
mirrors the consumer-driven generator decision (work without an owner does not linger) and
matches where single-threaded async runtimes converge (smol `Task` cancel-on-drop, Swift
task groups, Trio nurseries). See spec §7.2.

## References

- **genawaiter** — bidirectional generators/coroutines built on stable Rust async
  (`yield`/resume over an internal rendezvous): <https://crates.io/crates/genawaiter>
- **async-stream** — `Stream`s expressed with `yield` on stable async, via a thread-local
  rendezvous between producer and consumer: <https://crates.io/crates/async-stream>
- **Rust RFC 3513 — `gen` blocks** — language-level `gen`/`async gen` blocks (generators as
  the stackless-coroutine transform underlying async):
  <https://rust-lang.github.io/rfcs/3513-gen-blocks.html>
- **`tokio::task::LocalSet` / `spawn_local`** — running `!Send` futures/tasks on a
  current-thread runtime: <https://docs.rs/tokio/latest/tokio/task/struct.LocalSet.html>
