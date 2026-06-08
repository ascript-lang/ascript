# AScript Debugger (DAP) + Sampling Profiler — Design (DBG)

- **Status:** Draft for review
- **Date:** 2026-06-08
- **Code:** DBG (developer-tooling milestone of the Serious Language campaign — see `goal.md`)
- **Depends on:** NUM (cleaner debug values — `int`/`float` render unambiguously in variable views, no
  `5.0`-for-`5` confusion); otherwise independent. Builds on the shipped `src/worker/` isolate model only
  for the (staged) multi-isolate follow-up.
- **Depended on by:** nothing downstream blocks on DBG; it is developer tooling, not a capability gate.
- **Engines:** **the VM only.** The debugger and profiler attach to the **production bytecode VM**
  (`src/vm/`). The tree-walker is the differential oracle and is **not** instrumented — an explicit,
  documented asymmetry, exactly like the SP3 VM-only bytecode caps and the DX `--coverage` hook. The
  debugger therefore does **not** enter the four-mode byte-identity differential (it adds no opcode, no
  `Value` kind that programs observe, and — crucially — must not perturb program output at all; §7).
- **Breaking:** no. Two additive CLI subcommands/flags (`ascript dap`, `run --inspect`, `run --profile`),
  one **optional** `.aso` section (a stripped `.aso` still runs), and a **single** `None`-gated VM
  instrumentation seam (`Vm.instrument`, §3.2 — shared with DX coverage) that is inert when nothing is
  attached.

---

## 1. Summary & motivation

AScript has a complete LSP (~24 capabilities, `src/lsp/server.rs`), a formatter, a checker, and — post-DX —
doc-gen and a parallel test runner. The one conspicuous gap in the "stand next to Java / C# / Swift / Go on
developer experience" pillar is **there is no way to stop a running program and look at it.** Grounded in
the tree:

- **No DAP server.** `src/main.rs`'s `Command` enum is `Run/Build/Repl/Fmt/Check/Test/Lsp` (+ the `pkg`
  commands). There is no `dap` subcommand parallel to `Lsp` (`src/main.rs:91`, dispatched at
  `src/main.rs:528` → `ascript::lsp::run_server().await`). A serious language ships a debugger its editors
  can drive (VS Code, Neovim DAP, IntelliJ all speak the Debug Adapter Protocol).
- **No runtime introspection seam.** The VM run loop (`Vm::run_loop`, `src/vm/run.rs:581`) is a tight
  `loop { … match op { … } }` with no per-instruction hook; the only existing observation seams are the
  `--no-specialize` kill switch (`Vm.specialize: bool`, `src/vm/run.rs:104`) and the SP3 call-depth `Cell`
  (`enter_frame_depth`, `src/vm/run.rs:299`). There is nothing that can pause execution, inspect
  `fiber.frames` / `fiber.stack` (`src/vm/fiber.rs:71`), or map a source line to a bytecode offset.
- **No line↔offset debug info.** `Chunk` carries a `spans: Vec<(usize, Span)>` table (`src/vm/chunk.rs:247`,
  binary-searched by `Chunk::span_at`, `:640`) — one **char-offset `Span`** per emitted instruction, used
  for diagnostics. There is **no line table** (lines are derived from `Span` + source on demand) and **no
  local-name table** (`FnProto` carries `params` with names, `src/vm/chunk.rs:358`, but locals introduced by
  `let` have only resolver slot indices — their names are not retained past compilation). A debugger needs
  *line → offset* (to set a breakpoint) and *slot → name* (to label variables); neither exists yet.
- **No profiler.** There is no sampling profiler and no flamegraph output. `print`-and-`time.now()`
  profiling is the only option today.

This spec adds a **Debug Adapter Protocol server** (`ascript dap`, mirroring `ascript lsp`), a **line↔
bytecode debug-info section** in the `Chunk`/`.aso` (optional, strippable), and a **sampling CPU profiler**
(`ascript run app.as --profile cpu -o flame.json`). The non-negotiable spine of the whole spec is **Gate 12:
zero steady-state cost on the production VM when nothing is attached** — analyzed in detail in §3, because
getting that wrong silently re-taxes every program in the campaign.

### Sub-deliverables

