# AScript Workers — Spec B: Stateful Workers (Actors & Streaming Generators)

- **Status:** Draft for review
- **Date:** 2026-06-07
- **Depends on:** Spec A (`2026-06-07-workers-foundation-stateless-design.md`) — the
  structured-clone serializer, sendability rules, code-shipping, isolate bootstrap, cancel/error
  model, and the `worker` keyword are all reused.
- **Engines:** both (tree-walker oracle == VM, byte-identical)

---

## 1. Summary & motivation

Spec A added the **stateless, pooled** worker lifecycle (`worker fn`, request/response). Spec B
adds the **stateful, dedicated-isolate** lifecycle, which the research showed is a *single*
mechanism powering two surface forms:

| You write | It is | Returns |
|---|---|---|
| `worker class C` | a stateful **actor** | a proxy handle |
| `worker fn* g()` | a streaming **producer** | a streaming handle |

Both are "a thing that lives in its own dedicated isolate for its lifetime, holds state, and is
talked to over time through serialized messages." They share the dedicated-isolate lifecycle,
the mailbox, and the cancel/teardown model — so they are specified and built together.

The actor model is the universal answer for **stateful objects across an isolation boundary**
(Comlink, Java RMI remote objects, .NET `MarshalByRefObject`, Microsoft Orleans grains,
Erlang/Elixir GenServer). The streaming model is the universal answer for **demand-driven
streaming across shared-nothing processes** (Elixir GenStage, gRPC server-streaming, Web
transferable streams, Python `multiprocessing.imap`).

## 2. The dedicated-isolate lifecycle (shared by §3 and §4)

