# DBG — DAP Debugger + Sampling Profiler — Implementation Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; the reviewer runs the commands and probes edges). Steps use `- [ ]`. Format mirrors the NUM
> foundation plan (`superpowers/plans/2026-06-08-numeric-model.md`).

**Spec:** `superpowers/specs/2026-06-08-debugger-profiler-design.md`. **Branch:** `feat/debugger-profiler`
off `main`. **Depends on:** NUM merged (`Value::Int`/`Float` split — variable views render `5` vs `5.0`
unambiguously; otherwise DBG is independent). **Breaking:** no (two additive CLI subcommands/flags, one
*optional/strippable* `.aso` section, one `None`-gated VM seam).

**Architecture:** the spine is **Gate 12 zero-cost-when-off**. A *single* `Vm.instrument:
Option<Box<Instrumentation>>` field (beside `specialize`, `src/vm/run.rs:104`) carries
`{breakpoints, profiler, coverage}` — the `None` default is the byte-identical-to-today hot path; DX's
`--coverage` shares this field (goal-brief reconciliation #6 — whichever of DBG/DX merges first introduces
it). Breakpoints are **bytecode-patching**: overwrite the op byte at a resolved offset with a new
`Op::Break` (never compiler-emitted), original saved in a side table; patched offsets trap with **no
hot-loop check even when armed**. The DAP server (`src/dap/`) mirrors the LSP stdio JSON-RPC loop
(`src/lsp/server.rs`) and talks to the VM thread over a `Send` plain-data channel (worker-airlock
discipline). Debug info — a `line_starts` table derived from `Chunk.spans` + a `FnProto.local_names`
table populated from the resolver's `bindings` — lives in an *optional, strippable* trailing `.aso`
section (sequential version bump). The sampling profiler reads frame names published at push/pop only,
behind the same single gate. **v1 is single-isolate**; worker-fn breakpoints report `verified: false`.

**Tech stack:** Rust; VM only (the tree-walker is NOT instrumented — documented asymmetry like SP3 caps
and DX coverage); `src/vm/{opcode,run,chunk,aso,verify,disasm}.rs`; `src/syntax/resolve/`; `src/compile/`;
new `src/dap/`; new `src/profile/`; `src/main.rs`; `src/lib.rs`; a new `dap` Cargo feature (default-on,
like `lsp`).

---

## Shared API Contract (pinned to current code)
**Existing (verified):**
- `Vm` struct + `specialize: bool` field `src/vm/run.rs:104`; `Vm::new` `:161`, `Vm::new_generic` `:169`,
  `Vm::with_specialize` `:175` (the `None`-gated-seam construction pattern to mirror).
