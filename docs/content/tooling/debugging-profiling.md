# Debugging & profiling

AScript ships a source-level debugger (a [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/)
server) and a CPU sampling profiler, both built into the `ascript` binary. They hang
off a single, **zero-cost-when-off** instrumentation seam in the bytecode VM: a normal
`ascript run` carries no debugger or profiler overhead — the dispatch loop is
byte-for-byte identical to a build without them (verified by the perf gate).

> Both tools are VM-only (the production engine). They are unaffected by, and do not
> affect, the `--tree-walker` differential oracle.

## Debugging

### Quick start

```bash
ascript run --inspect path/to/program.as
```

`--inspect` starts a DAP server on stdio for that program instead of running it
normally. An editor's DAP client connects to the process and drives breakpoints,
stepping, and inspection. The program **stops at entry**; program output is delivered
to the editor as DAP `output` events (so it never collides with the protocol stream).

There are two entry points:

| Command | Use |
|---|---|
| `ascript run --inspect <file>` | The program is pre-set from the CLI. Capability flags (`--sandbox`, `--deny …`) are honored — a debugged program is sandboxed exactly like a normal run. |
| `ascript dap` | A bare DAP server; the program path comes from the editor's `launch` request. Takes no capability sandbox flags — if you need a sandboxed debug session, use `ascript run --inspect --sandbox <file>` instead. |

### VS Code

Add a `.vscode/launch.json` (the AScript DAP type is a thin adapter over the binary):

```json
{
  "version": "0.2.0",
  "configurations": [
    {
      "type": "ascript",
      "request": "launch",
      "name": "Debug current AScript file",
      "program": "${file}",
      "cwd": "${workspaceFolder}"
    }
  ]
}
```

and register the adapter (in an extension or your settings) as
`ascript dap` over stdio — the same shape as the LSP registration.

### What works

| Capability | Notes |
|---|---|
| Breakpoints (`setBreakpoints`) | Line breakpoints. A line with no executable instruction binds to the next one, or reports unverified. The real verdict arrives as a `breakpoint` event. |
| Stop on entry | The first stop is `reason: "entry"`; configure breakpoints, then `configurationDone` resumes. |
| `continue` | Resume to the next breakpoint. |
| `stackTrace` / `scopes` / `variables` | The paused call stack, each frame's `Locals` scope, and the rendered local values — answered from a plain-data snapshot taken at the stop. |
| `evaluate` | Evaluate an expression (Watch / Debug Console / hover) against the paused frame's locals + module globals. **Side-effecting expressions do run** — like the V8 debug console — but only when *you* request them. |
| `disconnect` / `terminate` | Resume the program to completion and end the session. |
| Re-`launch` | A `launch` while a session is already live cleanly reaps the old session (resumes it, joins its threads) and resets the session state, so the re-launch behaves like a fresh session with no stale frames or zombie threads. |

### Replay debugging (time travel)

Record a run, then debug the recorded trace with **backward stepping** — the campaign's
flagship. Capture a trace with `ascript run --record crash.trace app.as` (or let
`ascript test --record` capture a failing test), then:

```text
ascript dap --replay crash.trace          # editor-driven
ascript run --inspect --replay crash.trace app.as   # run-path equivalent
```

The debuggee runs under the strict **Replay** context — every HTTP response, `fs` read,
clock value, and random draw is exactly as it was recorded, with **no real I/O**. Because
the run is fully deterministic, the adapter advertises the DAP `supportsStepBack`
capability and implements both `stepBack` and `reverseContinue`:

| Command | Lands on |
|---|---|
| `stepBack` | The **previous stop** (breakpoint / step boundary). |
| `reverseContinue` | The previous **breakpoint** stop (or the entry stop if none). |

A backward step is **deterministic re-execution** (the rr model — no checkpointing, no VM
state capture): the adapter tears down the debuggee, respawns it on the same trace, and
re-runs the program prefix to the target stop, replaying the navigation log and absorbing
the intermediate stops. Set a breakpoint at the panic, run to it, then **step backwards**
to the corruption point — with every effect pinned. The honest cost is one re-execution
per backward step (O(stops × prefix)); because replay does no I/O and sleeps are virtual,
the prefix re-executes at full VM speed (interactive-fast for the programs a debugger
session handles).

