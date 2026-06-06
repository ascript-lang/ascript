# ADR: genai `!Send` bridging — empirical decision (SP11 Phase A)

> **Status:** Decided (in-LocalSet path). Recorded 2026-06-05.
> **Context:** SP11 `std/ai` wraps the `genai` crate (v0.6.4). The one load-bearing
> unknown (design §1 / §7 Q1) was whether genai's `Client::exec_chat` /
> `exec_chat_stream` / `embed` futures require `Send` or assume a multi-thread
> tokio runtime — which would be incompatible with AScript's `!Send`
> current-thread runtime (`#[tokio::main(flavor = "current_thread")]` + a
> `LocalSet`, `Rc`/`RefCell` interior mutability, `await_holding_refcell_ref =
> "deny"`).

## Decision

**Take the in-LocalSet path.** genai runs directly on the current-thread runtime
via `tokio::task::spawn_local`. The documented dedicated single-thread worker
bridge (design §1 fallback) is **NOT needed and NOT built**.

## Evidence (the Phase-A spike)

`tests/ai.rs::spike_genai_runs_on_current_thread_localset_with_rc_in_scope`
constructs a `genai::Client`, points it at a local in-process mock HTTP server
(no network, no key) via a `ServiceTargetResolver`, and drives `exec_chat` inside
`tokio::task::spawn_local(...)` on a `Builder::new_current_thread()` runtime +
`LocalSet`, with an `Rc<()>` (a `!Send` value) held live across the genai
`.await`. It compiles and passes — returning the mocked `"pong"`.

- If genai's futures required `Send`, holding the `Rc` across the await inside a
  `spawn_local` task would still compile (spawn_local does not require Send), but
  the broader point is that genai is plain `reqwest`-based: reqwest works on a
  current-thread runtime, and genai spawns nothing onto a multi-thread pool.
- The spike runs to completion with no "spawn from outside a multi-thread runtime"
  panic, confirming genai does not assume `Handle::current()` is multi-thread.

## Consequences

- **Architecture:** the genai `Client` is built once per `Interp` (with our pooled
  rustls reqwest client injected via `ClientBuilder::with_reqwest`), stored behind
  `Interp.ai` (a `RefCell`). Each call **takes the client out / clones the cheap
  `Arc`-backed handle before any `.await`** (take-out-across-await), so no `RefCell`
  borrow is ever held across a genai future (`await_holding_refcell_ref` stays
  satisfied).
- **Testing seam:** the same `ServiceTargetResolver` mechanism that the spike uses
  to point at a mock is the fixture-replay seam for every SP11 test — recorded
  JSON/SSE bodies served from a loopback `MockServer`, zero secrets, zero outbound
  sockets. genai also accepts a custom `reqwest::Client` (`with_reqwest`) which we
  use for the real, pooled client.
- **No worker thread:** there is no `mpsc` boundary, no `Send` plain-data marshaling
  of request/response structs, and `Value`/`Rc` never cross a thread boundary.