| # | Deliverable | Branch-able independently? | Gate-12 surface |
|---|---|---|---|
| G1 | Debug-info: line↔offset + slot-name tables in `Chunk`, optional `.aso` section | yes | none (static tables; not in the hot loop) |
| G2 | The zero-cost VM trap hook (breakpoint-patching + a `None`-gated control channel) | yes (lands before G3) | **the central one** — §3 |
| G3 | `ascript dap` DAP server + `run --inspect` | depends on G1+G2 | none new (drives G2's hook) |
| G4 | Sampling profiler (`run --profile cpu -o flame.json`) | yes (independent of G2/G3) | a timer-thread stack read; §6 |
| G5 | Multi-isolate debugging | **deferred follow-up** (§7) | per-isolate trap channels |

---

## 2. The model: an out-of-band debug controller over an in-band trap

A debugger is two pieces that must not contaminate each other:

1. **The DAP server** — a `tower-lsp`-style stdio JSON-RPC loop (mirroring `src/lsp/server.rs`) that speaks
   the Debug Adapter Protocol to the editor. It is the *controller*: it owns no VM state, it sends commands
   (set breakpoint, step, continue, evaluate) and receives events (stopped, exited, output).
2. **The VM trap hook** — the single point inside `Vm::run_loop` where execution can yield control to the
   debugger. It is the *observation seam*: when a breakpoint is hit or a step completes, the VM **pauses**
   (parks the fiber, hands the debugger a snapshot of `fiber.frames`), waits for a resume command, then
   continues. **When no debugger is attached this seam is inert** (§3) — the whole game.

The two communicate across a thread boundary (the DAP server runs its JSON-RPC loop; the program runs on the
`WORKER_STACK_SIZE` worker thread, `src/lib.rs:59`) via a `Send` command/event channel. The VM thread stays
`!Send` internally — the channel carries only **plain data** (breakpoint sets, step requests, serialized
variable trees), never `Value`/`Rc`/`Cc`. This is the same airlock discipline the worker subsystem uses
(`src/worker/serialize.rs`): the debug snapshot is *serialized out* of the VM heap, never a live reference.

### `ascript run app.as --inspect`

`--inspect` runs the program with the trap hook **armed and broken-on-entry**: it starts the DAP server
side-channel, parks the fiber at the first instruction of the entry `proto` (before `Op` 0 executes,
`src/lib.rs:512`), and waits for the editor to attach + send `configurationDone` before continuing. This is
the Node `--inspect-brk` / V8-inspector model: the program does not run until a debugger says go. Without
`--inspect`, `ascript run` is byte-for-byte the path it is today (the hook never arms — §3).

---

## 3. The zero-cost architecture (the central analysis — Gate 12)

> **Gate 12 (non-negotiable):** "Any always-present instrumentation … is zero-cost when disabled —
> flag-gated with a predictably-not-taken branch or a separate dispatch path, proven by a benchmark. The VM
> perf gate (bench geomean ≥ 2× the tree-walker) holds across the entire campaign; a regression is a bug to
> fix, never a tradeoff to accept."

This is the most important section of the spec. The question is precisely: **what does the production
dispatch loop pay when no debugger is attached?** The answer must be *nothing measurable*.

### 3.1 The three candidate mechanisms

**(A) A per-instruction unconditional check** — read a `debug_attached` flag (or a per-line breakpoint
predicate) at the *top of every iteration* of `run_loop`. **Rejected.** The loop body is currently a single
`code[ip]` fetch + `Op::from_u8` + `match` (`src/vm/run.rs:590–598`); the hottest ops (`Const`, `Add`, local
load/store) are a few instructions each. Adding an unconditional load+branch **per opcode** — even an
almost-always-not-taken one — measurably taxes tight numeric loops: it is exactly the few-percent overhead
the `OffsetHasher` comment (`src/vm/chunk.rs:24–53`) documents the codebase already fighting to remove from
the IC side-maps. A per-instruction debug check is the perf killer Gate 12 names; it is **out**.

**(B) A per-line check behind a predictably-not-taken branch** — check the flag only at instructions that
*begin a new source line* (a `line_start` bit the compiler sets, ~once per statement, not per opcode), and
mark the branch `#[cold]` / `unlikely`. Far cheaper than (A) (line boundaries are sparse), and it is the
classic interpreter "safepoint" placement. But it **still pays on every line**, and worse: it requires the
loop to carry per-instruction "is this a line boundary?" state, which complicates the hot path. Acceptable as
a *fallback*, but not the lowest-overhead design. **Not chosen as primary.**

**(C) Breakpoint-patching the bytecode** (the chosen primary) — when the debugger sets a breakpoint at
line L, resolve L → bytecode offset (G1's line table), and **overwrite the opcode byte at that offset** with
a dedicated `Op::Break` trap, saving the original byte in a side table. The trap opcode's handler is the
*only* place that talks to the debugger; **every other offset is unmodified**, so the loop dispatches the
original opcode with zero added work. Stepping is implemented by patching the *next* line's offset(s)
temporarily. Removing a breakpoint restores the saved byte. This is the GDB/LLDB `int3`/`brk` software-
breakpoint technique and the JVM/V8 bytecode-patching model. **The not-attached cost is exactly zero: no
flag, no branch, no extra state in the hot loop — the bytecode is identical to an unpatched run.**

### 3.2 The locked design: (C) breakpoint-patching, with a `None`-gated arm

**LOCKED.** Primary mechanism is **(C) breakpoint-patching**. The full picture:

- **Arm gate — ONE unified `Vm.instrument` seam (reconciles DX cross-cutting #6).** Both DBG (breakpoints +
  profiler) and the DX `--coverage` hook need an `Option`-gated point of execution-time observation. Rather
  than two sibling fields (`Vm.debugger` + `Vm.coverage`, each its own `None`-check), DBG and DX share a
  **single** field on `Vm` beside `Vm.specialize` (`src/vm/run.rs:104`):

  ```rust
  // src/vm/run.rs (replaces a would-be Vm.debugger / Vm.profiler / Vm.coverage trio)
  instrument: Option<Box<Instrumentation>>,   // Box: keep Vm small; the None path is the hot path
  ```
  ```rust
  struct Instrumentation {
      breakpoints: Option<DebuggerHook>,   // DBG §3.2  (stepping/profiler state, command channel)
      profiler:    Option<ProfilerHook>,   // DBG §6    (frame-name snapshot publisher)
      coverage:    Option<CoverageTable>,  // DX  §6.3  (line-hit counts)
  }
  ```
  `None` is the default and the **untouched** path — no patching, no `Op::Break`, no per-line counting, the
  loop is byte-identical to today. This is the same `None`-gated seam pattern as `Vm.specialize` and the SP9
  determinism `RefCell<Option<…>>` — a pattern this codebase trusts and proves zero-cost.

- **One predictably-not-taken check, not three (the cross-cutting #6 mandate).** Every armed sub-feature is
  reached *through* the one `self.instrument.is_some()` gate; the hot loop never grows a second or third
  `Option` discriminant load. The constructor is `Vm::with_instrument(inst)` (DBG's `--inspect`/`dap` and
  DX's `--coverage` both build the appropriate `Instrumentation` and pass it); the existing `Vm::new` leaves
  it `None`. **Reconciliation with breakpoint-patching:** the two armed mechanisms are *not* both per-op
  branches — DBG breakpoints stay **patched-byte traps** (`Op::Break`, zero hot-loop check even when armed),
  so the `instrument` gate only governs the *stepping/profiler/coverage* state that the trap handler,
  frame-push/pop sites, and line-boundary counter consult. DX's line-coverage increment is the one path that
  pays a per-line cost *when armed* (it counts at each line-start op); when `instrument == None` that path is
  unreached and the cost is zero. The §3.4 benchmark's config (2) (`instrument == None`) is therefore the
  single zero-cost proof for DBG **and** DX. (DX's own coverage-off benchmark, its DX-side Gate-12 artifact,
  measures the same not-taken gate — the two specs assert the identical not-attached path.)
- **When `Some` with `breakpoints == Some(hook)`,** the hook owns: the `Send` command/event channel to the
  DAP server, the breakpoint set (offset → saved original byte), the current step mode, and the call-stack
  depth captured at a `step over` (to know when a `step into` a callee should re-break). Setting a breakpoint
  patches one byte; clearing restores it. (The profiler and coverage sub-features are independent: a run may
  arm any subset — e.g. `--profile cpu` populates only `instrument.profiler`, `--coverage` only
  `instrument.coverage`.)
- **The trap opcode.** A new `Op::Break` is added (no operand). It is **never emitted by the compiler** — it
  only ever appears because the debugger patched a byte at runtime. Its handler in `run_loop`: (1) recover
  the original opcode from the hook's side table, (2) park the fiber and ship a `Stopped` event + a frame
  snapshot to the DAP server, (3) block on the command channel until `continue`/`step`/`next`/`stepOut`,
  (4) **execute the saved original opcode** (re-dispatch, having NOT advanced `ip` past it), then continue.
  Because the trap re-runs the displaced instruction, single-stepping and breakpoints compose correctly.
- **Why a real opcode and not a flag.** `Op::from_u8` already does an exhaustive `match` (`src/vm/run.rs:591`);
  adding one arm costs the not-attached loop nothing (the byte is never that value unless patched). The
  alternative — a flag checked before the `match` — reintroduces mechanism (A)'s per-instruction tax.
- **Stepping (`next`/`stepIn`/`stepOut`).** Implemented as *transient* breakpoints, not a per-instruction
  trace:
  - `stepIn` / `next` (step over): patch `Op::Break` at every offset that begins a *different source line*
    than the current one reachable from here — in practice, patch the next-line offset(s) in the current
    proto **plus** (for `stepIn`) the entry offset of any callee, or (for `next`) re-break only when the
    call stack returns to the current depth at a new line. The hook records the call-depth at step time
    (read from `fiber.frames.len()`, `src/vm/fiber.rs:72`) and the SP3 depth `Cell` to disambiguate.
  - `stepOut`: patch the return site — break when `fiber.frames.len()` drops below the current depth.
  - Transient step-breakpoints are cleared the moment the step completes, so they never accumulate.
- **The cold edge cases pay only when armed.** Patching interacts with `.aso` verification (`src/vm/verify.rs`)
  and with the inline-cache side maps (`field_ics`/`method_ics`, keyed by offset, `src/vm/chunk.rs:57`):
  patching a byte does **not** move any offset, so the IC maps stay valid; and the verifier runs at load
  time, before any patching, so a patched `Op::Break` is never serialized or verified. All of this is on the
  `Some` path; the `None` path is untouched.

### 3.3 Why patching beats a separate instrumented dispatch loop

A clean alternative is **a second `run_loop_debug` selected at attach time** (the "separate dispatch path"
Gate 12 explicitly blesses). It has zero not-attached cost too (the production loop is literally unchanged).
**Rejected as primary** for one reason: it **duplicates the entire ~5000-line `run_loop`** (`src/vm/run.rs`),
which is the codebase's single most correctness-critical function and the one held byte-identical to the
tree-walker. Two copies is a divergence farm — every opcode fix must land twice, and a debug-only bug would
not be caught by `vm_differential`. Breakpoint-patching adds **one** opcode arm to the **one** loop, so there
is no second implementation to keep in sync. (A duplicated loop is reconsidered only if patching proves
insufficient for some opcode — e.g. a super-instruction the debugger must split — and even then only for that
op, documented.)

### 3.4 The benchmark requirement (REQUIRED, the proof)

Gate 12 is satisfied by **measurement, not assertion**. The plan MUST include a benchmark that proves no
steady-state regression:

- **Bench:** the existing VM perf-gate corpus (the geomean-≥-2×-tree-walker set referenced in `goal.md`
  Gate 12 and `CLAUDE.md` "Accepted SP1 trade-offs") is run in three configurations and compared:
  **(1)** today's `main` (pre-DBG), **(2)** post-DBG with **nothing attached** (`Vm.instrument == None` — the
  single not-taken gate, §3.2; this is also DX's coverage-off config), **(3)** post-DBG with a debugger
  attached but **no breakpoints set** (`instrument.breakpoints == Some(empty)` — proves the armed-but-idle
  overhead, which should also be ~free since no byte is patched).
- **Pass condition:** config (2) is within noise of config (1) (the not-attached path is the production
  path — it must be a *statistical no-op*, the bar the `None`-gated seams already meet), and the VM geomean
  stays ≥ 2× the tree-walker. A regression in (2) is a **bug to fix** (a stray flag check leaked into the hot
  loop), never an accepted tradeoff. Config (3)'s overhead is reported but only gated loosely (it is the
  debug session, not production).
- **Where it lives:** a criterion bench under `benches/` (or the existing perf harness) plus a CI assertion
  that (2) ≈ (1). This bench is the spec's primary acceptance artifact.

---

## 4. The DAP server

### 4.1 CLI surface

A new `src/main.rs` subcommand, parallel to `Lsp`:

```
ascript dap                       # Debug Adapter Protocol server over stdio (editor-driven)
ascript run app.as --inspect      # run with the adapter armed, break on entry, wait for attach
```

`ascript dap` is feature-gated like `lsp`/`pkg` (a new `dap` Cargo feature, default-on) so
`--no-default-features` need not build it. It dispatches (mirroring `src/main.rs:528`) to a new
`ascript::dap::run_server().await`. `--inspect` is an additive flag on the existing `Run` command
(`src/main.rs:17`), plumbed into `run_file_on_vm_with_packages` (`src/lib.rs:464`) to construct the `Vm` with
`Vm::with_instrument(Instrumentation { breakpoints: Some(hook), .. })` instead of `Vm::new` (§3.2).

### 4.2 Adapter architecture (mirror the LSP)

The DAP server reuses the LSP server's exact shape (`src/lsp/server.rs`): a stdio JSON-RPC loop on a
multi-thread-friendly tokio runtime (DAP messages are `Send`), holding adapter state behind a
`tokio::sync::Mutex`. There is no off-the-shelf `tower-dap` as mature as `tower-lsp`; the DAP wire format is a
simple `Content-Length`-framed JSON request/response/event protocol (identical framing to LSP), so the
adapter implements the small message loop directly (the `debugserver-types` / `dap-types` crate supplies the
typed message structs behind the `dap` feature). The adapter:

- **Launches/attaches** the debuggee: on a DAP `launch` request it spawns the program on the
  `WORKER_STACK_SIZE` worker thread (the same `std::thread::Builder` path as `src/lib.rs:59`) with
  `Vm.instrument = Some(Instrumentation { breakpoints: Some(hook), .. })`, holding the `Send` channel ends.
  (`attach` to an already-running `--inspect`ed process connects to its waiting channel.)
- **Translates** DAP requests to hook commands and hook events to DAP events across the channel.

### 4.3 Capabilities (v1)

| DAP capability | v1 | Mechanism |
|---|---|---|
| `setBreakpoints` (line) | ✅ | line → offset (G1 table) → patch `Op::Break` (§3.2) |
| `setBreakpoints` (conditional) | ✅ | patched trap evaluates the condition via the existing expr evaluator (§4.5); break only if truthy |
| `setBreakpoints` (logpoint) | ✅ | trap evaluates the message template, emits a DAP `output` event, auto-continues (no stop) |
| `configurationDone` / `launch` / `attach` | ✅ | adapter lifecycle |
| `continue` / `pause` | ✅ | resume the parked fiber / arm a one-shot break at the next safepoint |
| `next` (step over) / `stepIn` / `stepOut` | ✅ | transient step-breakpoints (§3.2) |
| `stackTrace` | ✅ | snapshot of `fiber.frames` (§4.4) |
| `scopes` (Locals / Upvalues / Self) | ✅ | per-frame slot/cell/`self` enumeration (§4.4) |
| `variables` (lazy expansion of containers) | ✅ | serialized `Value` tree, lazily by `variablesReference` (§4.4) |
| `evaluate` (watch / REPL in frame) | ✅ | compile + run the expression in the paused frame's scope (§4.5) |
| `threads` | ✅ (single) | one thread = the single isolate (v1); multi-isolate is G5 (§7) |
| `exceptionInfo` / break-on-panic | ✅ | a Tier-2 `Control::Panic` raised under a debugger stops at the faulting frame before unwinding |
| `disassemble` (DAP) | ✅ (nice-to-have) | reuse `src/vm/disasm.rs` — bytecode view in the editor |
| data breakpoints / function breakpoints | ❌ v1 | deferred (§8) |

### 4.4 Stack frames, scopes & variable inspection

On a stop, the hook builds a snapshot **without holding any `RefCell`/`Cc` borrow across the channel send**
(the M17 invariant, `CLAUDE.md`; the snapshot is serialized data, copied out):

- **`stackTrace`** walks `fiber.frames` (`src/vm/fiber.rs:72`) top-down. Each `CallFrame` yields: the
  function name (from `FnProto`/the closure's debug name), the current source line (from the frame's `ip` →
  `Chunk::span_at` → line via the source), and a frame id. The call-site span lives on the frame
  (`ret_span`, `src/vm/fiber.rs:37`) for the caller line.
- **`scopes`** per frame: **Locals** (the frame's local window `stack[slot_base .. slot_base + slot_count]`,
  `src/vm/fiber.rs:4`, labeled by the **slot-name table** G1 adds — without it locals are `slot_3`),
  **Upvalues** (the frame's `cells`, `src/vm/fiber.rs:31`, captured by the closure), and **Self** (the method
  receiver / `def_class` context, `src/vm/fiber.rs:43`, when present).
- **`variables`** renders each `Value` to a DAP variable `{name, value: display, variablesReference}`. A
  scalar (`Int`/`Float`/`Bool`/`Str`/`Nil`) renders inline (NUM makes `int` vs `float` unambiguous —
  `5` is an `int`, `5.0` a `float`, no more "is this number really integral?"). A container
  (`Array`/`Object`/`Map`/`Set`/`Instance`) gets a non-zero `variablesReference` the editor expands lazily —
  one channel round-trip per expansion, so deep/cyclic graphs are paged, never eagerly walked (cycle-safe:
  the hook tracks visited `Cc`/`Rc` identities). Native resource handles (`Value::Native`) render as an
  opaque `<native #id>` (they must never be traced — `CLAUDE.md` GC invariant).

### 4.5 `evaluate` (watch & debug console)

DAP `evaluate` in a paused frame compiles the expression text through the **CST front-end + compiler**
(`src/compile/`) into a tiny throwaway chunk whose free names resolve against the paused frame's scope
(locals/upvalues/globals), runs it on the **same VM** re-entrantly (the VM already re-enters `self.run` for
method/closure calls, `src/vm/run.rs`), and serializes the result back. Side-effecting expressions *do* run
(this is a debug console, like the LSP REPL / V8 debug eval) — documented, and the debugger never injects
evaluation the user did not request, preserving §7's "stepping is observation" contract for the
*non-evaluated* run.

---

## 5. Debug info & `.aso`

### 5.1 The two new tables (in `Chunk`)

The debugger needs two maps the `Chunk` does not carry today:

1. **Line ↔ offset.** *(Grounded: there is no line table today — only char-offset spans.)*
   `Chunk.spans: Vec<(usize offset, Span)>` (`src/vm/chunk.rs:247`) maps each instruction to a **char-offset
   `Span`** (`span_at` binary-searches it, `:640`); a source *line* is not stored anywhere — it would be
   derived on demand by counting newlines in the module source up to a span's start offset. The line↔offset
   mapping `setBreakpoints` needs (line → first executable offset, and offset → line for `stackTrace`)
   therefore must be **DERIVED**, not read from an existing table:
   - Build a one-time **source line index** (a sorted `Vec<u32>` of newline char-offsets → a char-offset →
     `(line, col)` lookup by `partition_point`, the standard rope/line-index technique).
   - Combine it with `spans`: for each distinct source line, the **first** instruction offset whose span
     starts on that line is its breakpoint target. DBG materializes this as
     `line_starts: Vec<(u32 line, u32 offset)>`.

   It is a **pure function of `spans` + source**, so it can be reconstructed lazily at *attach* time when the
   source is available (the `--inspect` path always has it). Precomputing and serializing `line_starts` into
   the `.aso` debug section is what additionally keeps **`.aso`-only runs (no source on disk) debuggable** —
   the offset→line direction still needs the line numbers the debug section carries. Either way it is debug
   info, never consulted by `run_loop`.
2. **Slot → name.** *(Grounded: this table does not exist today.)* `FnProto` carries only `params` (named)
   plus `arity`/`ret`/flags (`src/vm/chunk.rs:335-363`); the per-frame `FrameInfo` the resolver produces
   carries `slot_count`/`upvalues`/`cell_slots`/`value_capture_slots` (`src/syntax/resolve/types.rs:71-82`)
   but **no slot → name map** — so a `let`/`const`/loop-var local survives compilation as a bare slot index,
   its source name dropped. **Without a name table, locals in the debugger are `slot_0`, `slot_1`.** The
   names are *recoverable*, not lost forever: the resolver records every declaration with its name and slot
   in `ResolveResult.bindings: Vec<Binding>` (`Binding { name: String, slot: u32, … }`,
   `src/syntax/resolve/types.rs:31-34`). DBG adds a **debug-only** `FnProto.local_names: Vec<(u32 slot,
   Rc<str> name)>` populated by the compiler from those `bindings` (grouped per frame), retaining what is
   otherwise discarded after resolution. It is **optional/strippable** debug info (omitted by `--strip`,
   §5.2), so absent metadata simply falls back to `slot_N` labels.

Both tables are **debug-only metadata** and **carry no runtime tax**: they are never read by `run_loop`
(they live beside the chunk, consulted only by the attached hook and by `--inspect`'s breakpoint resolution),
and a `--strip`ped chunk omits them entirely. The cost is purely a slightly larger non-stripped `.aso` and a
trivial compile-time population pass — zero on the hot path, zero in the not-attached run.

### 5.2 The optional `.aso` debug section (strippable)

`.aso` serializes `Chunk` (`src/vm/aso.rs`, `write_chunk`/`read_chunk`, `:517`/`:565`); `spans` already
round-trips (`:540`/`:593`). DBG adds an **optional** debug section after the existing chunk body:

- A **section presence flag** (one byte / bit in the header) gates a trailing `debug` block carrying
  `line_starts` + `local_names` (+ the source path, optionally the source text for `.aso`-only debugging).
- **A stripped `.aso` still runs.** `ascript build` emits the debug section by default; `ascript build
  --strip` (additive flag) omits it. A reader that finds no debug section runs the program normally and
  reports "no debug info" if a debugger attaches to it (breakpoints degrade to unavailable, like a stripped
  native binary). The run path (`read_chunk` → VM) is **unchanged** when the section is absent — the section
  is read only when a debugger is present.
- **Verification.** `src/vm/verify.rs` gains bounds checks for the new section (line/slot indices in range)
  so a malformed debug section in an untrusted `.aso` is a clean `AsoError`, not a panic — this matters for
  BIN (native binaries embed `.aso`) and is on FUZZ's `.aso`-deserializer surface.

### 5.3 `.aso` version bump — SEQUENTIAL coordination (do NOT hardcode 19)

`ASO_FORMAT_VERSION` is **currently 18** (verified: `src/vm/aso.rs:105`, `pub const ASO_FORMAT_VERSION: u32 =
18;`). Adding the optional debug section is a serialization-layout change, so it bumps the version. **Per
`REVIEW-FINDINGS-2026-06-08.md` cross-cutting #5 ("`.aso` version bumps are sequential — NUM/ADT/IFACE/DBG
each +1 by merge order — never hardcode 19"), the bump is SEQUENTIAL, not "→19."** NUM, ADT, and IFACE each
also bump it; the merge order assigns the numbers (first to land → 19, next → 20, …). **The implementer MUST
read the live `ASO_FORMAT_VERSION` constant at implementation time and bump it by one — never hardcode 19.**
If DBG lands after NUM (likely — NUM is the foundation and merges first), DBG is whatever NUM left + 1. The
plan's first task records the observed constant. Because the debug section is **additive, trailing, and
strippable** (an optional block after the chunk body — a reader that finds no section runs normally, §5.2), a
stripped `.aso` produced at one version still loads, and the bump is the only layout coupling to the merge
wave. `src/vm/verify.rs` updates in the same change (per `CLAUDE.md` ".aso versioning").

> **Coordination note for the merge wave:** DBG, NUM, ADT, IFACE all touch `write_chunk`/`read_chunk` layout.
> The later-merging spec rebases onto the earlier's `.aso` layout + version. DBG's section is **purely
> additive and trailing** (an optional block after the chunk body), so it composes cleanly with whatever
> field additions NUM/ADT/IFACE made earlier in the chunk — it does not collide with their layout, only with
> the shared version counter.

---

## 6. Sampling profiler

A **lower-effort, independent** deliverable (G4) — no breakpoints, no stepping, no DAP, just periodic stack
sampling — that is **also zero-cost when off**.

### 6.1 CLI surface

```
ascript run app.as --profile cpu -o flame.json     # sample the call stack; emit a flamegraph
```

`--profile cpu` (the only mode in v1; `--profile` reserves the option for future `alloc`/`gc` modes) arms a
**sampling profiler** for the run. `-o` chooses the output path (default `profile.json`).

### 6.2 Mechanism: a timer thread reading the call-frame stack

- A dedicated **sampler thread** wakes on a fixed interval (default ~1 ms, `--profile-hz` to tune) and reads
  the current **call-frame stack** — the function names along `fiber.frames` (`src/vm/fiber.rs:72`), top to
  bottom — recording one *stack sample* (a `Vec<frame-name>`). Samples aggregate into a call-tree
  (frame-path → hit count).
- **The cross-thread read is the only subtlety.** The VM is `!Send`; the sampler thread cannot touch
  `Rc`/`Cc`. So the profiler does **not** read the live `Value` graph — it reads only **frame identity**
  (the function names / proto pointers), which the armed profiler publishes to the sampler through a `Send`,
  lock-free seam: the VM, *when profiling is armed*, writes the current frame-name vector into a
  shared `Arc<Mutex<…>>` or (preferably) a single-writer `arc-swap`/seqlock snapshot at **frame
  push/pop only** (`fiber.frames.push` at `src/vm/run.rs:1253`; the matching decrement via
  `enter_frame_depth`/`leave_frame_depth`, `:299`/`:318`) — not per instruction. The sampler reads that
  snapshot. **When not armed, the VM writes nothing** — the publish is behind the same single `Vm.instrument`
  gate as the debugger (§3.2): `instrument.is_none()` (the production path) skips it entirely;
  `instrument.profiler.is_some()` selects the publish.
- **Why frame-push/pop, not per-instruction:** frame transitions are already the SP3 call-depth seam
  (`enter_frame_depth`/`leave_frame_depth`), so publishing the frame name there is a handful of extra writes
  *per call*, not per opcode — and only when armed. Per-instruction line attribution (for line-level flame
  graphs) is a documented follow-up; v1 is **function-level** sampling, which is the standard CPU-profiler
  granularity (async-profiler, pprof, perf default to function frames).
- **Zero-cost when off:** `Vm.instrument == None` → the push/pop sites do the same single `None`-gated
  nothing as the debugger and coverage seams (§3.2). The §3.4 benchmark covers this (a "profiler armed but
  off" config is not needed; the not-armed path is the production path the bench config (2) already proves).

### 6.3 Output: flamegraph-compatible

The aggregated call-tree is emitted as the **Brendan-Gregg collapsed-stack / `speedscope` JSON** format —
the de-facto flamegraph interchange that `speedscope.app`, `flamegraph.pl`, and most editors ingest directly.
v1 emits the speedscope JSON schema (`-o flame.json`); a `--profile-format collapsed` option emits the
folded-text format for `flamegraph.pl`. The output is a **golden-testable** artifact (a deterministic program
under a fixed sample schedule produces a stable tree — tested with a synthetic clock, mirroring SP9's
`VirtualClock`, so the profiler-output golden is not wall-clock-flaky).

---

## 7. Multi-isolate (staged, and honest about the difficulty)

A real debugger for a multi-core AScript program must attach **across worker isolates** — each isolate is its
own OS thread, its own tokio runtime + `LocalSet`, its own `Interp`/`Vm`/`Cc` heap (`src/worker/isolate.rs`,
the pool `src/worker/pool.rs`). This is **genuinely hard**, and the spec stages it honestly rather than
pretending v1 covers it.

### 7.1 v1: single-isolate

**v1 debugs the main isolate only.** A `worker fn`'s body runs on a pool isolate (`src/worker/dispatch.rs`);
in v1, breakpoints inside a `worker fn` are **reported as unverified/unbound** (the DAP `breakpoint` event's
`verified: false`), with a clear message: "debugging inside worker isolates is not yet supported." The main
isolate — where the bulk of program logic and orchestration lives — is fully debuggable. This is a real,
useful, shippable debugger (the same staging Node took: `worker_threads` debugging arrived after main-thread
debugging).

### 7.2 The follow-up: multi-isolate (G5, deferred)

Why it is hard (the honest accounting):

- **N independent VMs, N trap channels.** Each isolate needs its own `Vm.instrument` (with `breakpoints`
  armed) and its own `Send`
  command/event channel to the single DAP server. DAP models this as multiple **threads** (`threads`
  request) — each isolate is one DAP thread — which the protocol supports, but the adapter must fan out
  breakpoint sets to every isolate and multiplex N event streams.
- **Stop-the-world is genuinely the hard part.** When a breakpoint hits in isolate 2, the editor expects
  isolate 1 to **also pause** (the "all-stop" debugging model) — otherwise the program races on under you.
  But the isolates share no memory and communicate by message; there is no global pause primitive. Achieving
  all-stop means broadcasting a pause request to every isolate's trap channel and waiting for each to reach a
  safepoint — and an isolate blocked in a native I/O syscall (not at a bytecode safepoint) cannot promptly
  acknowledge. The pragmatic stage is **non-stop debugging** (only the breakpointed isolate pauses; others
  keep running) first, with all-stop as a further refinement — exactly the gdb non-stop/all-stop distinction.
- **Birth/death timing.** Isolates are demand-grown and pooled (`src/worker/pool.rs` — lazy, idle-reused);
  an isolate that does not exist yet when a breakpoint is set must inherit the breakpoint set on birth, and a
  retired isolate must detach cleanly. The adapter owns a breakpoint registry it replays onto each new
  isolate.

**Decision (LOCKED):** ship single-isolate debugging in v1; multi-isolate is a **documented, owner-noted
follow-up** (a Tier-1 "unsupported" message in the meantime, never a silent failure — Gate 6). The profiler
(§6) is *easier* to make multi-isolate (each isolate's sampler publishes into a shared aggregate keyed by
isolate id — no stop-the-world needed), so **profiler multi-isolate may land before debugger multi-isolate**;
v1 profiler is still single-isolate for parity of scope, with multi-isolate aggregation noted as the natural
next step.

---

## 8. Implementation surface & cross-cutting checklist

Per `CLAUDE.md` "Touching syntax" (mostly **N/A** — DBG adds **no grammar, no AST, no `ExprKind`/`Pattern`/
`Stmt`, no tree-sitter change**; the one new opcode is runtime-patched, never parsed) plus the DBG surfaces:

**Debug-info (G1):**
- **`src/vm/chunk.rs`:** `line_starts` table (derived from `spans` + source) + `FnProto.local_names` (slot →
  name, retained from the resolver). Both debug-only metadata, never read by `run_loop`.
- **`src/syntax/resolve/`:** retain `let`/`const`/loop-var slot → name through resolution (currently dropped)
  so the compiler can populate `local_names`.
- **`src/vm/aso.rs`:** optional trailing debug section in `write_chunk`/`read_chunk`; **bump
  `ASO_FORMAT_VERSION` by reading the current constant (sequential, NOT hardcoded 19 — §5.3)**; `--strip`
  flag on `ascript build` (`src/main.rs` `Build`). `src/vm/verify.rs`: bounds-check the new section.

**The trap hook (G2 — the zero-cost core):**
- **`src/vm/opcode.rs`:** new `Op::Break` (no operand), **never compiler-emitted**.
- **`src/vm/run.rs`:** the **single** `Vm.instrument: Option<Box<Instrumentation>>` field (beside
  `specialize`, `:104`) holding `{breakpoints, profiler, coverage}` (§3.2 — DBG owns `breakpoints`/`profiler`,
  DX owns `coverage`; coordinate the struct shape with DX so it lands once); the `Op::Break` arm in
  `run_loop` (recover original byte → park → channel → resume → re-dispatch, §3.2). The `None` path is
  byte-identical to today. `Vm::with_instrument(inst)` constructor (mirrors `Vm::with_specialize`, `:175`).
- **The `Send` channel airlock** (a new `src/dap/channel.rs` or in `src/vm/`): plain-data commands/events,
  the serialize-out snapshot of frames/variables (reuse `src/worker/serialize.rs` discipline — never send
  `Value`/`Cc`).

**DAP server (G3):**
- **New `src/dap/` module** mirroring `src/lsp/` (server loop + per-capability handlers), feature-gated by a
  new `dap` Cargo feature (default-on, like `lsp`).
- **`src/main.rs`:** the `Dap` subcommand (parallel to `Lsp`, `:91`/`:528`) + the `--inspect` flag on `Run`
  (`:17`); `--inspect` plumbed into `src/lib.rs:464` (`run_file_on_vm_with_packages`) to build the `Vm` with
  an `Instrumentation { breakpoints: Some(hook), .. }` and break-on-entry.
- **`Cargo.toml`:** the `dap` feature pulling a DAP-types crate (`dap` / `debugserver-types`); no new
  always-on dependency.

**Profiler (G4):**
- **`src/vm/run.rs`:** `instrument.profiler: Option<ProfilerHook>` (a sub-field of the single
  `Vm.instrument`, §3.2); publish the frame-name snapshot at frame push/pop only (`:1253`, `:318`),
  `None`-gated through the one `Vm.instrument` check. A sampler thread + a `Send` snapshot seam.
- **`src/profile/` (or in `src/vm/`):** sample aggregation → speedscope/collapsed output.
- **`src/main.rs`:** `--profile cpu`, `-o`, `--profile-hz`, `--profile-format` flags on `Run`.

**Docs & examples (Gate 13):**
- **`docs/content/`:** a "Debugging & profiling" guide page (DAP setup, `--inspect`, `--profile`) — **add its
  slug to the `NAV` array in `docs/assets/app.js`** (the orphan gotcha: no `NAV` entry → unreachable).
- **A VS Code `launch.json` example** (an `ascript`-type debug config: `request: launch`, `program:
  ${file}`, driving `ascript dap`) under `docs/` / `editors/`, plus a Neovim DAP snippet.
- **`README.md`** debugging/profiling mention (the general-purpose repositioning, Gate 13).
- **`examples/`:** a small program with a deliberate bug used as the debugging walkthrough + the profiler
  golden's input; an `examples/advanced/` profiled CPU-bound program. (Examples stay runnable; the debug
  metadata does not change their output — Gate 9.)

**Unchanged:** the GC, the `Interp` async model, all *existing* opcodes/`Value` kinds, the tree-sitter
grammar (no grammar change), the formatter, the checker/types, the LSP (DBG is a *parallel* server, not an
LSP change), and — when no debugger/profiler is attached — `run_loop` itself (the spine of Gate 12).

## 9. Determinism & correctness

- **Stepping is observation, not mutation.** A breakpoint/step that is **not hit** changes nothing: with no
  debugger attached the bytecode is unpatched and `Vm.instrument == None`; with a debugger attached but a given
  breakpoint not reached, that offset is patched but never executed, so program behavior is identical. The
  required test: a program's output is **byte-identical** run normally, run under `--inspect` with breakpoints
  set but auto-continued, and run under the profiler — the §7 "observation" contract, the same posture as the
  DX coverage hook ("a `--coverage` run and a normal run produce identical program output").
- **`evaluate` is the one intentional mutation seam** (§4.5) — it runs user-requested code that *can* side-
  effect, exactly like any debug console. It is never injected; the debugger evaluates only what the user
  asks. Outside `evaluate`, the debugger reads (serializes-out) and never writes the program heap.
- **No four-mode differential impact.** DBG adds `Op::Break` (never compiler-emitted, never in a normal
  chunk), debug-only metadata tables, and `None`-gated hooks — `vm_differential` is unchanged (the
  tree-walker is not instrumented; the VM's not-attached path is byte-identical to today). A test asserts the
  differential corpus output is unchanged with the DBG opcode/field additions present.
- **Profiler determinism** (§6.3): output is deterministic under a fixed (virtual) sample schedule; the
  golden uses a synthetic clock so it is not wall-clock-flaky.
- **No `await` across a borrow** (Gate 4): the trap hook parks the fiber and communicates over the `Send`
  channel **without holding any `RefCell`/`Cc` borrow** across the channel wait — the snapshot is built and
  serialized first, then the borrow is dropped, then the thread blocks on the channel.

## 10. Testing

- **The zero-cost benchmark (REQUIRED — §3.4):** the VM perf-gate corpus in configs (1) pre-DBG, (2) post-DBG
  not-attached, (3) post-DBG attached-no-breakpoints; assert (2) ≈ (1) within noise and geomean ≥ 2× the
  tree-walker. **This is the spec's primary acceptance gate** — a regression in (2) fails the merge.
- **Gate 10 = DAP protocol + breakpoint/step/inspect + profiler-golden tests (happy + edge).** The next four
  bullets (protocol envelope, breakpoint/step correctness, inspection correctness, profiler golden) together
  ARE Gate 10: each is exercised on both a happy path and at least one edge case, and all live in
  `tests/dap.rs` / the profiler golden test.
- **DAP protocol tests** (`tests/dap.rs`, spawning the built binary like `tests/lsp.rs`): drive the adapter
  over stdio with scripted DAP requests (`initialize` → `launch` → `setBreakpoints` → `configurationDone` →
  expect `stopped` → `stackTrace`/`scopes`/`variables` → `continue` → `terminated`). Assert the protocol
  envelope and payloads.
- **Breakpoint / step correctness (happy + edge):** breakpoint hits at the right line; conditional
  breakpoint breaks only when truthy; logpoint emits output and does not stop; `next` steps over a call;
  `stepIn` enters; `stepOut` returns; break-on-panic stops at the faulting frame. **Edge:** breakpoint on a
  blank/comment line (binds to the next executable offset or reports unbound); breakpoint in dead/uncalled
  code (never hit); breakpoint inside a `worker fn` (reported unverified, v1 — §7); two breakpoints same
  line; clearing all breakpoints restores every patched byte.
- **Inspection correctness:** locals labeled by name (G1 slot-name table); upvalues from `cells`; `self` for
  a method frame; lazy container expansion; a **cyclic object** expands without infinite loop (the visited-set
  cycle guard); a `Value::Native` renders opaque. NUM edge: an `int` and a `float` of the same numeric value
  render distinguishably.
- **`.aso` debug section (happy + edge):** build-with-debug round-trips line/slot tables; **`--strip` omits
  the section and the stripped `.aso` still runs**; a debugger attaching to a stripped `.aso` reports "no
  debug info" gracefully; a malformed debug section is a clean `AsoError` (FUZZ surface), not a panic; the
  version bump matches `ASO_FORMAT_VERSION + 1` of whatever landed before (§5.3).
- **Profiler:** a deterministic CPU-bound program under a synthetic sample clock yields a **golden**
  speedscope tree; the collapsed-text format golden; `--profile` output leaves program stdout byte-identical
  to a non-profiled run (observation-only); empty/short program produces a valid (possibly trivial) profile.
- **Examples (Gate 9) — a VS Code launch-config + a profiled sample.** Gate 9 ships two concrete, runnable
  artifacts (§8 "Docs & examples"): **(a)** a **VS Code `launch.json`** (an `ascript`-type debug config:
  `request: launch`, `program: ${file}`, driving `ascript dap`) plus the matching debugging-walkthrough
  `examples/` program — the example runs with output unchanged with/without `--inspect`; **(b)** a **profiled
  sample** — an `examples/advanced/` CPU-bound program run under `--profile cpu`, whose golden speedscope
  profile is stable and whose stdout is byte-identical to a non-profiled run. Both examples stay
  four-mode-clean and fmt-idempotent (no grammar change, so fmt is trivially unaffected).
- **Gates:** clippy clean in **both** feature configs (with and without the new `dap` feature, and
  `--no-default-features`); `cargo test` + `--no-default-features` green; `examples/**` still emits **zero**
  `type-*` diagnostics (DBG adds no inference); `vm_differential` unchanged.

## 11. Scope & rejected alternatives

**In scope:** `ascript dap` (DAP over stdio, mirroring `ascript lsp`) with line/conditional/log breakpoints,
step over/into/out, continue/pause, stack/scope/variable inspection, watch/console `evaluate`, break-on-panic;
`run --inspect` (break-on-entry, wait-for-attach); the line↔offset + slot-name debug-info tables and the
optional, strippable `.aso` debug section (sequential version bump); the sampling CPU profiler (`run --profile
cpu`) with speedscope/collapsed flamegraph output; the **zero-cost benchmark** proving no steady-state
regression. **Single-isolate** in v1.

**Out of scope / deferred:**
- **Multi-isolate debugging** (G5) — documented follow-up; v1 reports worker breakpoints as unverified (§7).
  Hard part is stop-the-world across share-nothing isolates; non-stop-first, then all-stop.
- **Line-level (vs function-level) profiling** — v1 samples function frames; per-line attribution later.
- **`alloc`/`gc`/`off-CPU` profiling modes** — `--profile` reserves the option; v1 is `cpu` only.
- **Data breakpoints / function breakpoints / hot-reload (edit-and-continue)** — DAP optional capabilities,
  later.
- **Time-travel / reverse debugging** — the SP9 record/replay seam could enable it eventually; explicitly out
  of v1 (and gated on determinism work, not scheduled).

**Rejected:**
- **A per-instruction unconditional debug check** (mechanism A, §3.1) — the perf killer Gate 12 names; taxes
  every opcode even when detached. **The central rejection of this spec.**
- **A per-line flag check in the hot loop as the *primary* mechanism** (mechanism B) — cheaper than A but
  still pays on every line and complicates the loop; kept only as a documented fallback if patching proves
  insufficient for some op (§3.2).
- **A fully duplicated `run_loop_debug`** (the separate-dispatch-path option, §3.3) — zero not-attached cost
  but duplicates the ~5000-line correctness-critical loop, a divergence farm the `vm_differential` gate would
  not cover. Breakpoint-patching adds one arm to the one loop instead.
- **Instrumenting the tree-walker** — it is the oracle; the production debugger attaches to the production
  engine (the VM). Documented asymmetry, like SP3 caps and DX coverage.
- **gdb/native-debugger integration / DWARF emission** — AScript is not compiled to native machine code (the
  VM interprets bytecode; BIN bundles the *runtime* + `.aso`, not native codegen), so there is no native
  frame for gdb to inspect. The DAP server is the right abstraction; a future JIT could revisit native
  unwinding, out of scope here.
- **A bespoke debug protocol** — DAP is the universal standard (VS Code, Neovim, IntelliJ, Emacs DAP);
  inventing our own would orphan every editor. Mirror DAP exactly, as we mirror LSP.

## 12. Grounding (verified sources)

- **Debug Adapter Protocol** (Microsoft DAP spec): the request/response/event model, `setBreakpoints`,
  `stackTrace`/`scopes`/`variables` with lazy `variablesReference`, `next`/`stepIn`/`stepOut`, `threads`,
  `evaluate`, `Content-Length`-framed JSON — the wire format DBG implements (same framing as LSP, mirrored
  from `src/lsp/server.rs`).
- **V8 Inspector / Node `--inspect` / `--inspect-brk`** — the break-on-entry-and-wait-for-attach model
  (`run --inspect`, §2) and the debug-console `evaluate` posture (§4.5).
- **Software-breakpoint patching** — GDB/LLDB `int3`/`brk` trap-byte technique; the JVM (`Breakpoint`
  bytecode replacement) and V8 (debug-bytecode) bytecode-patching model. The zero-not-attached-cost basis of
  §3.2 — the bytecode is identical to an unpatched run; only set offsets trap.
- **rust-analyzer / CodeLLDB** — the editor-side DAP integration shape (a `launch.json` `type` driving an
  adapter binary), the `launch.json` example in §8.
- **Sampling profilers / flamegraphs** — async-profiler and Linux `perf` (timer-thread call-stack sampling,
  function-frame granularity), Brendan Gregg's collapsed-stack format + `flamegraph.pl`, and the
  `speedscope` JSON schema (the §6.3 output). pprof's call-tree aggregation model.
- **In-tree grounding:** the `Vm.specialize` `--no-specialize` kill switch (`src/vm/run.rs:104`,
  `with_specialize` `:175`) and the SP9 determinism `RefCell<Option<…>>` cell as the **`None`-gated
  zero-cost-when-off pattern** DBG reuses for the single `Vm.instrument` seam (§3.2, shared with DX
  coverage); `Vm::run_loop` (`src/vm/run.rs:581`)
  the single dispatch loop a new opcode arm slots into; `Fiber`/`CallFrame` (`src/vm/fiber.rs:20`/`:71`,
  `frames`/`stack`/`cells`/`def_class`/`slot_base`) the live state the snapshot reads; `Chunk.spans` +
  `span_at` (`src/vm/chunk.rs:247`/`:640`) the existing offset↔span table the line index derives from;
  `ASO_FORMAT_VERSION = 18` (`src/vm/aso.rs:105`) + `write_chunk`/`read_chunk` (`:517`/`:565`) the serializer
  the optional debug section extends; `src/lsp/server.rs` the stdio JSON-RPC server `src/dap/` mirrors;
  `src/worker/` (isolate/pool/serialize) the multi-isolate follow-up's substrate and the snapshot airlock
  discipline.
```