`evaluate` works for pure-value inspection at any stop, but an expression that would call a
recorded function (e.g. `time.now()` / `fs.read(…)`) is **refused** with a clean message —
running it would consume a trace event and desync the replay. A `stepBack` while a
re-execution is already in flight is refused (`time travel in progress`); a `stepBack` at
the entry stop is a clean error (nowhere to go). A non-replay `ascript dap` /
`run --inspect` session is byte-for-byte unchanged — `supportsStepBack` is absent and the
time-travel paths are inert.

### v1 limitations

- **Stepping** (`next` / `stepIn` / `stepOut`) currently resumes to the next breakpoint
  rather than single source line (transient line-stepping is a follow-up). The commands
  are accepted and honest about this.
- **stepBack granularity:** a backward step lands on the previous *stop*
  (breakpoint/step boundary), not the previous *instruction*. Periodic checkpointing to
  make backsteps O(1) is the recorded v2.
- A breakpoint inside a function called multiple times traps at most once per
  `setBreakpoints` (the documented trap-once trade-off); time-travel re-execution re-applies
  the breakpoint set on each respawn, so the navigation log replays correctly regardless.
- Conditional breakpoints and logpoints reuse the same expression evaluator and are a
  documented follow-up.
- A breakpoint inside a `worker fn` (a separate isolate) is not yet supported.
- Replay debugging is single-isolate: a trace recorded under workers refuses at record
  time (shared-nothing isolates have no trace identity in v1).

## Profiling

```bash
ascript run --profile cpu -o flame.json path/to/program.as
```

A statistical CPU sampling profiler. It publishes the current call-stack at frame
push/pop only (never per instruction), a sampler thread aggregates the samples into a
function-level call tree, and the result is written out. The program's own output is
**byte-identical** to a non-profiled run — profiling is observation-only.

| Flag | Meaning |
|---|---|
| `--profile cpu` | Enable CPU sampling (the only mode in v1). |
| `-o <file>` | Output path (default `profile.json` for `speedscope`, `profile.txt` for `collapsed`). |
| `--profile-hz <N>` | Sample rate in Hz (default 1000, i.e. ~1 ms). |
| `--profile-format <fmt>` | `speedscope` (default) or `collapsed` — see below. |

`--profile` is only supported for `.as` files on the bytecode VM. It is not available for `.aso`
compiled programs or with the `--tree-walker` engine.

### Output formats

- **`speedscope`** — JSON you can open at [speedscope.app](https://www.speedscope.app/)
  for an interactive flame graph. Default output path: `profile.json`.
- **`collapsed`** — Brendan-Gregg folded stacks (`a;b;c <count>` per line), the input
  format for [FlameGraph](https://github.com/brendangregg/FlameGraph) and many other
  tools. Default output path: `profile.txt`.
- **`deterministic-speedscope`** / **`deterministic-collapsed`** — the same formats but
  using an inline, call-structure-driven clock instead of the wall-clock sampler thread.
  Golden-stable (no timing variance between runs), used for tests and snapshot assertions.

```bash
# Folded stacks for a flame graph:
ascript run --profile cpu --profile-format collapsed -o out.folded program.as
```

v1 is **function-level** (per-line attribution is a follow-up).

## Compiled programs

`ascript build` embeds an optional, strippable debug section (the module source plus
per-function line and variable tables) so a debugger attached to a compiled `.aso` still
shows source lines and locals:

```bash
ascript build program.as              # debug info INCLUDED (default)
ascript build --strip program.as      # smaller .aso, no debug info
```

`ascript run --inspect program.aso` debugs the compiled artifact directly. With the
debug section present, breakpoints and the call stack resolve against the embedded source;
attached to a `--strip`ped `.aso` the debugger degrades gracefully — source lines are
simply unavailable (it never guesses).

## Zero-cost guarantee

The debugger and profiler are reached only when explicitly attached. With neither armed
(`ascript run`), the VM's hot path is identical to a build without the tooling — the
acceptance benchmark requires the not-attached path to be a statistical no-op versus the
baseline, and an *attached-but-idle* debugger to stay within timing noise. A regression
there is treated as a bug, never an accepted trade-off.
