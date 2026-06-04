# SP9 — Robust recursion, replay durability, determinism seams — Design

> **Status:** approved design, ready for implementation planning (superpowers:writing-plans).
> **Supersedes** the SP9 PROPOSAL (`2026-06-04-sp9-explicit-stack-vm-PROPOSAL.md`), which recommended a
> go/no-go on a monolithic model-2b explicit-stack VM. This design takes the **owner decision**:
> *unbundle the three M17 async non-goals, and do everything achievable WITHOUT building the full
> model-2b VM.* No monolithic explicit-stack/reified-continuation VM is built here.

- **Date:** 2026-06-04
- **Branch:** `feat/sp1-engine-parity`
- **Relates to:**
  - `adr/2026-05-30-async-generators.md` — the three deferred M17 non-goals (#1 durable continuations,
    #2 robust deep recursion, #3 deterministic scheduling).
  - `2026-06-04-sp9-explicit-stack-vm-PROPOSAL.md` — the feasibility study this design realizes.
  - `2026-06-04-sp3-runtime-robustness-design.md` (SP3 §B) — the **graceful** recursion-depth cap.
    SP9 §1 raises/removes the *practical* ceiling at the narrow native re-entry points; SP3's cap
    becomes a safety **backstop**. The two are coordinated, not in conflict (see §1.5).
  - `2026-06-02-bytecode-vm-design.md` — the VM is **model 2a** (an explicit `Fiber.frames` stack with
    a flat run loop; the residual native recursion is the four re-entry points enumerated in §1.2).

**Goal:** Convert each of the three M17 async non-goals from "deferred, needs a model-2b VM" into the
**maximal subset achievable on the existing 2a engine**, and document precisely the residual that
genuinely requires 2b so **nothing is silently dropped**:

1. **Robust unbounded recursion** — remove the native-stack overflow at the four narrow native
   re-entry points with `stacker::maybe_grow`. Cheap, byte-identical, no 2b.
2. **Durable execution** — via **event-sourced deterministic replay** (the Temporal/Restate/Cloudflare
   model), NOT continuation serialization (architecturally impossible with live native handles). A
   `std/workflow` subsystem: a workflow/activity API, an append-only serializable event log, a
   determinism discipline for workflow code, and resume-from-log semantics. Workflow code re-runs on an
   ordinary stack — no 2b.
3. **Determinism seams** — the achievable-without-2b subset: an injectable virtual **clock**, a seeded
   **RNG**, and **recorded I/O ordering** consumed by the replay engine, behind a per-`Interp`
   deterministic-mode context, WITHOUT replacing tokio.

**The explicitly-out residual (requires 2b — documented, not dropped):** bit-for-bit deterministic
interleaving of *arbitrary concurrent tokio tasks* (a program that races N un-coordinated
`task.spawn`s and expects the OS-thread/tokio scheduler order to be reproducible). That needs an owned
single-threaded cooperative scheduler replacing tokio's `LocalSet`/`spawn_local`, i.e. the §1.2
"explicit driver" of the proposal — the one genuine 2b item. SP9 makes determinism reachable for the
*workflow* case (recorded ordering) and for the *single-task* case (seeded clock+RNG), and states the
arbitrary-interleaving case as the named out-of-scope residual (§3.6).

**Architecture:** Three independent workstreams. §1 touches the VM/coro/compiler/tree-walker re-entry
points only (a guard per site). §3 adds a `DeterminismContext` to the shared `Interp` and routes the
clock/RNG/sleep stdlib seams through it. §2 is a new `std/workflow` stdlib module built **on top of**
§3's seams (it needs the recorded clock/RNG/ordering to replay deterministically). The whole-corpus
**three-way differential** (tree-walker == specialized-VM == generic-VM) and the **perf gate** stay
green throughout: §1 is byte-identical by construction; §3's seams are inert unless deterministic mode
is explicitly entered (default runs unchanged); §2 is an additive opt-in module.

**Tech stack:** Rust. New dep: `stacker` (probe-and-grow native stack; widely used, no `unsafe` beyond
the crate's own). Event-log serialization reuses the existing `json` value codec
(`json::to_json_lossy` / `json.parse`) so the **core language config** (`--no-default-features`) story
is explicit (see §2.7). CST front-end → resolver → compiler → `Chunk` → VM (default); tree-walker is
the byte-identical oracle.

---

## Non-goals (explicitly out of SP9)

- **A monolithic model-2b VM** (explicit driver/scheduler replacing tokio + reified continuations +
  CPS-ified native callbacks). Not built. The proposal's §0/§5 recommendation and the owner decision
  agree: do the achievable subset on 2a.
- **Continuation serialization** (freezing a paused `Fiber` to disk). Architecturally unachievable with
  live native handles (open sockets/DB/child-process/TUI in flight) — see §2.1. Durability is achieved
  by **replay** instead. This is a documented *won't-do form*, not a silent drop.
- **Bit-for-bit determinism of arbitrary concurrent-task interleaving.** Requires the owned scheduler
  (2b). SP9 delivers the seeded-clock/RNG + recorded-ordering subset that covers single-task and
  workflow replay; the arbitrary-interleaving case is the named residual (§3.6).
- **Raising the per-platform native stack as the recursion answer.** §1 grows the stack *at the
  re-entry points* via `stacker` segments, which is exactly "robust recursion"; it does not globally
  resize the main thread stack (that would not help the re-entry sliver and is platform-fragile).

---

## §1 — Robust unbounded recursion (achievable now, cheap; no 2b)

### 1.1 Current behavior (verified, file:line)

The VM is already model-2a: a script→script call **pushes a `CallFrame` onto the heap-backed
`fiber.frames` Vec and continues the flat run loop** — it does **not** recurse on the native Rust
stack. So straight script recursion (`fn fib`) is bounded by heap, not the native stack (SP3 §B1
confirms ~1,000,000 compiled recursive calls and ~5,000,000 method calls survive). The **residual**
native-stack recursion — the only places that can `SIGABRT` (exit 134) — is the four narrow re-entry
points the proposal §1.2 enumerated, each now cited exactly:

1. **`Vm::run` itself** — `#[async_recursion::async_recursion(?Send)]` at `src/vm/run.rs:253`, the
   `pub async fn run(&self, fiber: &mut Fiber)` at `:429`. Every re-entry below lands a fresh
   `#[async_recursion]` frame here.
2. **`Vm::call_value`** — `#[async_recursion]` at `src/vm/run.rs:2547`, `pub async fn call_value` at
   `:2548`. The `Value::Closure` arm at `:2584` does `self.run(&mut fiber).await`; this is the genuine
   native recursion for **higher-order stdlib callbacks** (`array.map`/`filter`/`reduce`/comparators/
   middleware/`recover`) — a deep `map`-of-`map` nests Rust frames. The native-callee "other" arm at
   `:2614` re-enters the `Interp` dispatch.
3. **`Vm::invoke_compiled_method`** — `#[async_recursion]` at `src/vm/run.rs:3378`, with
   `self.run(&mut fiber).await` inside; and **`Vm::vm_construct`** — `#[async_recursion]` at `:3282`,
   which calls `call_value` (default thunks at `:3331`) and `invoke_compiled_method` (init at `:3346`).
   These are the non-IC method-dispatch / constructor native re-entry paths.
4. **Generator `resume`** — `src/coro.rs:148` `pub async fn resume`, `:156` `resume_body`, `:216`
   `resume_vm` (which `await`s `Vm::run` to drive a Suspended `Fiber`). Nested generator composition
   (a generator whose body resumes another) nests Rust frames here. The tree-walker analogue is the
   `#[async_recursion]` `run_body`/`eval_expr` chain (`src/interp.rs:865/963/974/1365/1657/…`).
5. **Deeply nested expressions** — the VM compiler `compile_expr` (`src/compile/mod.rs:905`) is a
   **synchronous** recursive `fn` (NOT async), and the tree-walker evaluator `eval_expr`
   (`src/interp.rs:1365`) is `#[async_recursion]`. A `((((…))))` ~50k deep overflows on both
   (SP3 §B1).

### 1.2 Mechanism — `stacker::maybe_grow` at each re-entry point

`stacker::maybe_grow(red_zone, stack_size, || { … })` checks the remaining native stack and, if below
`red_zone`, allocates a **fresh stack segment** before invoking the closure, so deep native recursion
stops overflowing. It is the cheapest possible answer, an external widely-used crate, with **no
semantic change** — a program that previously `SIGABRT`ed now succeeds; nothing that previously worked
changes (the guard is a no-op when the stack is healthy). This is the proposal's recommended path
(§2.1/§5).

The `async` re-entry points (sites 1–4) need the **async-aware** form: `stacker::maybe_grow` is
synchronous, so for an `.await` body we wrap the *driving* call, not the future. Two concrete patterns,
chosen per site:

- **Sites that own the re-entry boundary synchronously** (the compiler `compile_expr` at
  `src/compile/mod.rs:905`, and the tree-walker's *non-async* recursion if any): wrap the recursive
  self-call body directly in `stacker::maybe_grow(RED_ZONE, STACK_SIZE, || self.compile_expr(inner))`.
  Pure, no async.
- **The `#[async_recursion]` re-entry points** (`Vm::run` re-entry via `call_value` `:2584` /
  `invoke_compiled_method` / `vm_construct`; `coro::resume_vm`; tree-walker `run_body`/`eval_expr`):
  these `.await` a future, so use **`stacker::maybe_grow`'s segment around the synchronous poll
  boundary that `async_recursion` already inserts** — concretely, place the guard at the **call site
  that constructs the boxed re-entry future** (e.g. wrap the `Box::pin` driver, or use
  `async_recursion`'s own boxing as the segment seam). The pragmatic, verified-equivalent form is to
  grow the stack **once per logical re-entry** at the narrow funnel: in `call_value`'s `Value::Closure`
  arm before `self.run(...).await`, in `invoke_compiled_method` before its `self.run(...).await`, in
  `resume_vm` before its `Vm::run` await, and in the tree-walker's `run_body` entry. Because
  `async_recursion` boxes each recursive future, the synchronous portion of each re-entry (the part
  that consumes native stack before the next suspension point) runs inside the grown segment. The
  red-zone heuristic guarantees we never enter the boxed future with less than `RED_ZONE` native stack.

> **Implementation note for the planner:** the exact wrapping form at an `#[async_recursion]` boundary
> must be **measured** (the failing deep-`map`/deep-generator/deep-paren reproducers from SP3 §B1 must
> go from exit 134 → success), not assumed. The plan's Phase 1 writes those reproducers as failing
> tests FIRST, then adds the guard, then re-runs to green. If a pure-async-boundary `maybe_grow` proves
> insufficient at a site, the documented fallback is the **re-entry trampoline** for that one site
> (proposal §2.1, second bullet — convert the recursive `run().await` into a fiber-stack loop): no new
> dep, no `unsafe`. `stacker` is the default; the trampoline is the per-site escape hatch. Either way
> the change is local to the four re-entry points.

### 1.3 Constants and tuning

- `RED_ZONE`: the minimum remaining native stack below which a fresh segment is allocated. Conservative
  default **128 KiB** (must comfortably exceed the largest single VM/tree-walker frame; the
  `#[async_recursion]` frames in `interp.rs` are "very large" per SP3 §B1, so 128 KiB is deliberately
  generous).
- `STACK_SIZE`: the size of each freshly-allocated segment. Default **2 MiB** (matches a typical thread
  stack; amortizes the allocation across many re-entries).
- Both live as named `const`s in a single `src/vm/stack.rs` (or a small `recursion` module) so the two
  engines share them and the values are auditable in one place.

### 1.4 Byte-identical / no-2b rationale

- **No `Value`, opcode, AST, or scheduling change.** `stacker` only relocates *where* native frames
  live; the program's observable behavior (stdout, exit, panic messages/spans) is unchanged for every
  program that already terminated. The whole-corpus three-way differential is **byte-identical** by
  construction (no corpus program recurses deep enough to even trigger a segment allocation).
- **A program that previously overflowed now succeeds.** This is the only observable change, and it is
  strictly an improvement (exit 134 / `SIGABRT` → correct result), applied **identically on both
  engines** (the guard sits at the matching logical re-entry on each), so the differential stays green
  for the newly-succeeding programs too.
- **No 2b.** No explicit driver, no reified continuations, no CPS transform of native callbacks, no
  tokio replacement. The frame stack we already have (model 2a) + a stack-grow guard delivers robust
  recursion — exactly the proposal's conclusion.

### 1.5 Coordination with SP3's graceful cap (no conflict)

SP3 §B adds a single **logical** `call_depth: Cell<u32>` on `Interp` that raises a clean Tier-2
`maximum recursion depth exceeded` at a conservative `MAX_CALL_DEPTH`, **identically on both engines**,
before the native stack blows. SP9 §1 removes the *native* overflow at the re-entry points. The two
compose cleanly:

- **SP3's cap stays as the product default and the safety backstop.** A clean catchable error at a
  fixed logical depth is the right default (AScript's "no hidden control flow" ethos), and it bounds
  the heap-`frames` growth too (which `stacker` does NOT bound — `stacker` trades stack overflow for
  unbounded memory growth, which is *less* desirable as an always-on default; see proposal open Q3).
- **SP9 §1 makes the native re-entry paths able to reach SP3's logical cap without `SIGABRT`ing first.**
  Before SP9, the native re-entry paths (deep `map`/generator-compose) overflow *far sooner* than
  SP3's logical cap (SP3 §B1: a few thousand native frames vs the cap's high logical limit). After
  SP9, those paths grow native stack on demand, so the **logical cap (SP3) is the real ceiling**, hit
  identically on both engines — which is exactly what keeps them byte-identical.
- **`MAX_CALL_DEPTH` is therefore raised** (its value is owned by SP3; SP9 records that with §1 in
  place the cap can be set to the desired *logical* recursion ceiling rather than a conservative
  native-stack-derived one). If the owner wants genuinely unbounded recursion (memory-bounded only),
  the cap becomes opt-out (a `Vm` mode) — see §1.6 open question. Default: cap stays on; `stacker`
  ensures the cap (not the native stack) is what fires.

### 1.6 Open question (recursion)

- Should "robust mode" (the `stacker` guard) be **always-on** (current §1 design: yes, the guard is
  inert until needed) while SP3's logical cap remains the actual ceiling — i.e. `stacker` exists only
  to ensure the re-entry paths *reach* the cap rather than `SIGABRT` early? Or should there be an
  explicit opt-in "unbounded recursion" `Vm` mode that *also* raises/disables SP3's cap (trading clean
  errors for OOM risk)? **Design recommendation:** always-on `stacker` + cap-stays-on (no behavior
  change beyond "re-entry paths reach the cap cleanly"); a separate opt-in unbounded mode is a future
  knob, not SP9. Owner sign-off requested (see open questions at end).

### 1.7 File-touch map (§1)

| Area | Files |
|---|---|
| New constants module | `src/vm/stack.rs` (or `src/recursion.rs`): `RED_ZONE`, `STACK_SIZE`, a `grow` helper |
| VM re-entry | `src/vm/run.rs` (`call_value` `:2584`, `invoke_compiled_method` `:3378`, `vm_construct` `:3282`) |
| Generator re-entry | `src/coro.rs` (`resume_vm` `:216`) |
| Compiler nested-expr | `src/compile/mod.rs` (`compile_expr` `:905`, synchronous) |
| Tree-walker re-entry | `src/interp.rs` (`run_body`, `eval_expr` `:1365`) |
| Dep | `Cargo.toml` (`stacker`, core/unconditional — must build under `--no-default-features`) |
| Tests | `tests/vm_limits.rs` (deep `map`/generator/paren reproducers: exit 134 → success, both engines) |

---

## §2 — Durable execution via event-sourced replay (`std/workflow`; no 2b)

### 2.1 Why NOT continuation serialization (the won't-do form, documented)

The ADR imagined durability as "serialize a paused continuation to disk and thaw it in a fresh
process." With the explicit `Fiber`, the *in-memory* state (`frames`+`stack`+`ip`+cells) is
addressable — but everything it **points at** defeats serialization:

- **Native resources are unserializable.** A paused workflow almost always holds a `Value::Native`
  handle (open TCP socket, streaming HTTP body, SQLite connection/transaction, child process, TUI
  terminal, SSE/WebSocket stream — all backed by `Interp.resources` at `src/interp.rs:285`). These are
  live kernel/OS state, not data. You cannot freeze a half-read socket and thaw it later. This is the
  universal industry finding (proposal §2.3; Restate/Temporal citations).

So SP9 does **not** serialize continuations. Instead it adopts what the entire durable-execution
industry converged on — **event-sourced deterministic replay** — which needs **no 2b VM** (Temporal
implements it for ordinary Java/Go/Python on a normal stack):

> A **workflow** is deterministic AScript code. Its non-deterministic effects (I/O, time, randomness,
> network) are performed **only** inside **activities**. The engine persists an append-only **event
> log** of every activity's *result* (a serializable `Value`). On resume after a crash, a fresh run
> **re-executes the workflow code from the top**, but each activity call, instead of re-running its
> side effect, **returns its recorded result from the log** — so the workflow deterministically
> fast-forwards to exactly where it left off. The continuation is *reconstructed by replay*, never
> serialized. Native handles live only *inside* activities and never cross a suspension boundary.

### 2.2 Surface API (`std/workflow`)

A new feature-gated stdlib module `std/workflow` (`src/stdlib/workflow.rs`), registered in both match
arms of `src/stdlib/mod.rs`, gated by a `workflow` Cargo feature (default-on, depends on `data` for
JSON serialization — mirrors the `log` feature's dependency, see §2.7). Exports:

```as
import { run, activity, resume } from "std/workflow"

// An activity wraps a side-effecting function. Its RESULT is what gets recorded.
// `activity(name, fn)` returns a callable; calling it inside a workflow records
// (on first run) or replays (on resume) its result by the call's deterministic
// sequence position.
let fetchUser = activity("fetchUser", async fn (id) {
    return await http.get("https://api/users/" + id)   // native handle lives ONLY here
})

let chargeCard = activity("chargeCard", async fn (amt) {
    return await sql.exec(db, "INSERT INTO charges ...", [amt])
})

// A workflow is DETERMINISTIC code: control flow + calls to activities.
// No direct I/O, no `time.now`/`math.random`/`uuid.v4` except via the workflow ctx.
fn signupFlow(ctx, input) {
    let user = ctx.call(fetchUser, input.id)       // recorded/replayed
    let now  = ctx.now()                            // recorded virtual clock (§3)
    let id   = ctx.random()                         // recorded seeded RNG  (§3)
    ctx.call(chargeCard, user.plan.price)
    return { user: user, at: now, txn: id }
}

// `run` executes a workflow to completion, persisting events to a log sink.
let result = await workflow.run(signupFlow, input, { log: "flows/signup-42.log" })

// `resume` re-runs the SAME workflow against an existing log: completed activities
// replay from the log; the first not-yet-recorded activity executes for real and
// is appended. Idempotent: resuming a completed log returns the recorded result
// without re-running anything.
let result = await workflow.resume(signupFlow, input, { log: "flows/signup-42.log" })
```

**The workflow context `ctx`** is the single seam through which a workflow touches non-determinism:
- `ctx.call(activity, ...args)` — record-or-replay an activity result by sequence position.
- `ctx.now()` — the virtual clock (§3); recorded on first run, replayed on resume.
- `ctx.random()` / `ctx.uuid()` — the seeded RNG / deterministic uuid (§3); recorded/replayed.
- `ctx.sleep(ms)` — a *durable timer*: records a "wake at T" event; on resume, if T has passed, returns
  immediately (no real sleep). (Phase 2c — see plan.)

Workflow code that calls a side-effecting stdlib fn **directly** (not via an activity / `ctx`) is a
**determinism violation** — detected and reported (§2.5).

### 2.3 Event-log format (append-only, serializable Values — NOT continuations)

The log is an append-only sequence of newline-delimited JSON records (one event per line), serialized
via the existing `json::to_json_lossy` codec (cycles→`"[Circular]"`, functions→`"<function>"`,
NaN→null — never panics; same total-serialization guarantee `std/log` relies on). Record shape:

```jsonc
{ "seq": 0, "kind": "WorkflowStarted", "input": <Value>, "wf": "signupFlow", "ts": 1717459200000 }
{ "seq": 1, "kind": "ActivityCompleted", "name": "fetchUser", "args": [<Value>...], "result": <Value> }
{ "seq": 2, "kind": "ClockRead",  "value": 1717459200123 }
{ "seq": 3, "kind": "RandomRead", "value": 0.5734 }
{ "seq": 4, "kind": "ActivityFailed", "name": "chargeCard", "args": [...], "error": <Value> }
{ "seq": 5, "kind": "TimerSet",    "wake": 1717459260000 }
{ "seq": 6, "kind": "WorkflowCompleted", "result": <Value> }
```

- **`seq` is the deterministic sequence position** — on replay, the engine matches the workflow's
  Nth `ctx`-effect to the Nth recorded event of that kind, and asserts the **call signature matches**
  (activity name + args hash) so a code change that reorders effects is caught as a **non-determinism
  error** (Temporal's "non-deterministic workflow" detection) rather than silently replaying a wrong
  value (§2.5).
- **Only `Value`-serializable data persists.** Activity *results* must be `Value`s the JSON codec
  round-trips (Number/Str/Bool/nil/Array/Object/Map/Bytes; cycles handled like `std/log`). A result
  that is a native handle / function / class is a **constraint violation** reported at record time
  (you returned a socket from an activity — return its *data* instead). This is the explicit, honest
  constraint (§2.6), the same one every durable-execution system imposes.
- **Append-only & crash-safe.** Each event is `fsync`-appended before the workflow proceeds past it
  (configurable: `{ durability: "fsync" | "buffered" }`). A crash mid-activity leaves the log without
  that activity's `ActivityCompleted`, so resume re-executes it (at-least-once activity execution — the
  standard durable-execution guarantee; activities must be idempotent or guarded, documented in §2.6).

### 2.4 Resume-from-log semantics (re-run, fast-forward; no 2b)

`workflow.resume(wf, input, {log})`:
1. Read the log; if it ends in `WorkflowCompleted`, return that result (idempotent no-op).
2. Otherwise **re-run `wf(ctx, input)` from the top on an ordinary stack.** The `ctx` is in **replay
   mode**: each `ctx.call`/`ctx.now`/`ctx.random` consumes the next matching recorded event and returns
   its value **without executing the side effect**, asserting the signature matches (§2.5).
3. When `ctx` reaches an effect with **no corresponding recorded event** (the point the previous run
   crashed at), it switches to **record mode**: executes the activity for real, appends the event,
   and continues recording from there.
4. On `WorkflowCompleted`, append the terminal event and return.

Because step 2 re-runs ordinary code that fast-forwards through recorded results, **no continuation is
serialized and no 2b VM is needed** — this is precisely why Temporal/Restate/Cloudflare work on stock
runtimes. The workflow's local variables are reconstructed by re-execution; only activity *results*
(and clock/RNG reads) come from the log.

### 2.5 Determinism constraints on workflow code (enforced)

Workflow code MUST be deterministic so replay reaches the same effects in the same order. Enforced two
ways:

- **Runtime replay-mismatch detection (always on).** During replay, if the Nth `ctx` effect's
  signature (kind + activity name + args hash) does not match the Nth recorded event, the engine raises
  a Tier-2 panic **`workflow non-determinism: expected <recorded> at seq N, got <actual>`** — the
  standard Temporal failure mode, surfacing a workflow-code change that broke replay.
- **Static lint (`std/check` rule, additive, zero-FP).** A `workflow-determinism` checker rule
  (`src/check/rules/`) flags, *inside a function passed to `workflow.run`/`resume`*, direct calls to
  known non-deterministic stdlib seams (`time.now`, `date.now`, `math.random`, `crypto.randomBytes`,
  `uuid.v4`, `net.*`, `fs.*`, `sql.*` outside an `activity`) — recommending the `ctx`/activity form.
  Best-effort (a workflow passed indirectly may not be analyzable) and **zero false positives on the
  corpus** (the existing checker bar); the *runtime* detector is the authoritative guarantee.

### 2.6 Honest constraints (documented, not silently dropped)

1. **Workflow code must be deterministic** — same inputs ⇒ same control flow ⇒ same effect order. Loops
   over recorded data, conditionals on recorded values, and `ctx`-mediated time/RNG are fine; direct
   I/O / `time.now` / `math.random` in workflow body are violations (§2.5).
2. **Only `Value`-serializable activity results persist** — a native handle/function/class returned
   from an activity is a constraint violation at record time. Activities return *data*; native handles
   live only *inside* the activity body and never cross the log boundary.
3. **Activities are at-least-once** — a crash between side effect and `ActivityCompleted` append re-runs
   the activity on resume. Activities must be idempotent or externally guarded. (Exactly-once would need
   two-phase commit with the external system — out of scope; documented like every other system does.)
4. **Native handles never survive a restart** — they live only inside activities. A workflow that
   conceptually "holds a connection across a sleep" must re-establish it in the next activity (the
   `transient`/re-establish discipline from the proposal §2.3).

### 2.7 `--no-default-features` story (explicit)

The event log uses the `json` codec, which lives behind the `data` feature (`serde`/`serde_json` are
optional in `Cargo.toml:28-29`, gated by `data`). Therefore `std/workflow` is a **`workflow` Cargo
feature that depends on `data`** (exactly the pattern `log` uses — CLAUDE.md: "`log` … depends on
`data` for JSON serialization"). Under `--no-default-features` the module is `#[cfg]`-compiled out and
`import "std/workflow"` is an unknown-module error — symmetric on both engines, no partial subsystem.
The §1 recursion guard and the §3 seams remain core/unconditional (they don't need `serde`): the
`DeterminismContext` records `Value`s in-memory using plain Rust types, and only the *persistence* of
the workflow log needs JSON. So determinism-mode replay of an in-memory workflow could in principle
work without `data`, but durable (on-disk) workflows require `data`; SP9 gates the whole `std/workflow`
module on `data` for simplicity and honesty (no half-feature).

### 2.8 File-touch map (§2)

| Area | Files |
|---|---|
| New stdlib module | `src/stdlib/workflow.rs` (`exports`, `call`, `run`/`activity`/`resume`, `ctx`, event log) |
| Routing | `src/stdlib/mod.rs` (both match arms; `pub mod workflow` gated `#[cfg(feature="workflow")]`) |
| Feature | `Cargo.toml` (`workflow = ["data"]`, in `default`) |
| Event-log codec | reuse `src/stdlib/json.rs` (`to_json_lossy`, `parse`) |
| Determinism source | depends on §3's `DeterminismContext` (clock/RNG/ordering seams) |
| Checker | `src/check/rules/workflow_determinism.rs` (additive lint, zero-FP) |
| Docs / examples | `docs/content/stdlib/workflow.md`, `examples/advanced/workflow_*.as` |
| Tests | `tests/workflow.rs` (record → crash-simulate → resume → byte-identical result; replay-mismatch detection; idempotent resume) |

---

## §3 — Determinism seams (the achievable subset; no tokio replacement, no 2b)

### 3.1 What's achievable now vs the 2b residual

| Seam | Achievable on 2a (SP9) | Mechanism |
|---|---|---|
| **Virtual clock** (`time.now`, `date.now`, `monotonic`) | ✅ | inject a clock into `Interp`; deterministic mode reads recorded/virtual time |
| **Seeded RNG** (`math.random`, `randomInt`, `shuffle`, `uuid.v4`, `crypto.randomBytes`) | ✅ | replace the thread-local seed with a per-`Interp` seeded PRNG |
| **Recorded I/O ordering** (for workflow replay) | ✅ | the §2 event log records the order/result of `ctx`-mediated effects |
| **Durable timers** (`ctx.sleep`) | ✅ | recorded "wake at T"; resume fast-forwards |
| **Bit-for-bit interleaving of arbitrary concurrent `task.spawn`/`race`/`gather`** | ❌ **(2b residual)** | needs an owned single-threaded cooperative scheduler replacing tokio's `LocalSet` — §3.6 |

The seams are inert in the default run; they activate only when a **deterministic-mode context** is
entered (by `workflow.run`/`resume`, or an explicit `--deterministic --seed N` future CLI flag). So the
whole-corpus three-way differential is **unchanged** by default (§3.5).

### 3.2 The `DeterminismContext` (per-`Interp`)

Add an optional `determinism: RefCell<Option<DeterminismContext>>` to `Interp` (beside `inflight`/
`log_level` at `src/interp.rs:305-317`). When `None` (default), every seam behaves exactly as today.
When `Some`, the seams route through it:

```rust
struct DeterminismContext {
    mode: Mode,                 // Record | Replay
    clock: VirtualClock,        // current virtual ms-epoch; advances only on ctx.sleep / recorded reads
    rng: SeededRng,             // a deterministic PRNG (xorshift, same algo as math.rs:337) seeded by `seed`
    seed: u64,
    cursor: usize,              // replay cursor into the recorded event stream
    events: Vec<DetEvent>,      // ClockRead / RandomRead / ordering markers (the in-memory event stream
}                               // §2's on-disk log is the persisted projection of this)
```

`DeterminismContext` uses only core Rust types (no `serde`) so §3 is **core/unconditional** — it builds
under `--no-default-features`. (The *persistence* to disk is §2's `data`-gated concern.)

### 3.3 Clock seam

- `time.now` (`src/stdlib/time.rs:36-42`, currently `SystemTime::now()`), `time.monotonic` (`:43`,
  `Instant`-based via the `START` `LazyLock` at `:29`), and `date.now` (`src/stdlib/date.rs:83`,
  `Utc::now()`) read the wall/monotonic clock directly today.
- **Change:** route each through `Interp`. Add an `Interp::clock_now_ms()` / `clock_monotonic_ms()` that
  returns the real clock when `determinism` is `None`, and the `VirtualClock` value (record: read real
  clock once and append a `ClockRead`; replay: return the recorded value at `cursor`) when `Some`.
  The stdlib seams (`time`/`date`) take `&Interp` (the dispatch already threads it for async fns — see
  `src/stdlib/mod.rs:402` `time.sleep`) and call the accessor.
- `time.sleep` (`src/stdlib/mod.rs:409`, `tokio::time::sleep`): in deterministic mode, do **not** sleep
  real time — advance the virtual clock and (in a workflow) record a `TimerSet`/durable-timer event;
  replay returns immediately if the wake time has passed.

### 3.4 RNG seam

- `math.rs` already isolates randomness behind `next_random()` (`src/stdlib/math.rs:337`) backed by a
  **thread-local xorshift `RNG: Cell<u64>`** seeded from time+stack-addr (`:322-335`). This is the
  cleanest possible seam to take over.
- **Change:** `next_random()` consults `Interp.determinism`: when `Some`, draw from the context's
  `SeededRng` (record: draw + append `RandomRead`; replay: return the recorded value) instead of the
  thread-local. When `None`, the thread-local path is **byte-identical to today**. Same routing for
  `randomInt`/`shuffle`/`sample` (they already funnel through `next_random`).
- `uuid.v4` (`src/stdlib/uuid.rs:15`, `Uuid::new_v4()`) and `crypto.randomBytes`
  (`src/stdlib/crypto.rs:105`, `thread_rng().fill_bytes`): in deterministic mode, derive their bytes
  from the context's `SeededRng` so they are reproducible; in default mode, unchanged. (These take the
  `Interp`/ctx; `uuid`/`crypto` dispatch is sync today, so the deterministic path reads the context
  via the shared `Interp` handle the dispatcher already has.)

### 3.5 Byte-identical / no-2b rationale (§3)

- **Default runs are unchanged.** `determinism == None` is the default; every seam's `None` branch is
  the exact current code path. The whole-corpus three-way differential is byte-identical (no corpus
  program enters deterministic mode).
- **No tokio replacement.** The seams control *time/RNG values and recorded ordering*, not *task
  scheduling*. tokio's `LocalSet`/`spawn_local` (`src/vm/run.rs:934`) stays. A single-task or
  workflow program is deterministic because its *inputs* (clock/RNG) and its *recorded effect order*
  are pinned — not because the scheduler is owned.
- **A determinism oracle (new test).** Run a single-task program twice with the same seed in
  deterministic mode and assert byte-identical output (`tests/determinism.rs`). This is additional to
  the differential (the differential checks final-output equality across engines; the determinism
  oracle checks *same-seed-same-output*).

### 3.6 The explicitly-out 2b residual (named, not dropped)

**Bit-for-bit reproducible interleaving of arbitrary concurrent tasks is OUT of SP9 and requires 2b.**
Concretely: a program that spawns N un-coordinated `task.spawn`/`race`/`gather` tasks that interleave
through tokio's scheduler, and expects the *interleaving order itself* to be reproducible across runs,
cannot be made deterministic without **replacing tokio's `LocalSet`/`spawn_local` with an owned
single-threaded cooperative run-queue** — the proposal §1.2 "explicit driver" / §2.2 "own scheduler."
That is the one genuine model-2b item. SP9 does **not** build it.

What SP9 *does* cover, so this residual is as small as honestly possible:
- **Single-task determinism** — fully covered (clock+RNG seams). The overwhelmingly common case
  (a workflow, a script, a test) is one logical thread of control through activities.
- **Workflow ordering** — covered by the §2 event log's `seq`: a workflow's *own* effect order is
  recorded and replayed deterministically even though the underlying tokio scheduling is not pinned,
  because the workflow body is deterministic code and activities are sequenced by `seq`.
- **NOT covered:** two activities started concurrently inside one workflow whose *completion order*
  races. SP9's recommendation is that workflows sequence activities (`ctx.call` is await-sequenced);
  concurrent in-workflow fan-out with reproducible completion order is the 2b-residual edge and is
  documented as such (a workflow may `ctx.call` activities sequentially for determinism; parallel
  fan-out with deterministic join is the residual).

This is the precise, owner-requested statement of "what still requires 2b": **arbitrary
concurrent-task interleaving determinism.** Everything else in non-goal #3 (virtual clock, seeded RNG,
recorded ordering for replay) is delivered on 2a.

### 3.7 File-touch map (§3)

| Area | Files |
|---|---|
| Context | `src/interp.rs` (`determinism: RefCell<Option<DeterminismContext>>`; `clock_now_ms`/`clock_monotonic_ms`/`next_seeded` accessors) |
| New module | `src/det.rs` (`DeterminismContext`, `VirtualClock`, `SeededRng`, `DetEvent`) — core, no `serde` |
| Clock seam | `src/stdlib/time.rs` (`now`/`monotonic`), `src/stdlib/date.rs` (`now`), `src/stdlib/mod.rs` (`time.sleep`) |
| RNG seam | `src/stdlib/math.rs` (`next_random` consults ctx), `src/stdlib/uuid.rs` (`v4`), `src/stdlib/crypto.rs` (`randomBytes`) |
| Tests | `tests/determinism.rs` (same-seed-same-output oracle; default-mode byte-identical to today) |
| Docs | `docs/content/stdlib/{time,math}.md` (deterministic-mode note), `docs/content/stdlib/workflow.md` |

---

## §4 — Testing & quality bar (whole sub-project)

- **Differential oracle never relaxed.** Whole-corpus three-way (tree-walker == specialized-VM ==
  generic-VM) byte-identical, plus recorded goldens, plus the new per-workstream tests. §1 is
  byte-identical by construction (no corpus program recurses deep enough); §3 is byte-identical by
  default (seams inert unless deterministic mode is entered); §2 is an additive opt-in module. Any
  divergence on valid code = fix the root cause, never weaken the assertion.
- **Both feature configs.** `cargo test` green default AND `--no-default-features` (the latter exercises
  §1's `stacker` guard and §3's core seams; `std/workflow` is `#[cfg]`-out and its absence is asserted).
- **Clippy clean** under `--all-targets` AND `--no-default-features --all-targets`;
  `await_holding_refcell_ref` stays denied + clean (the `DeterminismContext` is read via `Cell`/short
  borrows; never hold its `RefCell` across an `.await` — take the needed value out first, exactly like
  the `resources` take-out-across-await discipline).
- **Perf gate.** geomean ≥2× compute-bound, no spec-vs-generic regression (`tests/vm_bench.rs`). §1's
  guard is a cheap remaining-stack probe per re-entry (not per opcode) — measure it adds no measurable
  regression to the hot IC call path (which does NOT route through `call_value`/`invoke_compiled_method`
  — confirm the `self.f(...)` IC fast path is untouched).
- **Determinism oracle** (`tests/determinism.rs`): same-seed-same-output for a single-task program.
- **Workflow oracle** (`tests/workflow.rs`): record → simulate crash (truncate log mid-activity) →
  resume → byte-identical final result; replay-mismatch detection on a deliberately
  non-deterministic workflow; idempotent resume of a completed log.
- **Per-task commit** with trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
  Independent per-phase review (re-read spec, re-run gates, adversarial edge probe).

---

## §5 — Consolidated file-touch map

| Workstream | Files |
|---|---|
| §1 recursion | `Cargo.toml` (stacker), `src/vm/stack.rs` (new), `src/vm/run.rs`, `src/coro.rs`, `src/compile/mod.rs`, `src/interp.rs`, `tests/vm_limits.rs` |
| §2 durability | `src/stdlib/workflow.rs` (new), `src/stdlib/mod.rs`, `Cargo.toml` (workflow feature), `src/check/rules/workflow_determinism.rs` (new), `tests/workflow.rs`, `docs/content/stdlib/workflow.md`, `examples/advanced/workflow_*.as` |
| §3 determinism | `src/det.rs` (new), `src/interp.rs`, `src/stdlib/{time,date,math,uuid,crypto}.rs`, `src/stdlib/mod.rs`, `tests/determinism.rs`, `docs/content/stdlib/{time,math}.md` |
| ADR update | `docs/superpowers/specs/adr/2026-05-30-async-generators.md` (reclassify the three non-goals per SP9) |

---

## §6 — Open questions for the owner

1. **(Recursion, §1.6)** Always-on `stacker` + SP3 cap stays the ceiling (recommended), vs an explicit
   opt-in "unbounded recursion" `Vm` mode that also disables SP3's cap (trades clean errors for OOM
   risk)? Default recommendation: the former.
2. **(Durability, §2)** Is `std/workflow` wanted as a real shipped subsystem now, or speced-and-deferred
   (build §1 + §3 first, §2 on demand)? It is the largest workstream; the plan phases it last and in
   sub-phases so it can be cut without affecting §1/§3.
3. **(Durability, §2.3)** Log format: newline-delimited JSON (chosen here, reuses `json` codec, human
   inspectable) vs a binary `.aso`-style framed log? JSON chosen for inspectability + codec reuse;
   confirm.
4. **(Determinism, §3)** Is deterministic mode entered ONLY via `workflow.run`/`resume` (SP9 default),
   or also via a top-level `--deterministic --seed N` CLI flag for whole-program replayable runs?
   The CLI flag is a small addition once the `DeterminismContext` exists; left as a follow-up unless
   wanted now.
5. **(Determinism residual, §3.6)** Confirm the arbitrary-concurrent-task-interleaving determinism is
   acceptable to leave as the named 2b residual (it is the one item that genuinely needs the owned
   scheduler). Workflows stay deterministic by sequencing activities; parallel in-workflow fan-out with
   reproducible join order is the documented edge.

---

## §7 — References

(Inherited from the SP9 PROPOSAL §7 — Temporal/Restate/Cloudflare event-sourced replay; FoundationDB
Flow / Antithesis deterministic-simulation seams; `stacker`/`corosensei`/`tramp` recursion options;
CPython-frame / WasmFX / Lua-resumable-VM prior art for the 2b residual.) The key load-bearing
citations for SP9's choices:

- **Replay, not continuation serialization** — Temporal event-history & deterministic-workflow model;
  Restate "non-serializable resources" finding (proposal §2.3).
- **`stacker` probe-and-grow** — the recommended cheap robust-recursion path (proposal §2.1, §5.2).
- **Seeded clock/RNG + recorded ordering** — FoundationDB Flow / Antithesis DST seams, applied as a
  per-`Interp` context rather than a tokio replacement (proposal §2.2).