Distinct from Spec A's pool. A dedicated isolate is **born on spawn, lives for its lifetime, and
is torn down on explicit `close()` or last-handle-drop** (extending Spec A's cancel-on-drop).

- **Not pooled.** Bounded by how many the program creates (like open file handles), not by
  `num_cpus`. (Web transferable-streams confirms the requirement: a streamed/stateful worker
  must stay live for its consumer, which a shared pool cannot guarantee.)
- **FIFO mailbox, one message at a time.** The isolate processes one inbound message
  (method call / resume) to completion before the next. This gives the GenServer/Akka
  guarantee: the isolate's state is touched by a single thread of execution, so **no internal
  locks and no data races** — for free.
- **Can own non-sendable resources.** Because the object/generator is *born in* the isolate, it
  may hold native resources (DB connections, sockets, file handles) that are opened inside the
  isolate and **never cross the boundary**. This is the canonical use, not a limitation.
- **Reuses Spec A's serializer** for all message arguments, return values, and yielded values.

## 3. Actors — `worker class`

```
worker class Db {
  field conn
  init(url) { self.conn = postgres.connect(url) }   // opened INSIDE the isolate
  fn query(sql) { return self.conn.query(sql) }
}

let db = await Db.spawn("postgres://…")   // births an isolate; returns a proxy handle
let rows = await db.query("select …")     // async message; only sql + rows serialize
db.close()                                 // (optional) explicit teardown; or drop the handle
```

Semantics:

- **`ClassName.spawn(args)`** births the isolate, runs `init` **in the isolate**, and returns
  `future<handle>` (spawning is async). `spawn` args are clone-checked (§ Spec A 5). A bare
  `ClassName(args)` still constructs a **local** instance — `spawn` is the explicit, honest
  "this births an isolate" form (no overloading of normal construction).
- **The handle is a proxy.** It is a new value kind/handle (see §6), *not* a `Value::Instance`.
  Every method call returns `future<T>` and is delivered as a mailbox message. Matches
  Comlink/RMI/Orleans exactly: "from the developer's perspective it's just an async method
  call."
- **No direct field access across the boundary.** `db.conn` is not readable from outside; expose
  state via methods (`fn get() { return self.count }`). (Comlink exposes fields as async
  getters; we keep the simpler methods-only rule for v1.)
- **State persists** across calls because there is exactly one object, living in one isolate.
- **Single-writer, no races** (mailbox, one-at-a-time).
- **Non-reentrant.** An actor method that calls back into *its own* handle would deadlock its
  own mailbox. This is detected: a same-actor self-call is a **recoverable Tier-2 panic** with a
  clear message (Akka/actor literature: actors are non-reentrant by default). Calls to *other*
  actors are fine.
- **Inheritance / code:** a `worker class` may extend a normal class; the superclass chain +
  method table ship to the isolate via Spec A code-shipping. `instanceof` against the handle
  checks the handle's declared class.
- **Lifecycle:** `close()` drops the isolate body and reclaims it; dropping the last handle does
  the same (cancel-on-drop). In-flight messages on a closed actor resolve to a recoverable
  panic.

## 4. Streaming generators — `worker fn*`

```
worker fn* records(path) {
  for line in file.lines(path) { yield parse(line) }   // reads the file IN the isolate
}

for await rec in records("big.csv") { process(rec) }   // demand-driven, backpressured
```

Semantics — the cross-thread extension of AScript's **already consumer-driven** generators
(`src/coro.rs`: a lazily-polled body, one step per `.next()`, parking at `yield` via `poll_fn`):

- **Calling a `worker fn*`** births a dedicated isolate and returns a **streaming handle**
  (a `Value::Generator` backed by a cross-thread driver, transparent to user code — `for await`
  / `.next()` / `.close()` work unchanged).
- **Demand-driven pull + bounded buffer = backpressure** (GenStage/gRPC): each consumer
  `.next()` is a demand credit across the channel; the producer runs ahead only up to a small
  **prefetch window** (default **1** = strict pull; configurable later). A full buffer parks the
  producer. This is the exact mechanism the consumer-driven local generator already models,
  now over a channel.
- **Bidirectional:** `gen.next(v)` injects `v` back across the boundary as the result of the
  producer's `yield` expression (gRPC bidi streaming). The serializer carries both directions.
- **Close/cancel:** `gen.close()` (or dropping the handle) closes the channel and tears down the
  isolate — extends Spec A cancel-on-drop.
- **Per-element cost:** every `yield` is one serialize/deserialize. Documented guidance:
  **yield chunks, not elements** (Python `imap` `chunksize`). No hard limit; a `prefetch`/chunk
  knob is a later, additive option.

## 5. The event bus across isolates (the bridge pattern)

Cross-isolate broadcast pub/sub is **deliberately not a core primitive** (rejected, §9). The
`events` bus stays **isolate-local and unchanged** — its listeners are closures and its handle
is `Native`, neither sendable. Cross-isolate eventing is expressed with the two forms above:

- **Point-to-point:** call an actor handle (`await logger.event(e)`).
- **Stream:** consume a `worker fn*` (or an actor's `fn*` method); `for await` *is* the
  subscription.
- **Fan-out to local listeners (the bridge):** a single consumer on the main isolate awaits a
  worker stream and re-emits on the **local** bus — cross-isolate hop is one serialized stream,
  fan-out is intra-isolate closures:

```
async fn pipe(gen, bus) { for await e in gen { bus.emit(e.kind, e) } }   // optional sugar
```

**Deliverable:** ship `pipe` as a small documented convenience (in `std/task` or `std/events`)
and a docs section *"Workers and the event bus"* explaining the intra- vs inter-isolate layering
and the bridge idiom. Backpressure threads through end-to-end for free (a slow local listener
slows `emit`, which slows `for await`, which slows the producer).

## 6. Implementation surface & cross-cutting subsystems

Reuses all of Spec A's foundation (serializer, sendability, isolate bootstrap, code-shipping,
cancel/error, the `worker` keyword infrastructure). The additional touchpoints — **each a
required deliverable**:

**Front-ends (two parsers):** `worker class` (class-decl `is_worker` flag) and `worker fn*`
(the fn-decl carries the existing generator flag + Spec A's `is_worker`) in BOTH `src/parser.rs`
and the CST parser (`src/cst/`).

**Tree-sitter grammar (`tree-sitter-ascript/`):** add `worker` on class declarations and the
`worker fn*` combination to `grammar.js`; regen `parser.c` (`--abi 14`); update
`queries/highlights.scm` (and `tags.scm` so `worker class` appears in symbol tagging). **Run
`./scripts/sync-grammar.sh` and bump the pinned SHA in `editors/zed/extension.toml` and
`editors/nvim/lua/ascript/treesitter.lua`** (mandatory on any `tree-sitter-ascript/**` change).
*(If Spec A and B land together, a single grammar sync + pin bump covers both.)*

**Editor integrations (`editors/`):** ensure `worker` (on `class`/`fn*`) is recognized in the
**VS Code TextMate grammar** (`editors/vscode/syntaxes/ascript.tmLanguage.json`) and the bundled
**Zed** (`editors/zed/languages/ascript/highlights.scm`) and **Neovim**
(`editors/nvim/queries/ascript/highlights.scm`) highlight copies — the same three surfaces as
Spec A (a single keyword addition covers both specs if landed together).

**Formatter (`src/fmt.rs` + `ast.rs` `Display`):** render `worker class C { ... }` and
`worker fn* g(...)`; canonical modifier order `static? worker? fn`/`worker class`. Idempotent;
add goldens.

**Checker & types (`src/check/`):**
- **Type inference (SP10):** `ClassName.spawn(args)` synthesizes `future<handle>`; a
  `worker fn*` call synthesizes the streaming-generator type (same surface type as a local
  generator). Actor-method calls on a handle synthesize `future<T>`. Keep `examples/**` at zero
  `type-*` diagnostics in both configs.
- **Call-arity (`std_arity.rs`):** register the `pipe` bridge helper; `spawn` arity is checked
  against the class `init` signature where statically known (reuse the constructor-arity path).
- **Non-reentrancy** is a **runtime** guard (it depends on dynamic actor identity), surfaced as
  a recoverable Tier-2 panic — not a static checker rule. A best-effort `worker-reentrancy` lint
  may flag an obvious literal self-call, default Warning (optional, additive).

**LSP (`src/lsp/`):**
- **Semantic tokens:** `worker` on classes/generators highlighted as a modifier.
- **Document/workspace symbols:** `worker class` and its methods appear in the outline; the
  actor's methods are navigable.
- **Hover:** hovering `Db.spawn` shows `future<Db handle>`; hovering an actor-handle method shows
  it returns `future<T>`; hovering a `worker fn*` shows the streaming type.
- **Navigation:** go-to-def / find-references / rename across `worker class`, its methods, and
  `worker fn*` declarations (extend `src/lsp/workspace.rs` indexing + tests).
- **Completion:** offer `worker` before `class`/`fn`; offer an actor handle's methods after `.`
  (resolved from the handle's declared class).

**REPL:** `worker class`/`worker fn*` are brace-delimited → existing `is_incomplete` buffering
handles them; session persistence of a `worker class` definition works as for any class. Add a
regression test.

**New value/handle kinds:** an **actor handle** and a **cross-thread generator driver** —
modeled as `Value::Native` resource handles (actor handle in `Interp.resources`; the generator
as a `Value::Generator` whose body is a cross-thread driver future) to avoid expanding the core
`Value` union and to inherit deterministic `Drop` reclamation. The GC must **not** trace into
them (native-handle invariant, `CLAUDE.md`).

**Dedicated-isolate manager:** spawn/track/teardown of non-pooled isolates; FIFO mailbox
(one-at-a-time) for actors; demand+bounded-buffer driver for generators; non-reentrancy guard.
Reuses Spec A isolate bootstrap + serializer + code-shipping.

**`.aso`:** `is_worker` on the class layout → **bump `ASO_FORMAT_VERSION`** (or share Spec A's
bump if landed together); update `verify.rs`.

**Determinism (SP9):** an actor's inbound message sequence + results and a generator's
yield/resume sequence are recorded as **event-sourced boundary events** — aligning with the
existing event-sourced workflow subsystem. Extend the `workflow-determinism` lint to flag
unrecorded cross-isolate interaction inside a workflow.

**Docs:** see §8 (workers page + `NAV` entry + the "Workers and the event bus" section).

**Tests:** `frontend_conformance.rs`, `treesitter_conformance.rs`, `vm_differential.rs` (both
configs), `check.rs`, `lsp.rs`, plus §7.

**Unchanged:** `Value` core union (handles are `Native`), `Interp` internals, GC, the
single-threaded hot path, the `events`/`std/sync` primitives.

## 7. Testing, example corpus & performance

### 7.1 Behavioral tests
- **Actors:** spawn → call → state persists across calls; concurrent calls to one actor are
  serialized (FIFO, one-at-a-time) with no interleaving; an actor owning a (mock) native
  resource works and the resource never crosses; non-reentrant self-call → recoverable panic;
  `close()`/last-drop tears down the isolate; in-flight call on a closed actor → recoverable
  panic; method panic → recoverable on caller.
- **Generators:** `for await` over a `worker fn*` yields in order; backpressure (prefetch=1) —
  producer does not run ahead of demand (assert via an instrumented producer); `gen.next(v)`
  injection round-trips; `gen.close()`/drop tears down the isolate.
- **Bridge:** `pipe(gen, bus)` fans a worker stream to multiple local listeners in order; a slow
  listener applies backpressure all the way to the producer.

### 7.2 All-modes execution (REQUIRED)
Every stateful-worker example (§7.3) runs with **identical, order-deterministic output** in all
four modes — **tree-walker, specialized VM, generic VM, and `.aso`-compiled** — via the extended
differential harness (same requirement and rationale as Spec A §11.3). Actor/generator examples
are written so their output ordering is deterministic (drive actors with sequenced awaits;
consume generators in order).

### 7.3 Example corpus (`examples/advanced/` — runnable, doubles as docs & all-modes tests)
- `workers_actor_counter.as` — stateful counter/cache actor; state persists across calls.
- `workers_actor_service.as` — a service actor owning a (mock) connection opened **inside** the
  isolate; fully error-handled (the "resource lives in the actor" pattern).
- `workers_stream_records.as` — `worker fn*` streaming parsed records with demand-driven
  backpressure; consumed via `for await`.
- `workers_stream_bidirectional.as` — `gen.next(v)` injecting values back into the producer.
- `workers_event_bridge.as` — the bridge: a `worker fn*` event source piped onto a **local**
  `events` bus that fans out to multiple listeners.
- `workers_actor_subscribe.as` — an actor exposing a `fn*` `subscribe` method (producer actor).

### 7.4 Performance measurement (REQUIRED, reported)
Extend the Spec A `bench/` harness/report with stateful-worker numbers:
- **Actor throughput:** messages/sec to a single actor (mailbox round-trip cost), and aggregate
  throughput across N independent actors on N cores (scaling).
- **Streaming throughput & the chunking effect:** records/sec for a `worker fn*` at prefetch=1
  vs a larger window, and **per-element vs per-chunk** yielding — quantifies the documented
  "yield chunks, not elements" guidance and the break-even chunk size.
- **Dedicated-isolate spawn cost:** `spawn` latency and steady-state per-message latency.
- Headline numbers on the **VM**; tree-walker informational. Figures recorded in the report; no
  hard CI threshold (CI core counts vary).

## 8. Documentation — new pages + final consistency & staleness sweep

### 8.1 New workers page
- A workers page under `docs/content/` (language guide) — **add its slug to the `NAV` array in
  `docs/assets/app.js`** (sidebar + cmd-K search derive from `NAV`; a page with no entry is
  unreachable — `CLAUDE.md` docs note). In-content links are resolved relative to the current
  page's directory, not absolute-from-root.
- Sections: the model + two lifecycles; `worker fn`/`static worker fn` (from Spec A) + cost
  model + capture/sendability; `worker class` actor semantics (proxy, async-only, no field
  access, non-reentrancy, owns resources); `worker fn*` streaming (demand-driven, bidirectional,
  chunk guidance); "Workers and the event bus" (intra- vs inter-isolate layering + bridge +
  `pipe`).
- Cross-link with the existing concurrency page
  (`docs/content/language/modules-async.md`, where async/concurrency lives) in both directions.

### 8.2 Final documentation consistency & staleness sweep (REQUIRED — runs after BOTH specs land)
A deliberate pass over the **entire** documentation set to (a) integrate workers and (b) catch
and fix **any** stale/contradictory information surfaced along the way — not limited to
worker-related content. Concretely:

- **`README.md`** (repo front door): add workers to the concurrency/capability description and
  the stdlib/feature table; reconcile any "single-threaded"/"no multithreading" phrasing into
  the accurate framing — *single-threaded per isolate, multi-core via shared-nothing workers*;
  verify the CLI list and links.
- **`docs/` static site:**
  - Landing (`docs/index.html`): re-verify the headline stats (value-kind count — note actor &
    generator handles are modeled as `Native`, so the ~16-kind count is **unchanged**; module
    count if a `worker` stdlib helper module is added) and any capability claims.
  - Concurrency content (`docs/content/language/modules-async.md` and any page asserting the
    execution model): update to include workers; remove/repair contradictions with the new
    parallelism story.
  - **Link & nav integrity:** confirm every page (incl. the new workers page) has a `NAV` entry
    and that in-content relative links resolve (the documented orphan/relative-link gotchas).
- **`CLAUDE.md`:** update the "What this is" / concurrency description to mention shared-nothing
  worker parallelism; clarify that the `!Send`, single-threaded model is **per-isolate**
  (parallelism is by isolation, not shared memory) so it isn't misread as "no parallelism
  possible"; add a **"Workers" entry under "Larger subsystems (campaign work)"** documenting the
  architecture (two lifecycles, the serializer airlock, the pool, actors/generators) for future
  sessions; add the feature-flag status (workers are **core/default**, built under
  `--no-default-features`, like the GC — confirm and state).
- **Main design spec** (`superpowers/specs/2026-05-29-ascript-design.md`): amend the non-goal
  *"No multithreading in user code (single-threaded event loop; see §7)"* (line ~50) with a
  supersession note pointing at these two worker specs (shared-nothing parallelism; the
  single-threaded model holds per isolate); cross-reference from §7 (async model).
- **`superpowers/roadmap.md`:** add the workers milestone entry (consistent with the existing
  milestone-by-milestone record).
- **Sanity verification:** read README + `docs/content/**` + CLAUDE.md end-to-end for internal
  consistency (stats, counts, capability claims, execution-model statements, dead links) and fix
  anything stale discovered in the process, worker-related or not. Serve the docs
  (`cd docs && python3 -m http.server`) to confirm the site renders and the new page is reachable
  via sidebar + cmd-K.

## 9. Scope & rejected alternatives

**In scope (Spec B):** `worker class` actors (spawn, proxy handle, async methods, FIFO mailbox,
non-reentrancy guard, resource ownership, close/drop); `worker fn*` streaming generators
(demand-driven pull, bounded buffer, bidirectional `next(v)`, close/drop); the `pipe` bridge
helper + docs; determinism event-sourcing; differential/conformance tests; docs + `NAV` entry.

**Rejected:**
- **Instance worker methods that copy `self` per call.** The copy strategy is for *immutable
  data*, never for stateful objects — mutations vanish and identity is meaningless. Every
  mature system (Comlink/RMI/Orleans/GenServer) uses the proxy/actor model instead, which
  `worker class` provides. Connection-holding objects, which copy-`self` would disqualify
  (non-sendable `self`), are the *ideal* actor case.
- **Cross-isolate broadcast pub/sub bus.** It is effectively a message broker (Send-backed
  subscriber registry, per-subscriber backpressure à la GenStage `DemandDispatcher`,
  subscriber-death cleanup, delivery/ordering guarantees, replay) — a third spec's worth of the
  thorniest semantics. The **bridge pattern** (§5) covers the overwhelming majority of need at
  zero new-primitive cost; a true isolate-mesh broadcast, if ever required, is better served by
  explicit actor forwarding or a purpose-built broker than by core syntax.
- **Cross-boundary field access on actors (`await handle.field`).** Methods-only for v1;
  Comlink-style async field getters are additive and out of scope.

## 10. Grounding (verified sources)

- Stateful objects across isolation = proxy/actor (not copy): Comlink (GoogleChromeLabs); Java
  RMI object model (Oracle); Microsoft Orleans (virtual actors/grains); Elixir GenServer.
- Actor mailbox semantics (FIFO, one-at-a-time → lock-free state; non-reentrant default):
  Akka/Akka.NET; Swift actors (SE-0306).
- Demand-driven streaming across shared-nothing processes: Elixir GenStage; gRPC flow control
  (streaming-only); Web transferable streams (incl. the "must stay live, bad for pools"
  constraint); Python `multiprocessing.imap` (lazy + `chunksize`).
- Boundary copy semantics: WHATWG structured-clone algorithm (shared with Spec A).
