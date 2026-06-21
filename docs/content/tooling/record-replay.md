# Record & replay

Record a program's non-deterministic inputs and effectful results into a portable **trace**
file, then re-execute the program byte-for-byte from that trace with **no real I/O**. A
production failure or a flaky CI run becomes a deterministic local repro you can hand to a
teammate, attach to a bug report, or carry onto a plane — and debug with **time travel** (step
backwards through the recorded run).

All the hard plumbing already exists and is battle-tested: the determinism seams (virtual
clock, seeded RNG, FFI replay), the structured-clone airlock serializer, and the
event-sourced mismatch detector that powers durable workflows. Record/replay is the
integration that turns them into a headline feature.

```bash
ascript run --record run.astrc program.as      # capture a trace
ascript run --replay run.astrc program.as      # re-run from it, no real I/O
```

> Both flags are VM- and tree-walker-shared: the determinism seams live on the shared
> interpreter, so a trace recorded on one engine replays byte-identically on the other
> (and on `.aso`). See [the CLI reference](../cli) for the full flag table.

## The deterministic-mode contract

`--record` is **not** a passive observation — it is a deterministic-mode run. Entering record
(or replay) activates the existing determinism seams, so a recorded run differs from a plain
run in pacing and random values (both still valid). Under a trace:

- the **wall and monotonic clock** become a virtual clock — no real time is read;
- **`time.sleep` does not sleep** — it advances the virtual clock instantly;
- **`math.random`, `uuid.v4`/`v7`, `crypto.randomBytes`** (and password salts) draw from a
  seeded PRNG — pin the seed with `--seed N` (the default is OS entropy, stored in the trace
  header so replay is exact).

This contract is *why replay is fast*: replay performs no network or disk I/O, sleeps are
instant, and recorded results return inline. A recorded 60-second integration run replays in
however long the pure compute takes.

## What a trace is

A trace is a new, separately-versioned binary container (magic `ASTRC`, a sibling discipline
to `.aso` — versioned, length-prefixed, CRC-checksummed, and **hostile-safe** to read). It
carries:

- a **header**: the seed, the start time, the program path, the program's source SHA-256, the
  recorded `argv`, and (for test traces) the test name and filter;
- the ordered **event stream**: every seamed draw (clock/RNG/UUID) and every recorded
  effectful result, each pinned by a signature hash of the call.

Effect results are encoded with the worker **airlock serializer**, not JSON, so the exact
`Value` survives — including the `int`/`float`/`decimal` distinction (a JSON-coded `5` vs
`5.0` would be an observable divergence at replay, since `int/int` truncates and float
printing always shows a decimal).

The trace is flushed at program end **including on panic and `exit(n)`** — a failed run is
precisely the one you want to replay. The reader bounds-checks every length and rejects a bad
CRC, a truncated record, an unknown record kind, or a newer format version as a clean error
(never a panic or unbounded allocation); a fuzz target exercises this against arbitrary bytes.

On `--replay`, the trace is verified before execution: the format version, the header CRC, and
then the **source digest** — replaying a `.as` whose source changed since recording is a clean
error (`trace was recorded for a different program; re-record`). The `argv` is taken from the
trace by default; explicitly-passed args that differ are the same clean error. (A `.aso`
replays without the source-digest check.)

## What replays, what's refused, what's pure

Every standard-library function is classified at the dispatch chokepoint. A completeness test
forces every module into exactly one class, so a newly-added module fails the build until it
is classified — there is no silent gap.

| What you call | Under `--record` / `--replay` |
|---|---|
| `std/math`, `std/string`, `std/json`, collections, `std/intl`, `std/stream`, `std/sync`, `std/log`, `std/compress`, `std/encoding`, `std/crypto` hashes, pure `std/jwt`/`std/email` builders | **Runs for real both times** — pure / in-memory (no event recorded). |
| `math.random`, `uuid.v4`/`v7`, `crypto` random + salts, `time.now`/`monotonic`/`sleep`, `date.now`, `ffi.call` | **Seamed** — recorded once, replayed from the seed/trace with no real RNG / clock / sleep. |
| `fs.read`/`write`/…, `env.get`/`set`, `os.*`, `io` stdin, `net.lookup`, `process.run`, buffered `http.get`/`post`/…, `workflow.run`/`resume`, `archive.tarExtractTo`/`zipExtractTo`/`tarCreateFromDir` | **Recorded** — the result is captured at record; replay returns it with **no real I/O** (delete the file or change the env between runs and replay still matches). |
| sockets / servers / WebSockets / SSE / streaming HTTP bodies, `process.spawn`, sqlite / postgres / redis, tui, ai, telemetry, docker, blob, oauth, `jwt.jwks`, `email.send`/`connect`, `time.interval`/`debounce`/`throttle`, `caps.drop`/`dropAll`, workers / `task.pmap`/`preduce` | **Refused** — a **loud error at RECORD** (no determinism seam in v1). |

Refusing at record (not just at replay) is the key invariant: a trace that records
successfully is replayable **by construction** — you never discover non-replayability at
replay time. Streaming, servers, and workers are recorded v2 items (below).

HTTP gets a minimal handle virtualization: a buffered `http.get`/`post`/… response is a native
handle, so record assigns it a trace-scoped id and captures both the handle fields
(`status`/`ok`/`url`/`headers`) and each subsequent `resp.text()`/`json()`/`bytes()` accessor
result; replay mints a virtual handle that serves those from the trace. The `{stream: true}`
reader, `http.sse`, `http.cancelToken`, and WebSockets are refused at record (v2).