- `Vm::run_loop` `src/vm/run.rs:581` — the single dispatch loop; per-iteration `byte = …code[fault_ip]`
  `:590`, `Op::from_u8(byte)` `:591`, `ip = operand_at + op.operand_width()` `:596`, `match op { … }`
  `:598`. Frame push site `fiber.frames.push` `:1253` (also `:3459`); `enter_frame_depth` `:299` /
  `leave_frame_depth` `:318` (the SP3 call-depth seam — the profiler's push/pop publish point).
- `Op` enum `src/vm/opcode.rs:29`; `Op::from_u8` `:501`; `operand_width` `:623`; `ALL` `:720` (round-trip
  test `:819`). New opcode adds an enum variant + a `from_u8` arm + an `ALL` entry + `operand_width` (0).
- `Fiber { frames, stack, state }` `src/vm/fiber.rs:71`; `CallFrame { closure, ip, slot_base, cells,
  ret_span, def_class, argc }` `:20` (the live state a stop snapshot reads — `slot_base`+`slot_count` for
  the local window, `cells` for upvalues, `def_class`/`self` for the receiver, `ret_span` `:37` for the
  caller line).
- `Chunk.spans: Vec<(usize, Span)>` `src/vm/chunk.rs:247` (char-offset, sorted); `Chunk::span_at` `:640`
  (binary search); `slot_count` `:259`, `cell_slots` `:257`, `source: RefCell<Option<Rc<SourceInfo>>>`
  `:307`. `FnProto` `:335` carries only `params` `:358` (no `local_names`). `read_u16` `:553`.
- Resolver: `Binding { name, kind, slot, … }` `src/syntax/resolve/types.rs:31`; `FrameInfo {slot_count,
  upvalues, cell_slots, value_capture_slots}` `:71`; `ResolveResult { frames: HashMap<…,FrameInfo>,
  bindings: Vec<Binding> }` `:91`/`:96`. The compiler already iterates `self.resolved.bindings`
  (`src/compile/mod.rs:969`, `:1603`, …) — the population source for `local_names`.
- `.aso`: `ASO_FORMAT_VERSION: u32 = 18` `src/vm/aso.rs:105` (**read live + 1, never hardcode 19 — §5.3**);
  `write_chunk`/`read_chunk` `:517`/`:565` (spans round-trip `:540`/`:593`). `verify` `src/vm/verify.rs:331`
  / `verify_chunk` `:341`.
- CLI: `Command` enum `src/main.rs:15` (`Run` `:17`, `Build` `:36`, `Lsp` `:93`); dispatch `Command::Lsp =>
  ascript::lsp::run_server().await` `:528`. `run_file_on_vm_with_packages` `src/lib.rs:464` (builds the
  `Vm` `:494`, runs `vm.run(&mut fiber)` `:516`). Worker thread builder `src/lib.rs:59`
  (`WORKER_STACK_SIZE`). LSP server `run_server` `src/lsp/mod.rs:30`; `tower-lsp` stdio loop
  `src/lsp/server.rs`. Worker airlock `encode`/`decode` `src/worker/serialize.rs:360`/`:517`.
- Perf harness: `tests/vm_bench.rs` (three-engine tree-walker/generic/specialized ratio harness, gate
  ">= 2× compute-bound, no regression"; corpus `benches()` `:198`). Cargo features: `default` `Cargo.toml:107`,
  `lsp = ["dep:tower-lsp", "tokio/io-std"]` `:161` (the feature-shape to mirror for `dap`).

**New names (do not rename):** `Op::Break`; `Vm.instrument: Option<Box<Instrumentation>>`;
`struct Instrumentation { breakpoints: Option<DebuggerHook>, profiler: Option<ProfilerHook>, coverage:
Option<CoverageTable> }`; `Vm::with_instrument(interp, inst)`; `Chunk.line_starts: Vec<(u32,u32)>`;
`FnProto.local_names: Vec<(u32, Rc<str>)>`; `src/dap/` module + `ascript::dap::run_server`; `src/profile/`;
`dap` Cargo feature; `--inspect` flag (Run), `--strip` flag (Build), `--profile`/`-o`/`--profile-hz`/
`--profile-format` flags (Run); `Command::Dap`.

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `cargo test` AND `cargo test --no-default-features` green; clippy clean BOTH configs **and** with/without
  the new `dap` feature.
- **Gate 1 unchanged:** DBG adds `Op::Break` (never compiler-emitted, never in a normal chunk), debug-only
  metadata, and `None`-gated hooks — `vm_differential.rs` must stay green in both feature configs (the
  tree-walker is not instrumented; the VM not-attached path is byte-identical). Fix the engine, never the
  assertion.
- **Gate 4:** the trap hook builds + serializes the snapshot, drops every `RefCell`/`Cc` borrow, THEN blocks
  on the channel — never hold a borrow across the channel wait. Snapshots are plain data, never `Value`/`Cc`.
- No grammar/AST/tree-sitter change (the one opcode is runtime-patched, never parsed) — fmt/parser/
  tree-sitter conformance are trivially unaffected.

---

## Task 1 — Record the live `.aso` version; the unified `Vm.instrument` seam (G2 core, zero-cost)
**Files:** `src/vm/run.rs`, a new `src/vm/instrument.rs` (or inline in `run.rs`). **Tests:** `run.rs`,
`tests/vm_differential.rs`.
- [ ] **First, record observed constants in this task's commit message:** read `ASO_FORMAT_VERSION`
  (`src/vm/aso.rs:105`, currently 18 — but NUM/ADT/IFACE may have bumped it; the live value is what Task 6
  bumps). Confirm whether DX has already introduced `Vm.instrument` (goal-brief #6): if present, rebase onto
  it and ADD `breakpoints`/`profiler` to the existing `Instrumentation`; if absent, DBG introduces it.
- [ ] Add `instrument: Option<Box<Instrumentation>>` to `Vm` beside `specialize` (`:104`), defaulting to
  `None` in `with_specialize` (`:175`). Define `Instrumentation { breakpoints: Option<DebuggerHook>,
  profiler: Option<ProfilerHook>, coverage: Option<CoverageTable> }` (stub the three hook types this task;
  `coverage` is DX's — leave a typed placeholder). Add `Vm::with_instrument(interp, inst)` mirroring
  `with_specialize`.
- [ ] Failing tests: a default `Vm` has `instrument.is_none()`; `with_instrument` round-trips; a
  no-op `Instrumentation { all None }` run produces byte-identical program output to a plain run (capture
  output, compare).
- [ ] **Zero-cost discipline:** `run_loop` (`:581`) gains NO new per-iteration field load on the `None`
  path. The only hot-loop change in later tasks is the `Op::Break` *match arm* (reached only by a patched
  byte). Confirm by reading the disassembly intent: nothing is added before the `match op`.
- [ ] Green both configs; `vm_differential` green; clippy. Independent review (greps for any per-iteration
  `self.instrument` load on the `None` path; confirms the field is `Box`ed so `Vm` stays small). Commit.

## Task 2 — `Op::Break` opcode + the trap handler (patch / park / resume / re-dispatch)
**Files:** `src/vm/opcode.rs`, `src/vm/run.rs`, `src/vm/disasm.rs`. **Tests:** `opcode.rs`,
`tests/vm_differential.rs`.
- [ ] Add `Op::Break` (no operand) to the enum (`:29`), `from_u8` (`:501`), `ALL` (`:720`), `operand_width`
  → 0 (`:623`). The round-trip test (`:819`) flushes a missing arm. **Never compiler-emitted** — assert no
  `emit`/compiler path produces it (grep `src/compile/` for `Op::Break` → none).
- [ ] `DebuggerHook` (in `src/vm/instrument.rs`): the breakpoint side table `HashMap<(proto-id, offset),
  u8 original_byte>`, the current step mode, the call-depth captured at a step, and the `Send`
  command/event channel ends. `set_breakpoint(offset)` saves the original byte and overwrites it with
  `Op::Break as u8`; `clear` restores it. (Patching does NOT move any offset → IC side maps `field_ics`/
  `method_ics` keyed by offset, `src/vm/chunk.rs`, stay valid; note this in the doc comment.)
- [ ] The `Op::Break` arm in `run_loop`: (1) recover the original opcode from the side table, (2) build a
  frame snapshot + ship a `Stopped` event, (3) **drop all borrows**, block on the command channel until
  `continue`/`step`/`next`/`stepOut`, (4) **re-dispatch the saved original opcode WITHOUT having advanced
  `ip` past it** (so the displaced instruction still runs). Implement re-dispatch by NOT consuming the
  break's "operand width" and looping back to execute the recovered op (mirror the `:594-596` ip-advance
  carefully — the break byte has width 0; the recovered op advances its own width).
- [ ] Stepping = transient breakpoints (NOT per-instruction trace): `stepOut` patches the return site (break
  when `fiber.frames.len()` < current depth); `next` re-breaks at the next-line offset when the stack
  returns to the current depth; `stepIn` also patches a callee entry offset. Read depth from
  `fiber.frames.len()` (`src/vm/fiber.rs:72`) + the SP3 depth `Cell`. Transient breakpoints clear the
  moment the step completes.
- [ ] Failing tests (unit-level, driving the hook directly without the full DAP server): patch an offset →
  `Op::Break` traps; the recovered op executes and the program result is unchanged after auto-continue;
  clearing restores the exact original byte; a program with a breakpoint in dead/uncalled code never traps.
- [ ] Disasm: render `Op::Break` (`src/vm/disasm.rs:25`). **Gate 1:** a `vm_differential` run with the
  `Op::Break` variant *present in the enum* (but never emitted) is byte-identical — assert the corpus is
  unchanged. Green both configs; review (probes re-dispatch correctness on a super-instruction-free op and
  on a 2-operand op); commit.

## Task 3 — Debug info: `line_starts` (derived) + `FnProto.local_names` (from resolver bindings)
**Files:** `src/vm/chunk.rs`, `src/syntax/resolve/types.rs` (if a name needs retaining), `src/compile/mod.rs`.
**Tests:** `chunk.rs`, `tests/frontend_conformance.rs` (compile-path), `compile` unit tests.
- [ ] `Chunk.line_starts: Vec<(u32 line, u32 offset)>` DERIVED from `spans` (`:247`) + module source: build
  a sorted newline-offset index over `source.text`, then for each distinct source line record the FIRST
  instruction offset whose `span` starts on that line (the breakpoint target). Pure function of `spans` +
  source → expose `Chunk::build_line_starts(&self) -> Vec<(u32,u32)>` so it can be reconstructed lazily at
  attach time AND serialized (Task 6). Provide `offset → (line,col)` and `line → first offset` lookups via
  `partition_point`.
- [ ] `FnProto.local_names: Vec<(u32 slot, Rc<str> name)>` (`:358` region). The resolver already records
  every `Binding { name, slot, … }` (`src/syntax/resolve/types.rs:31`) and `ResolveResult.bindings`
  (`:96`); the compiler iterates them (`src/compile/mod.rs:969`). Populate `local_names` per proto/frame
  from the bindings grouped by frame (`let`/`const`/loop-var/param). Debug-only; absent → debugger falls
  back to `slot_N`.
- [ ] Failing tests: `line_starts` maps a blank/comment line to the NEXT executable offset (or reports no
  offset); maps an executable line to its first instruction; `offset→line` round-trips a known program;
  `local_names` labels a `let x = …` slot "x" and a loop-var; a param keeps its name. **Neither table is
  read by `run_loop`** — assert no `run.rs` reference (grep).
- [ ] Green both configs; clippy; review (confirms the tables are pure metadata, no hot-loop reference,
  cost = a compile-time population pass only). Commit.

## Task 4 — The `Send` channel airlock (frame/variable snapshots, plain data)
**Files:** new `src/dap/channel.rs` (or `src/vm/instrument.rs`). **Tests:** unit tests in the module.
- [ ] Define the plain-data command/event protocol carried over a `std::sync::mpsc` (or tokio) `Send`
  channel between the DAP server thread and the VM worker thread: commands
  (`SetBreakpoints`/`Continue`/`Pause`/`Next`/`StepIn`/`StepOut`/`Evaluate`) and events
  (`Stopped`/`Output`/`Exited`/`Terminated`). NO `Value`/`Rc`/`Cc` crosses — reuse the
  `src/worker/serialize.rs` discipline (serialize-out, never a live reference).
- [ ] The frame/scope/variable snapshot types: `StackFrame {name, line, id}`,
  `Scope {Locals|Upvalues|Self}`, `Variable {name, value_display, variables_reference}`. The hook builds
  these from `fiber.frames` (`src/vm/fiber.rs:72`): name from `FnProto`/closure; line via frame `ip` →
  `Chunk::span_at` (`:640`) → `line_starts`; locals from `stack[slot_base..slot_base+slot_count]`
  (`fiber.rs:4`) labeled by `local_names` (Task 3); upvalues from `cells` (`:31`); `self`/`def_class`
  (`:43`). **Containers get a non-zero `variables_reference` for LAZY expansion** (one round-trip per
  expand) with a **visited-`Cc`/`Rc`-identity cycle guard**; `Value::Native` renders opaque `<native #id>`
  (never traced — GC invariant).
- [ ] **Gate 4 test:** building a snapshot holds no borrow across a (mock) channel send — the snapshot is
  fully owned plain data. NUM edge: an `int` `5` and a `float` `5.0` of equal value render distinguishably.
- [ ] Failing tests: a cyclic object expands without infinite loop; a deep array pages lazily; locals
  labeled by name; a `Value::Native` renders opaque. Green both configs; review; commit.

## Task 5 — `ascript dap` server + `run --inspect` (G3)
**Files:** new `src/dap/` (mirror `src/lsp/`: `mod.rs` + `server.rs` + per-capability handlers),
`src/main.rs`, `src/lib.rs`, `Cargo.toml`. **Tests:** `tests/dap.rs`.
- [ ] `Cargo.toml`: a `dap` feature (default-on, mirroring `lsp = ["dep:tower-lsp", "tokio/io-std"]`
  `:161`/`:107`) pulling a DAP-types crate (`dap` / `debugserver-types`); no new always-on dependency.
- [ ] `src/dap/server.rs`: a `Content-Length`-framed JSON request/response/event stdio loop (same framing as
  LSP) on a `Send`-friendly tokio runtime, state behind a `tokio::sync::Mutex` (mirror `src/lsp/server.rs`).
  `ascript::dap::run_server().await` (mirror `src/lsp/mod.rs:30`).
- [ ] Capabilities (v1, §4.3): `initialize`/`launch`/`attach`/`configurationDone`; `setBreakpoints`
  (line / conditional via the evaluator / logpoint → `output` event + auto-continue); `continue`/`pause`;
  `next`/`stepIn`/`stepOut`; `stackTrace`/`scopes`/`variables` (lazy); `evaluate` (Task 8); `threads`
  (single); `exceptionInfo`/break-on-panic (a Tier-2 `Control::Panic` under a debugger stops at the faulting
  frame before unwinding); `disassemble` (reuse `src/vm/disasm.rs`). On `launch`, spawn the debuggee on the
  `WORKER_STACK_SIZE` thread (`src/lib.rs:59` path) with `Vm::with_instrument(Instrumentation { breakpoints:
  Some(hook), .. })`, holding the channel ends.
- [ ] `src/main.rs`: `Command::Dap` (parallel to `Lsp` `:93`, dispatched like `:528`) and the `--inspect`
  flag on `Run` (`:17`). Plumb `--inspect` into `run_file_on_vm_with_packages` (`src/lib.rs:464`) to build
  the `Vm` with `with_instrument` + **break-on-entry** (park before `Op` 0 of the entry proto, wait for
  `configurationDone`). Without `--inspect`, the path is byte-for-byte unchanged.
- [ ] `tests/dap.rs` (spawn the built binary like `tests/lsp.rs`): scripted `initialize` → `launch` →
  `setBreakpoints` → `configurationDone` → expect `stopped` → `stackTrace`/`scopes`/`variables` →
  `continue` → `terminated`. Edge: breakpoint on a blank/comment line binds to the next offset or reports
  unbound; two breakpoints same line; clearing all restores every patched byte; breakpoint inside a
  `worker fn` reports `verified: false` with the "not yet supported" message (§7 single-isolate v1).
- [ ] **Observation contract:** program output is byte-identical run normally vs under `--inspect` with
  breakpoints set-but-auto-continued (Gate 9 / §9). Green both configs **and** `--no-default-features` (dap
  off) — assert `cargo build --no-default-features` does not pull the dap crate. Clippy all configs. Commit.

## Task 6 — `.aso` optional strippable debug section + version bump + verify + `--strip`
**Files:** `src/vm/aso.rs`, `src/vm/verify.rs`, `src/main.rs` (Build). **Tests:** `aso.rs`, `tests/cli.rs`.
- [ ] After the existing chunk body in `write_chunk` (`:517`)/`read_chunk` (`:565`), add an OPTIONAL trailing
  debug block gated by a presence byte/bit: `line_starts` + `local_names` (+ source path, optionally source
  text for `.aso`-only debugging). A reader that finds no section runs normally (`read_chunk` → VM path
  **unchanged** when absent).
- [ ] **Bump `ASO_FORMAT_VERSION` by reading the LIVE constant (`src/vm/aso.rs:105`) and +1 — NEVER hardcode
  19** (goal-brief reconciliation #1; §5.3 — NUM/ADT/IFACE also bump by merge order). Record the observed
  pre-bump value in the commit message.
- [ ] `verify.rs` (`verify_chunk` `:341`): bounds-check the new section (line/slot indices in range) → a
  malformed debug section in an untrusted `.aso` is a clean `AsoError`, NOT a panic (FUZZ surface). Reuse
  the P0 clamp pattern for variable-length reads.
- [ ] `src/main.rs` `Build` (`:36`): a `--strip` flag omitting the section; `ascript build` emits it by
  default.
- [ ] Failing tests: build-with-debug round-trips line/slot tables; `--strip` omits the section and the
  stripped `.aso` still runs; a debugger attaching to a stripped `.aso` reports "no debug info" gracefully;
  a malformed debug section → clean `AsoError`; the version equals `<observed> + 1`. Green both configs;
  review; commit.

## Task 7 — Sampling profiler (G4): timer thread + frame-name publish + speedscope output
**Files:** `src/vm/run.rs` (publish at push/pop), new `src/profile/` (aggregation + output),
`src/main.rs` (flags). **Tests:** `tests/profile.rs` (golden) or `src/profile/*` unit tests.
- [ ] `instrument.profiler: Option<ProfilerHook>` (sub-field of the single `Vm.instrument` — Task 1).
  When armed, the VM publishes the current frame-name vector into a `Send`, lock-free snapshot
  (`arc-swap`/seqlock or `Arc<Mutex<…>>`) at **frame push/pop ONLY** (`fiber.frames.push` `:1253`/`:3459`;
  the `leave_frame_depth` decrement `:318`) — NOT per instruction. `instrument.is_none()` (production) skips
  it entirely. NO `Value`/`Cc` crosses — only frame identity (names/proto pointers).
- [ ] A sampler thread wakes on a fixed interval (default ~1 ms, `--profile-hz` to tune), reads the
  published snapshot, records a `Vec<frame-name>` sample; samples aggregate into a frame-path → hit-count
  call-tree. v1 is **function-level** (per-line is a documented follow-up).
- [ ] `src/profile/`: emit speedscope JSON (`-o flame.json` default `profile.json`); `--profile-format
  collapsed` emits Brendan-Gregg folded text. Determinism: a synthetic sample clock (mirror SP9
  `VirtualClock`) so the golden is not wall-clock-flaky.
- [ ] `src/main.rs` `Run`: `--profile cpu` (only mode in v1; reserves the option), `-o`, `--profile-hz`,
  `--profile-format`. Plumb into `run_file_on_vm_with_packages` (`src/lib.rs:464`) → `with_instrument(…
  profiler: Some(hook) …)`.
- [ ] Failing tests: a deterministic CPU-bound program under the synthetic clock yields a GOLDEN speedscope
  tree; the collapsed-text golden; `--profile` leaves program stdout byte-identical to a non-profiled run
  (observation-only); an empty/short program produces a valid (possibly trivial) profile. Green both
  configs; **zero-cost** (the push/pop publish is `None`-gated — covered by Task 9's bench config (2)).
  Review; commit.

## Task 8 — `evaluate` (watch / debug console) in a paused frame
**Files:** `src/dap/` (evaluate handler), `src/vm/run.rs` (re-entrant eval). **Tests:** `tests/dap.rs`.
- [ ] DAP `evaluate` compiles the expression text through the CST front-end + compiler (`src/compile/`) into
  a throwaway chunk whose free names resolve against the paused frame's scope (locals via `local_names` +
  slots, upvalues via `cells`, globals via `user_globals`), runs it re-entrantly on the SAME `Vm` (the VM
  already re-enters for method/closure calls), and serializes the result back via the Task 4 airlock.
- [ ] Side-effecting expressions DO run (documented — like the V8 debug console); the debugger NEVER injects
  evaluation the user did not request (preserves §9's "stepping is observation" for the non-evaluated run).
- [ ] Failing tests: evaluate a local in a paused frame; a watch expression over an upvalue; a
  container result expands lazily; conditional-breakpoint reuse of the evaluator (break only when truthy);
  logpoint message-template evaluation emits `output` and does not stop. Green both configs; review; commit.

## Task 9 — The 3-config zero-cost benchmark (PRIMARY ACCEPTANCE GATE — §3.4)
**Files:** extend `tests/vm_bench.rs` (the existing three-engine harness) or a `benches/` criterion bench +
CI assertion. **Tests:** the bench itself.
- [ ] Run the existing VM perf-gate corpus (`benches()` `tests/vm_bench.rs:198`) in THREE configs:
  **(1)** pre-DBG `main` (baseline), **(2)** post-DBG with `Vm.instrument == None` (the not-attached
  production path — also DX's coverage-off config), **(3)** post-DBG attached-no-breakpoints
  (`instrument.breakpoints == Some(empty)` — armed-but-idle, no byte patched).
- [ ] **Pass condition (the merge gate):** config (2) is within noise of config (1) — a *statistical
  no-op*, the bar the `None`-gated seams already meet — AND the VM geomean stays ≥ 2× the tree-walker
  (the existing harness gate `:36`). A regression in (2) is a BUG to fix (a stray flag check leaked into the
  hot loop), never an accepted tradeoff. Config (3)'s overhead is reported but only loosely gated.
- [ ] Make this a CI assertion (the spec's primary acceptance artifact). Review (an independent reviewer
  RUNS the bench and confirms (2)≈(1) and geomean ≥ 2×). Commit.

## Task 10 — Docs, VS Code launch.json, examples (Gate 9 + Gate 11)
**Files:** `docs/content/` (+ `docs/assets/app.js` NAV), `editors/`/`docs/` (launch.json), `examples/`,
`README.md`, the design spec, `CLAUDE.md`, `roadmap.md`. **Tests:** conformance/examples runnable.
- [ ] `docs/content/`: a "Debugging & profiling" guide (DAP setup, `--inspect`, `--profile`). **Add its slug
  to the `NAV` array in `docs/assets/app.js`** (the orphan gotcha — no NAV entry → unreachable in sidebar
  AND cmd-K search).
- [ ] **Gate 9 artifact (a):** a VS Code `launch.json` (`ascript`-type, `request: launch`, `program:
  ${file}`, driving `ascript dap`) under `docs/`/`editors/`, plus a Neovim DAP snippet; AND the matching
  `examples/` debugging-walkthrough program (a deliberate bug) — output unchanged with/without `--inspect`.
- [ ] **Gate 9 artifact (b):** an `examples/advanced/` CPU-bound program run under `--profile cpu` whose
  golden speedscope profile is stable and whose stdout is byte-identical to a non-profiled run.
- [ ] `README.md` debugging/profiling mention (scripting→general-purpose repositioning, Gate 11); update the
  design spec status + `CLAUDE.md` (a DBG subsystem paragraph) + `roadmap.md`. Examples stay four-mode-clean
  and fmt-idempotent (no grammar change). Review; commit.

## Done when
Every task checked behind an independent review; **the 3-config bench (Task 9) proves config (2) ≈ (1)
within noise and geomean ≥ 2× the tree-walker** (the primary gate); `vm_differential` byte-identical in
both feature configs (Gate 1; `Op::Break` present but never emitted); clippy + `cargo test` green in both
feature configs AND with/without the `dap` feature AND `--no-default-features`; `examples/**` emits zero
`type-*` (DBG adds no inference); the `.aso` debug section is optional/strippable with a SEQUENTIAL version
bump (read live + 1, never 19); v1 is single-isolate (worker-fn breakpoints report `verified: false`).
Merge `--no-ff` to `main` (DBG is the last campaign feature; rebases onto whatever NUM/ADT/IFACE left for
`ASO_FORMAT_VERSION` and the `Vm.instrument` seam if DX landed it first).
