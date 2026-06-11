# `std/workflow` — durable execution via event-sourced replay

`std/workflow` provides **durable execution**: long-running, fault-tolerant
workflows that survive a crash and resume exactly where they left off — without
serializing a paused continuation (which is impossible with live native handles like
open sockets or DB connections). It uses the **event-sourced deterministic replay**
model the durable-execution industry converged on (Temporal / Restate / Cloudflare):
workflow code re-runs on an ordinary stack, and completed effects replay from an
append-only log instead of re-executing.

> Feature: `std/workflow` is gated on the `workflow` Cargo feature (default-on),
> which depends on `data` for the JSON event log. Under `--no-default-features`,
> `import "std/workflow"` is an unknown-module error.

## The model

- A **workflow** is **deterministic** AScript code: control flow plus calls to
  **activities** through the workflow **`ctx`**.
- An **activity** wraps a side-effecting function (I/O, time, randomness, network).
  Its **result** — which must be `Value`-serializable data — is what gets recorded.
- The engine persists an append-only **event log** (newline-delimited JSON). On
  `resume`, the workflow re-runs from the top, but each recorded `ctx.call`/`ctx.now`/
  `ctx.random` returns its logged value **without re-executing the side effect**, so
  the workflow fast-forwards to the crash point and continues.

## API

```ascript
import { run, resume, activity } from "std/workflow"

// An activity wraps a side-effecting fn. Its RESULT is recorded.
let fetchUser = activity("fetchUser", (id) => {
  // native handles (sockets/DB) live ONLY inside the activity, never across the log
  return { id: id, plan: { price: 4200 } }
})
let chargeCard = activity("chargeCard", (amount) => {
  return { ok: true, amount: amount }
})

// A workflow is deterministic: control flow + ctx-mediated effects.
fn signup(ctx, input) {
  let user = ctx.call(fetchUser, input.id)   // recorded / replayed by sequence
  let at   = ctx.now()                        // recorded virtual clock
  let id   = ctx.uuid()                        // recorded seeded uuid
  ctx.call(chargeCard, user.plan.price)
  return { at: at, txn: id }
}

// run: execute to completion, persisting events to a log file.
let result = await run(signup, { id: 42 }, { log: "flows/signup-42.log" })

// resume: re-run against an existing log. Completed activities replay; the first
// not-yet-recorded effect executes for real and is appended. Resuming a COMPLETED
// log returns the recorded result without re-running anything (idempotent).
let again = await resume(signup, { id: 42 }, { log: "flows/signup-42.log" })
```

### The workflow `ctx`

The `ctx` is the single seam through which a workflow touches non-determinism. Its
methods are call-position only (`ctx.now()`, not a bare `ctx.now`):

| method | meaning |
|---|---|
| `ctx.call(activity, ...args)` | record-or-replay an activity result by sequence position |
| `ctx.now()` | the virtual clock (ms-epoch), recorded/replayed |
| `ctx.random()` | a seeded `[0,1)` value, recorded/replayed |
| `ctx.uuid()` | a deterministic v4 UUID, recorded/replayed |
| `ctx.sleep(ms)` | a **durable timer**: records a wake time and advances the virtual clock — no real delay; on resume it returns immediately if the wake has passed |

### Options

`{ log: "path", durability?: "fsync" | "buffered" }` — `log` is the event-log file
path (required). `durability` defaults to `"fsync"` (the log is flushed to disk).

### Crash-atomic log writes

The event log is rewritten **atomically**: the new contents are written to a sibling
temp file, fsync'd (under `"fsync"`), and then `rename`d over the target — a POSIX
`rename` is atomic at the directory level. So at every instant the log path holds
**either the previous complete log or the new complete log, never a zero-byte or
half-written file**. A crash (OOM / `SIGKILL` / power loss) mid-write therefore cannot
corrupt the log into re-executing already-completed activities; the exactly-once
activity guarantee holds across a crash *during* the persist step, not just between
steps. Under `"fsync"` the parent directory is also fsync'd after the rename so the
rename itself is durable.

> **Single-writer per log.** A given log path must be written by **one** workflow
> run/`resume` at a time (the replay model already assumes this). The temp sibling is
> pid-qualified so two unrelated processes don't clobber each other's in-flight write;
> concurrent writers to the same log within one process are not supported.

## Event-log format

The log is newline-delimited JSON, one event per line, `seq`-ordered:

```jsonc
{ "seq": 0, "kind": "ActivityCompleted", "name": "fetchUser", "argsHash": "…", "result": {…} }
{ "seq": 1, "kind": "ClockRead",  "value": 1717459200123 }
{ "seq": 2, "kind": "RandomRead", "value": 0.5734 }
{ "seq": 3, "kind": "TimerSet",   "wake": 1717459260000 }
{ "kind": "WorkflowCompleted", "result": {…} }
```

On replay, the Nth `ctx`-effect is matched to the Nth recorded event of that kind,
and the **call signature** (activity name + args hash) is asserted — a workflow-code
change that reorders effects is caught as a **non-determinism error**
(`workflow non-determinism: expected … at seq N, got …`) rather than silently
replaying a wrong value.

## Constraints (honest, like every durable-execution system)

1. **Workflow code must be deterministic** — same inputs ⇒ same control flow ⇒ same
   effect order. Loops over recorded data, conditionals on recorded values, and
   `ctx`-mediated time/RNG are fine; direct `time.now()` / `math.random()` / I/O in
   the workflow body is a violation (flagged by the `workflow-determinism` checker
   lint, and caught at runtime by the replay-mismatch detector).
2. **Only `Value`-serializable activity results persist** — returning a native
   handle / function / class from an activity is a constraint violation at record
   time. Activities return *data*; native handles live only inside the activity.
3. **Activities are at-least-once** — a crash between an activity's side effect and
   its log append re-runs the activity on resume, so activities should be idempotent
   or externally guarded.
4. **Native handles never survive a restart** — re-establish them inside the next
   activity.

## Relationship to deterministic mode

`run`/`resume` enter the engine's deterministic mode ([SP9 §3 determinism seams]):
the virtual clock and seeded RNG that back `ctx.now`/`ctx.random`/`ctx.uuid` are the
same seams `--deterministic` uses, so a workflow's clock/RNG are reproducible across
record and replay.

> **Out of scope (the one model-2b residual):** reproducible *interleaving order* of
> arbitrary concurrent in-workflow `task.spawn` fan-out. Workflows stay deterministic
> by sequencing activities (`ctx.call` is await-sequenced); parallel in-workflow
> fan-out with a reproducible join order would require an owned cooperative scheduler
> replacing tokio (not built — see the async-generators ADR).