## Recording tests

```bash
ascript test --record file.as              # auto-save a trace for each FAILED test
ascript test --replay .ascript-traces/file__a_test.trace
```

`ascript test --record` runs each test file under one deterministic Record context and
auto-saves a per-test trace **only for a failed test** (a fully-green file writes nothing).
Traces land under **`.ascript-traces/`** in the working directory (project-local, like a
snapshot directory) named `<file_stem>__<test-name-slug>.trace`. After the tally, each saved
trace prints a `trace saved:` hint with the exact replay command.

> **Gitignore `.ascript-traces/`.** Traces are local repro artifacts to attach to a bug
> report or CI run, not source.

`ascript test --replay <trace>` re-runs module load + exactly that one test under strict
replay — every effect returns its recorded value, so you can replay a failed test after the
fixture or network is gone. Pin the seed with `--seed N`; `--frozen-time` and `--coverage`
compose, but `--parallel`, `--watch`, and combining `--record` with `--replay` are clean
errors in v1.

**The sibling-state caveat (documented and detected):** a sliced per-test replay re-runs
module load plus one test. A test that depends on a *sibling* test's seam effects diverges
loudly at replay — which is itself a finding (the test is order-dependent). A changed test
file replays with a printed warning rather than a hard error, because editing the test or code
between replays is the normal debugging loop.

## Replay debugging — time travel

The flagship. Record a failing run, then debug the trace with **backward stepping**:

```bash
ascript dap --replay crash.astrc                       # editor-driven
ascript run --inspect --replay crash.astrc app.as      # run-path equivalent
```

The debuggee runs under the strict Replay context — no real I/O, every HTTP response, `fs`
read, clock value, and random draw exactly as recorded — so any number of re-executions reach
byte-identical states. Because the run is fully deterministic, the adapter advertises the DAP
`supportsStepBack` capability (only when a trace is present) and implements both:

| Command | Lands on |
|---|---|
| `stepBack` | the **previous stop** (breakpoint / step boundary) |
| `reverseContinue` | the previous **breakpoint** stop (or the entry stop if none) |

A backward step is **deterministic re-execution** (the [rr](https://rr-project.org/) model — no
checkpointing, no VM-state capture): the adapter tears down the debuggee, respawns it on the
same trace, and re-runs the program prefix to the target stop, replaying the recorded
navigation log and absorbing the intermediate stops. Set a breakpoint at the panic, run to it,
then **step backwards** to the corruption point — with every effect pinned.

**Honest cost:** one re-execution of the program prefix per backward step (O(stops × prefix)).
Because replay does no I/O and sleeps are virtual, the prefix re-executes at full VM speed —
interactive-fast for the programs a debugger session handles, and the model rr validates in
production. Periodic checkpointing to make backsteps O(1) is a recorded v2.

`evaluate` works for pure-value inspection at any stop, but an expression that would call a
recorded function (e.g. `time.now()` / `fs.read(…)`) is refused with a clean message — running
it would consume a trace event and desync the replay. See
[Debugging & profiling → Replay debugging](debugging-profiling) for the DAP session details.

## When replay diverges

Strict replay never falls through to live I/O. Trace exhaustion or any signature mismatch is a
loud, indexed error — never a silently wrong value:

```text
replay divergence at event 412 of 1093 (trace .ascript-traces/orders__x.trace):
  expected: fs.read("config.toml")  [recorded args#9f31c2]
  got:      fs.read("config.json")  [args#0b77ee]
the program's effect order differs from the recording — re-record, or check for
unpinned nondeterminism (task interleaving, sibling-test state)
```

A divergence means the program's effect order changed since recording — either you edited the
code, or something the recording could not pin moved.

**The concurrency residual, stated honestly:** task interleaving is **not** pinned — AScript
does not replace tokio's scheduler (deterministic task scheduling is an architectural
non-goal). A single-task program replays exactly. A concurrent program whose *seam-event order*
depends on real I/O completion order may diverge at replay — and is **detected loudly by the
signature check**, never silently wrong. Recorded calls complete inline at replay, so
replay-side ordering follows program order; it is the record side that can capture a race.
Tagging events with task identity to pin interleavings is a recorded v2.

## Try it

Two runnable examples ship with the toolchain — see [Examples](../examples):

- **`examples/record_replay.as`** — an intro tour of the seamed (clock/RNG/UUID) and recorded
  (`fs`/`env`) effects, with the deterministic-mode contract explained inline.
- **`examples/advanced/replay_repro.as`** — the canonical "record a failure, replay it
  offline" pipeline (`fs` + `process.run`), fully error-handled, written as the trace-it /
  replay-it / step-back-through-it workflow.

```bash
ascript run --record run.astrc examples/record_replay.as
ascript run --replay run.astrc examples/record_replay.as   # identical output, no real I/O
```

## v2 (recorded, not silent)

- **Streaming / server / worker recording** — streaming HTTP bodies, SSE, WebSockets, sockets,
  `process.spawn`, server accept loops, and per-isolate trace files with spawn-bound child
  traces (workers are refused at record in v1).
- **Replay checkpointing** — periodic snapshots to make backward stepping O(1) instead of
  re-executing the prefix.
- **Task-identity-tagged events** — pinning concurrent interleavings beyond the current
  loud-divergence detection.
- **A first-class `--deterministic --seed N` alias**, plus the `--inspect --record`,
  `--profile --record`, and `--watch --record` matrices (each refused cleanly in v1).
