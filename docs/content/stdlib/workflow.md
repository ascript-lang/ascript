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

`{ log: "path", durability?: "fsync" | "group" | "buffered", groupWindowMs?: number, groupMaxEvents?: number }` —
`log` is the event-log file path (required). `durability` defaults to `"fsync"`.
See [Durability](#durability) for a full description of each mode and the loss-window
contract.

### Crash-atomic log writes (fsync and buffered modes)

Under `"fsync"` and `"buffered"`, the event log is rewritten **atomically**: the new
contents are written to a sibling temp file, optionally fsync'd, and then `rename`d
over the target — a POSIX `rename` is atomic at the directory level. So at every
instant the log path holds **either the previous complete log or the new complete log,
never a zero-byte or half-written file**. Under `"fsync"` the parent directory is also
fsync'd after the rename so the rename itself is durable.

Under `"group"` mode, events are **appended individually** as they are recorded (no
temp-rename); torn tails from a crash are repaired by prefix-truncation at the next
`resume`. See [Group mode repair](#group-mode-torn-tail-repair) below.

> **Single-writer per log.** A given log path must be written by **one** workflow
> run/`resume` at a time (the replay model already assumes this). The temp sibling is
> pid-qualified so two unrelated processes don't clobber each other's in-flight write;
> concurrent writers to the same log within one process are not supported.

## Durability

The `durability` option controls **when** events reach durable storage and **how much
work** a crash re-executes. The **default is full durability** (`"fsync"`); `"group"`
and `"buffered"` are explicit opt-ins per workflow. There is no global
`ascript.toml` default — a global knob that silently relaxes durability for code that
didn't ask is the failure mode being avoided.

### The three modes

| `durability` | write granularity | fsync policy | kill -9 mid-run | power / kernel loss |
|---|---|---|---|---|
| `"fsync"` (default) | whole-log snapshot at finish (temp+rename) | `F_FULLFSYNC` + dir-fsync per commit | loses the whole in-flight run; `resume` re-executes **all** activities | completed commits never lost |
| `"group"` (new) | per-event append at each recording call | coalesced: fsync when ≥ `groupMaxEvents` (default 128) unsynced records, or ≥ `groupWindowMs` (default 50 ms) since the oldest unsynced record | **loses nothing** — records are in the OS page cache the moment the recording call returns | loses at most the unsynced tail (bounded by the window while appending); the final tail after process exit rides the OS writeback horizon |
| `"buffered"` | whole-log snapshot at finish | none (OS-asynchronous writeback) | loses the in-flight run | recent commits may be lost (OS-dependent) |

### Choosing a mode

- **`"fsync"` (default):** use when per-commit durability is required and throughput is
  secondary. A crash mid-run costs re-executing the entire in-flight workflow, but every
  committed result is permanently safe.
- **`"group"`:** use for high-throughput pipelines where you want crash resilience
  without the per-commit `F_FULLFSYNC` overhead. Each `ctx.call` / `ctx.now()` /
  `ctx.uuid()` / `ctx.sleep()` immediately appends its event to the OS page cache —
  a kill -9 loses nothing. Power loss loses at most the unsynced tail (bounded by
  `groupWindowMs` / `groupMaxEvents` while the process is appending).
- **`"buffered"`:** use only where durability is not required. Fastest, but weakest.

### Activity at-least-once contract and idempotency

**All three modes share the same activity contract:** a crash between an activity's
side effect and its log append causes that activity to re-execute on resume. Under
`"fsync"` this happens for all activities in the in-flight run; under `"group"` only
for the single in-flight activity at crash time (because previous activities' results
are already in the OS page cache).

**Design every activity to be idempotent:**

- Use database upsert semantics rather than blind insert.
- Pass an **idempotency key** (order ID, request ID) to payment and messaging APIs.
- Guard mutations: "apply if not already applied for key X".

This is the same guidance Temporal, Restate, and every durable-execution system gives,
for the same reason: exactly-once side effects require externally-enforced idempotency,
not a different durability tier.

### Group mode options

```ascript
run(myFlow, input, {
  log: "flow.log",
  durability: "group",
  groupWindowMs: 50,   // optional: fsync deadline in ms (default 50)
  groupMaxEvents: 128, // optional: fsync after N unsynced events (default 128)
})
```

- `groupWindowMs`: positive finite number of milliseconds. Smaller = tighter power-loss
  window; larger = fewer fsyncs per unit time.
- `groupMaxEvents`: positive integer. Triggers an fsync after this many unsynced events,
  regardless of the time window.

An unknown `durability` value (e.g. `"groop"`) is a Tier-2 error that names the three
valid values.

### Group mode torn-tail repair

Appended records carry a `"crc"` field (CRC32 of the record's canonical JSON). On
resume, the log is scanned line by line; the **valid contiguous prefix** ends at the
first line that is not newline-terminated, not valid JSON, has a failing CRC, or has a
non-contiguous `seq`. The file is physically truncated to the end of the valid prefix
before parsing — replay correctness requires a contiguous event prefix. A torn tail
from a power loss is automatically repaired; the lost suffix re-executes on resume
(at-least-once, as documented above).

## Event-log format

The log is newline-delimited JSON, one event per line, `seq`-ordered:

```jsonc
{ "seq": 0, "kind": "ActivityCompleted", "name": "fetchUser", "argsHash": "…", "result": {…} }
{ "seq": 1, "kind": "ClockRead",  "value": 1717459200123 }
{ "seq": 2, "kind": "RandomRead", "value": 0.5734 }
{ "seq": 3, "kind": "BytesRead",  "bytes": [12, 240, 5, …] }
{ "seq": 4, "kind": "TimerSet",   "wake": 1717459260000 }
{ "kind": "WorkflowCompleted", "result": {…} }
```

`BytesRead` records a seeded **byte draw** — the entropy behind `ctx.uuid()`,
`uuid.v4`/`uuid.v7`, `crypto.randomBytes`, and the `crypto.hashPassword`/`bcryptHash`
salts. The *exact drawn bytes* are logged, so replay reproduces them **verbatim**
(faithful regardless of the seed), and a wrong-kind or wrong-length event at that
position surfaces a divergence — the same record/replay discipline as `RandomRead`.

> **Large draws bloat the log.** Bytes are stored verbatim as a JSON number array
> (~3 bytes per byte). A 16-byte UUID is nothing, but a large `crypto.randomBytes(n)`
> in a workflow body balloons the log (a 1 MiB draw → a ~3 MB entry; the 16 MiB max →
> ~56 MB). To draw a large key or blob, do it **inside an `activity`** — then only the
> derived result, not the raw entropy, enters the event log.

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

The seeded **byte** draws behind `uuid.v4`/`uuid.v7`, `crypto.randomBytes`, and the
`crypto.hashPassword`/`bcryptHash` salts are **fully event-sourced** (a `BytesRead`
event per draw): replay returns the exact recorded bytes and a desync is detected —
so they are replay-faithful and divergence-safe, not merely seed-reproducible. (They
are still advised toward the `ctx`/activity form by the `workflow-determinism` lint,
for the same clarity reason `math.random` is — the seam is genuinely non-deterministic
*outside* a workflow.)

> **Out of scope (the one model-2b residual):** reproducible *interleaving order* of
> arbitrary concurrent in-workflow `task.spawn` fan-out. Workflows stay deterministic
> by sequencing activities (`ctx.call` is await-sequenced); parallel in-workflow
> fan-out with a reproducible join order would require an owned cooperative scheduler
> replacing tokio (not built — see the async-generators ADR).
