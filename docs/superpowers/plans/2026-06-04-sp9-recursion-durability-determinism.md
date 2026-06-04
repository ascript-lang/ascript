# SP9 — Robust recursion, replay durability, determinism seams — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`)
> syntax for tracking.

**Goal:** Realize the three M17 async non-goals on the existing model-2a engine, to the extent each is
achievable WITHOUT a model-2b VM, and leave the genuine 2b residual documented (not dropped):
(1) robust unbounded recursion via `stacker` at the four native re-entry points; (3) determinism seams
(virtual clock + seeded RNG + recorded ordering) behind a per-`Interp` deterministic-mode context; and
(2) durable execution via event-sourced replay (`std/workflow`), built on top of (3).

**Architecture:** Three independent workstreams as four phases. **Phase 1 (recursion)** is small,
independent, and lands first. **Phase 2 (determinism seams)** adds the inert-by-default
`DeterminismContext`. **Phase 3 (replay durability)** is the largest — the `std/workflow` subsystem in
sub-phases, built on Phase 2. **Phase 4** is docs + the ADR reclassification + holistic review. Each
phase is TDD, ends green on both feature configs + clippy + the whole-corpus three-way differential +
perf gate, and gets an independent review before the next.

**Tech Stack:** Rust. New dep `stacker` (core/unconditional). CST front-end → resolver
(`src/syntax/resolve`) → compiler (`src/compile/mod.rs`) → `Chunk` → VM (`src/vm/*`). Tree-walker
(`src/interp.rs`) is the byte-identical oracle. Event-log serialization reuses `src/stdlib/json.rs`
(gated by the `data` feature).

**Spec:** `docs/superpowers/specs/2026-06-04-sp9-recursion-durability-determinism-design.md`.

**Branch:** `feat/sp1-engine-parity`.

---

## Conventions for every task

- **Differential harness:** `tests/vm_differential.rs` compares `ascript::vm_run_source(src)`
  (specialized VM), `ascript::vm_run_source_generic(src)` (generic VM), and `ascript::run_source_exit`
  (tree-walker). "Byte-identical" = identical stdout + exit on all three. Read neighbors before adding.
- **Per-engine smoke:** `cargo build` then `target/debug/ascript run X.as` (VM) vs
  `target/debug/ascript run --tree-walker X.as`.
- **Gate after each phase (paste tails):** `cargo test --test vm_differential 2>&1 | tail`;
  `cargo test 2>&1` (0 failures all binaries); `cargo test --no-default-features 2>&1` (0 failures);
  `cargo clippy --all-targets` AND `cargo clippy --no-default-features --all-targets` (clean);
  `grep await_holding_refcell_ref Cargo.toml` (still `deny`);
  `cargo test --release --test vm_bench -- --ignored --nocapture` (geomean ≥2×, no spec-vs-generic
  regression).
- **Commit trailer:** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Never** weaken a differential assertion or edit a passing tree-walker test to match the VM. A
  divergence on valid code = fix the root cause. Never hold a `RefCell` borrow (incl. the new
  `determinism` cell) across an `.await` — take the value out first (the `resources` discipline).

---

## Phase 1 — Robust unbounded recursion (`stacker` at the native re-entry points)

Small, independent, byte-identical. Lands first. Spec §1.

**Files:** `Cargo.toml` (add `stacker`), new `src/vm/stack.rs` (constants + `grow` helper),
`src/vm/run.rs` (re-entry sites at `:2584`, `:3378`, `:3282`), `src/coro.rs` (`resume_vm` `:216`),
`src/compile/mod.rs` (`compile_expr` `:905`, synchronous), `src/interp.rs` (`run_body`, `eval_expr`
`:1365`), `tests/vm_limits.rs`.

### Task 1.1: failing deep-recursion reproducers

- [ ] **Step 1 — Write failing tests** in `tests/vm_limits.rs` (new file or extend the SP3 one if it
  exists — read first). Each is a program that currently `SIGABRT`s (exit 134) at a §1.1 re-entry site
  and must, after the guard, **succeed** byte-identically on BOTH engines. Use depths large enough to
  overflow today but bounded enough to run in <1s once grown:

  - **Deep higher-order callback** (`call_value` re-entry, `src/vm/run.rs:2584`): a deeply nested
    `array.map`/`reduce` or a recursive comparator chain that nests `call_value` ~10,000 deep.
  - **Deep generator composition** (`resume_vm`, `src/coro.rs:216`): a generator whose body resumes
    another, nested ~10,000 deep.
  - **Deeply nested expression** (compiler `compile_expr` `:905` + tree-walker `eval_expr` `:1365`):
    `let x = ((((…1…))))` with ~50,000 nested parens (SP3 §B1's confirmed crasher).
  - **Deep non-IC method dispatch** (`invoke_compiled_method`/`vm_construct`): a recursion routed
    through the non-IC method path.

  Assert exit 134 (or capture the `SIGABRT`) BEFORE the fix; the harness records "currently crashes."
- [ ] **Step 2 — Run, verify they crash today:** `cargo test --test vm_limits 2>&1 | tail` — document
  the exit-134 / overflow for each (these are the failing baseline).

### Task 1.2: add `stacker` + the shared grow helper

- [ ] **Step 3 — Add dep.** `Cargo.toml`: `stacker = "0.1"` as a **core/unconditional** dep (NOT a
  feature — it must build under `--no-default-features`; verify with a no-features build after).
- [ ] **Step 4 — Create** `src/vm/stack.rs` (or `src/recursion.rs`): named `const RED_ZONE: usize =
  128 * 1024;` and `const STACK_SIZE: usize = 2 * 1024 * 1024;` (spec §1.3), plus a tiny
  `pub fn grow<R>(f: impl FnOnce() -> R) -> R { stacker::maybe_grow(RED_ZONE, STACK_SIZE, f) }` and an
  async-boundary helper as needed (see Step 6). Document the constants. `cargo build` (+ no-features).
- [ ] **Step 5 — Commit:** `feat(recursion): add stacker dep + shared RED_ZONE/STACK_SIZE grow helper`.

### Task 1.3: guard the synchronous re-entry (compiler nested-expr)

- [ ] **Step 6 — Implement** the synchronous case first (no async subtlety): wrap the recursive body of
  `compile_expr` (`src/compile/mod.rs:905`) in `crate::vm::stack::grow(|| { … })` around the recursion
  (or guard at the entry so each level re-checks). Same for any synchronous deep recursion in the
  tree-walker expr path that is NOT `#[async_recursion]`.
- [ ] **Step 7 — Run** the nested-paren reproducer (both engines, compile + tree-walker eval) → success,
  byte-identical. `cargo test --test vm_limits nested 2>&1 | tail`.
- [ ] **Step 8 — Commit:** `feat(recursion): grow native stack in compile_expr (deep nested expressions)`.

### Task 1.4: guard the async re-entry points (VM + generator + tree-walker)

- [ ] **Step 9 — Read** the exact `.await` boundaries: `call_value` `Value::Closure` arm
  (`src/vm/run.rs:2584` `self.run(...).await`), `invoke_compiled_method` (`:3378`, its `self.run`),
  `vm_construct` (`:3282`), `coro::resume_vm` (`:216`, its `Vm::run` await), tree-walker `run_body` +
  `eval_expr` (`src/interp.rs:1365`). Note: these are `#[async_recursion]`, which **boxes** each
  recursive future — the synchronous prologue of each re-entry runs before the next suspension.
- [ ] **Step 10 — Implement** the async-boundary guard: at each funnel, wrap the **synchronous setup +
  the spawning of the boxed re-entry future** in `stacker::maybe_grow` so the native-stack-consuming
  portion of each re-entry runs inside a grown segment, guaranteeing ≥`RED_ZONE` stack before the boxed
  future is entered. Concretely: grow around the `self.run(...)`/`Vm::run(...)` driver at each of the
  five sites. (Spec §1.2: measure; if a pure-async `maybe_grow` proves insufficient at a specific site,
  fall back to the per-site **re-entry trampoline** for that site only — convert the recursive
  `run().await` into a fiber-stack loop — no new dep, no `unsafe`. Document which form each site uses.)
- [ ] **Step 11 — Run** ALL Task-1.1 reproducers (deep map/reduce, deep generator compose, deep method
  dispatch) → success on BOTH engines, byte-identical. `cargo test --test vm_limits 2>&1 | tail`.
- [ ] **Step 12 — Confirm the IC fast path is untouched.** The hot `self.f(...)` inline-cache call path
  (frame-push only, no `call_value`/`invoke_compiled_method`) must NOT gain a `maybe_grow` (it doesn't
  re-enter). Verify by reading the IC call arm; perf gate confirms no regression.

### Task 1.5: Phase-1 gate + SP3 coordination note

- [ ] **Step 13 — Whole-corpus differential** must stay byte-identical (no corpus program triggers a
  segment): `cargo test --test vm_differential 2>&1 | tail`.
- [ ] **Step 14 — Full gate set** (both feature configs + clippy both + perf bench). The guard is a
  per-re-entry stack probe (not per-opcode) → no measurable perf regression.
- [ ] **Step 15 — SP3 coordination:** if SP3's `MAX_CALL_DEPTH` is already merged, add a comment at its
  definition noting SP9 §1 lets the native re-entry paths reach the logical cap cleanly (so the cap is
  the ceiling, not the native stack). If the owner picked the "always-on stacker, cap stays" option
  (spec §1.6), no value change is needed here. Do NOT change the cap value without owner sign-off.
- [ ] **Step 16 — Commit:** `feat(recursion): grow native stack at VM/generator/tree-walker re-entry — deep recursion no longer SIGABRTs`.

### Task 1.6: Phase-1 independent review

- [ ] **Step 17 — Independent review:** re-read spec §1; re-run the gate; adversarially probe other
  native re-entry sites the audit might have missed (grep `self.run(`, `Vm::run`, `call_value(` across
  `src/`); confirm byte-identical differential; confirm no `unsafe` beyond stacker's own. Fix any gap.

---

## Phase 2 — Determinism seams (`DeterminismContext`: virtual clock + seeded RNG)

Inert by default → byte-identical by default. Built before Phase 3 (the workflow replay engine needs
these seams). Spec §3.

**Files:** new `src/det.rs` (`DeterminismContext`, `VirtualClock`, `SeededRng`, `DetEvent`),
`src/interp.rs` (add the `determinism` cell + accessors), `src/stdlib/{time,date,math,uuid,crypto}.rs`,
`src/stdlib/mod.rs` (`time.sleep`), `tests/determinism.rs`.

### Task 2.1: the context type + Interp wiring (no behavior change)

- [ ] **Step 1 — Create** `src/det.rs` (core, NO `serde`): `DeterminismContext { mode: Mode, clock:
  VirtualClock, rng: SeededRng, seed: u64, cursor: usize, events: Vec<DetEvent> }`; `Mode {Record,
  Replay}`; `SeededRng` = the same xorshift algorithm as `src/stdlib/math.rs:337-347` but seeded from
  an explicit `u64` (not time+addr); `VirtualClock` holding a current ms-epoch; `DetEvent {ClockRead,
  RandomRead, …}`. Unit-test the `SeededRng` is reproducible for a fixed seed.
- [ ] **Step 2 — Wire** `Interp`: add `determinism: RefCell<Option<DeterminismContext>>` beside
  `inflight`/`log_level` (`src/interp.rs:305-317`); initialize `None` in the constructor (`:378`+).
  Add accessors `clock_now_ms(&self) -> f64`, `clock_monotonic_ms(&self) -> f64`,
  `next_seeded_f64(&self) -> Option<f64>` (returns `None` when determinism is `None`, so callers fall
  back to today's path) and a `with_determinism`/`enter_deterministic(seed)` helper for tests/§3.
  CRITICAL: never hold the `determinism` `RefCell` across `.await` — accessors take the value out and
  drop the borrow before any await.
- [ ] **Step 3 — Run** `cargo build` (+ no-features) and the full suite → unchanged (nothing reads the
  context yet). Commit: `feat(det): DeterminismContext + Interp wiring (inert; no behavior change)`.

### Task 2.2: RNG seam (math/uuid/crypto)

- [ ] **Step 4 — Failing determinism test** in `tests/determinism.rs`: enter deterministic mode with
  seed=42, call `math.random` twice, assert the sequence equals a second seed=42 run (same-seed-same-
  output, spec §3.5). Currently fails (no seam).
- [ ] **Step 5 — Implement** `src/stdlib/math.rs::next_random` (`:337`): consult
  `Interp.determinism` — when `Some`, draw from the context `SeededRng` (Record: draw + push
  `RandomRead`; Replay: return recorded at `cursor`); when `None`, the existing thread-local path
  **unchanged** (byte-identical). Thread `&Interp` to `next_random` (the math dispatch has it). Same
  routing for `randomInt`/`shuffle`/`sample` (they funnel through `next_random`). For `uuid.v4`
  (`src/stdlib/uuid.rs:15`) and `crypto.randomBytes` (`src/stdlib/crypto.rs:105`), derive bytes from
  the context `SeededRng` in deterministic mode; unchanged otherwise.
- [ ] **Step 6 — Run** → the seed=42 test passes; default-mode RNG tests unchanged.
- [ ] **Step 7 — Commit:** `feat(det): seeded RNG seam (math/uuid/crypto) — reproducible under deterministic mode`.

### Task 2.3: clock seam (time/date/sleep)

- [ ] **Step 8 — Failing test:** in deterministic mode, `time.now`/`date.now` return the recorded
  virtual time (two runs same seed/recorded → identical); `time.sleep(ms)` does not sleep real time
  (advances virtual clock). Currently fails.
- [ ] **Step 9 — Implement:** route `time.now` (`src/stdlib/time.rs:36`), `time.monotonic` (`:43`),
  `date.now` (`src/stdlib/date.rs:83`) through `Interp::clock_now_ms`/`clock_monotonic_ms` — real clock
  when `None` (byte-identical), virtual/recorded when `Some`. `time.sleep` (`src/stdlib/mod.rs:409`):
  in deterministic mode advance the virtual clock + record a timer event instead of `tokio::time::sleep`.
- [ ] **Step 10 — Run** → clock tests pass; default-mode time/date tests unchanged.
- [ ] **Step 11 — Commit:** `feat(det): virtual clock seam (time/date/sleep) under deterministic mode`.

### Task 2.4: Phase-2 gate + determinism oracle + review

- [ ] **Step 12 — Determinism oracle** in `tests/determinism.rs`: a single-task program using
  random+time runs twice with the same seed → byte-identical output (spec §3.5). Add a default-mode
  assertion that the SAME program in non-deterministic mode is byte-identical to pre-SP9 (seams inert).
- [ ] **Step 13 — Whole-corpus differential** byte-identical (no corpus program enters deterministic
  mode): `cargo test --test vm_differential 2>&1 | tail`.
- [ ] **Step 14 — Full gate set** both feature configs + clippy both + perf (the `None` branch is a
  single `RefCell` `is_none` check on the seam paths — confirm no hot-path regression; the seams are
  in stdlib calls, not the VM opcode loop).
- [ ] **Step 15 — Independent review:** confirm every seam's `None` branch is the exact current code;
  confirm no `RefCell` held across `.await`; adversarially check the §3.6 residual is honestly scoped
  (no claim of arbitrary-task-interleaving determinism). Commit fixes if any.

---

## Phase 3 — Durable execution via event-sourced replay (`std/workflow`)

The largest workstream. Built on Phase 2's seams. Sub-phased so it can be cut/deferred without touching
Phases 1–2 (spec §6 open Q2). Spec §2.

**Files:** new `src/stdlib/workflow.rs`, `src/stdlib/mod.rs` (routing + `pub mod` gated
`#[cfg(feature="workflow")]`), `Cargo.toml` (`workflow = ["data"]` in `default`),
`src/check/rules/workflow_determinism.rs` (new lint), reuse `src/stdlib/json.rs`, `tests/workflow.rs`,
`docs/content/stdlib/workflow.md`, `examples/advanced/workflow_*.as`.

### Task 3.1: feature + module skeleton + `activity`

- [ ] **Step 1 — Add feature.** `Cargo.toml`: `workflow = ["data"]`, add to `default`. Declare
  `#[cfg(feature = "workflow")] pub mod workflow;` in `src/stdlib/mod.rs` and register it in **both**
  match arms (`std_module_exports` + `call`). Under `--no-default-features` it is compiled out;
  `import "std/workflow"` → unknown-module error (assert this in a no-features test — symmetric both
  engines, spec §2.7).
- [ ] **Step 2 — Implement `activity(name, fn)`** (`src/stdlib/workflow.rs`): returns a tagged value
  (an Object `{__kind:"activity", name, fn}` — NO new `Value` variant, mirroring `std/schema`'s tagged
  Objects) wrapping the side-effecting fn. A bare `activity(...)` outside a workflow is just a callable
  that runs its fn directly (no recording) — recording happens only via `ctx.call`.
- [ ] **Step 3 — Failing test** in `tests/workflow.rs`: `activity("a", fn(){return 1})` constructs;
  calling it outside a workflow runs the fn. Run → implement → green.
- [ ] **Step 4 — Commit:** `feat(workflow): std/workflow module skeleton + activity() (feature-gated on data)`.

### Task 3.2: the workflow context + `run` (record mode)

- [ ] **Step 5 — Implement the `ctx`** passed to a workflow body: a tagged Object exposing `call`,
  `now`, `random`, `uuid` (and `sleep` in 3.4) that route through the Phase-2 `DeterminismContext` in
  **Record** mode. `ctx.call(activity, ...args)`: run the activity fn for real, JSON-serialize the
  result via `json::to_json_lossy`, append an `ActivityCompleted` event (with `seq`, name, args hash),
  return the result. `ctx.now`/`ctx.random` append `ClockRead`/`RandomRead` (reusing Phase-2's events).
- [ ] **Step 6 — Implement `workflow.run(wf, input, {log})`**: enter deterministic mode (Record) on the
  `Interp` (Phase 2's `enter_deterministic`), build a fresh `ctx`, call `wf(ctx, input)` to completion,
  append `WorkflowStarted`/`WorkflowCompleted`, flush the event stream to the log sink (newline-JSON,
  spec §2.3; `{durability: "fsync"|"buffered"}`). Returns the workflow result.
- [ ] **Step 7 — Failing test:** a workflow that calls two activities + `ctx.now` + `ctx.random`,
  run with a temp-file log → asserts the result AND that the log contains the expected event sequence.
  Run → implement → green.
- [ ] **Step 8 — Constraint check:** an activity returning a native handle / function → constraint
  violation at record time (`"workflow: activity result is not serializable"`). Test it.
- [ ] **Step 9 — Commit:** `feat(workflow): workflow context + run() record mode + event log (newline-JSON)`.

### Task 3.3: `resume` (replay mode) + non-determinism detection

- [ ] **Step 10 — Implement `workflow.resume(wf, input, {log})`** (spec §2.4): read the log; if it ends
  in `WorkflowCompleted`, return that result (idempotent). Else re-run `wf(ctx, input)` from the top
  with `ctx` in **Replay** mode: each `ctx.call`/`ctx.now`/`ctx.random` consumes the next matching
  recorded event at `cursor` and returns its value WITHOUT executing the side effect, asserting the
  signature (kind + activity name + args hash) matches. On an effect with NO recorded event (the crash
  point) → switch to Record mode, execute for real, append, continue.
- [ ] **Step 11 — Failing tests** (the durability oracle, spec §4):
  - **record → simulate crash → resume → byte-identical result:** run a workflow but truncate the log
    after the first `ActivityCompleted` (simulating a crash mid-second-activity), then `resume` →
    the first activity replays (NOT re-executed — assert its side effect, e.g. a counter, fires only
    once across record+resume), the second executes for real, final result matches a clean run.
  - **replay-mismatch detection:** resume a log against a workflow whose effect order changed →
    Tier-2 panic `workflow non-determinism: expected <recorded> at seq N, got <actual>`.
  - **idempotent resume:** resuming a completed log re-runs nothing, returns the recorded result.
- [ ] **Step 12 — Run → implement → green.** Commit: `feat(workflow): resume() replay mode + non-determinism detection (durable execution)`.

### Task 3.4: durable timers (`ctx.sleep`)

- [ ] **Step 13 — Implement `ctx.sleep(ms)`** (spec §2.2/§3.3): Record mode appends a `TimerSet
  {wake}` and advances the virtual clock (no real sleep); Replay returns immediately if `wake` has
  passed. Failing test: a workflow with a `ctx.sleep` between two activities resumes correctly without
  real delay. Run → implement → green. Commit: `feat(workflow): durable ctx.sleep timer`.

### Task 3.5: the `workflow-determinism` checker lint

- [ ] **Step 14 — Implement** `src/check/rules/workflow_determinism.rs` (additive, zero-FP, spec §2.5):
  inside a function passed to `workflow.run`/`resume`, flag direct calls to `time.now`/`date.now`/
  `math.random`/`crypto.randomBytes`/`uuid.v4`/`net.*`/`fs.*`/`sql.*` (outside an `activity`),
  recommending the `ctx`/activity form. Best-effort; the runtime detector (3.3) is authoritative.
- [ ] **Step 15 — Corpus zero-FP guard:** `ascript check examples/*.as examples/advanced/*.as` →
  0 new diagnostics. Failing test (a deliberately-wrong workflow flags; a correct one doesn't).
  Run → implement → green. Commit: `feat(check): workflow-determinism lint (zero-FP)`.

### Task 3.6: example + docs + Phase-3 gate

- [ ] **Step 16 — Corpus example** `examples/advanced/workflow_signup.as`: a fully error-handled
  record→resume workflow (deterministic, ends with a stable print). Verify it runs:
  `target/release/ascript run examples/advanced/workflow_signup.as`. Ensure it parses on all parsers.
- [ ] **Step 17 — Docs** `docs/content/stdlib/workflow.md`: the API (`run`/`activity`/`resume`/`ctx`),
  the event-log format, the determinism constraints + at-least-once activity caveat (spec §2.6), and
  the `data`-feature requirement. Verify snippets against the binary.
- [ ] **Step 18 — Phase-3 gate:** full gate set both feature configs (the no-features build must compile
  with `std/workflow` `#[cfg]`-out, and `cargo test --no-default-features` green) + clippy both + perf
  (workflow is opt-in, no hot-path impact) + the workflow oracle (`tests/workflow.rs`).
- [ ] **Step 19 — Independent review:** re-read spec §2; adversarially probe the replay edges
  (crash exactly on the boundary event; resume against a longer/shorter log; non-serializable result;
  concurrent in-workflow fan-out — confirm it's documented as the §3.6 residual, not silently
  mis-handled). Fix at the root. Commit fixes.

---

## Phase 4 — ADR reclassification + docs + holistic review

**Files:** `docs/superpowers/specs/adr/2026-05-30-async-generators.md`, `docs/content/*`,
`docs/superpowers/specs/2026-05-29-ascript-design.md`.

### Task 4.1: reclassify the three M17 non-goals in the ADR

- [ ] **Step 1 — Update** the ADR's deferral section per SP9's outcome (spec §0/§1.4/§2.1/§3.6):
  - **#2 robust deep recursion:** delivered — explicit `Fiber.frames` (already) + SP9 §1 `stacker`
    guard at the native re-entry points; SP3's cap is the safety backstop/ceiling.
  - **#1 durable continuations:** reclassified from "deferred, needs 2b" to **"won't-do as
    continuation serialization (native handles make it impossible); delivered as replay-based durable
    execution (`std/workflow`), which needs no 2b."**
  - **#3 deterministic scheduling:** the seeded-clock/RNG + recorded-ordering subset is delivered
    (SP9 §3); the named residual that still requires 2b is **arbitrary concurrent-task interleaving
    determinism** (an owned scheduler replacing tokio).
- [ ] **Step 2 — Commit:** `docs(adr): reclassify the three M17 non-goals per SP9 (recursion done, durability=replay, determinism subset done)`.

### Task 4.2: language/stdlib docs + holistic gate

- [ ] **Step 3 — Update** `docs/content/stdlib/{time,math,workflow}.md` (deterministic-mode notes;
  workflow page from 3.7) and the language spec (`docs/superpowers/specs/2026-05-29-ascript-design.md`)
  with a "robust recursion / durable execution / determinism" note pointing at SP9. Verify snippets.
- [ ] **Step 4 — Holistic gate:** full gate set both feature configs + clippy both + the recursion
  reproducers + determinism oracle + workflow oracle + whole-corpus three-way differential
  byte-identical + perf geomean ≥2× no spec-vs-generic regression.
- [ ] **Step 5 — Holistic independent review:** re-read the whole spec; confirm all three workstreams
  match; confirm the §3.6 2b residual is the ONLY explicitly-out item and is documented (not dropped);
  confirm byte-identical default behavior. Fix any divergence at the root.
- [ ] **Step 6 — Final commit** if review surfaced fixes.

---

## Self-review (author)

**Spec coverage:** §1 recursion → Phase 1 (all four re-entry sites: `call_value` :2584, generator
`resume_vm` :216, method dispatch `invoke_compiled_method`/`vm_construct`, nested-expr `compile_expr`
:905 + tree-walker `eval_expr` :1365); §3 determinism seams → Phase 2 (clock, seeded RNG, the residual
named in 2.4/4.2); §2 durability → Phase 3 (activity, run/record, resume/replay, durable timer, lint,
example/docs). ADR reclassification → Phase 4. All three workstreams + the explicitly-out residual
covered.

**Ordering rationale:** recursion first (smallest, independent, byte-identical — no dependency on the
others); determinism seams second (Phase 3 replay needs them); durability last (largest, sub-phased,
cuttable per open Q2 without touching Phases 1–2).

**No-2b discipline:** Phase 1 only relocates native frames (`stacker`) — no driver, no scheduler, no
CPS. Phase 2 is an inert-by-default `DeterminismContext` — tokio untouched. Phase 3 re-runs workflow
code on an ordinary stack (replay), never serializing a continuation. The one 2b residual
(arbitrary-task-interleaving determinism) is documented in spec §3.6 and the ADR (Phase 4), not
implemented.

**Differential/perf safety:** §1 byte-identical by construction (no corpus program recurses to a
segment); §3 byte-identical by default (`None` branch = current code); §2 additive opt-in module. Perf
gate after every phase; the §1 guard is per-re-entry (not per-opcode) and the IC fast path is
explicitly left untouched (Task 1.4 Step 12).

**Placeholder scan:** no "TBD". The one place deferred to the implementer is the exact async-boundary
`stacker` wrapping form at the `#[async_recursion]` sites (Task 1.4 Step 10) — the spec §1.2 mandates
*measuring* it (failing reproducer → green) with a documented per-site trampoline fallback, not
guessing. Test programs are concrete; file:line citations are from the live source.

**Type consistency:** `stacker` core-unconditional dep; `RED_ZONE`=128 KiB / `STACK_SIZE`=2 MiB in
`src/vm/stack.rs`; `determinism: RefCell<Option<DeterminismContext>>` on `Interp`; `activity` as a
tagged Object (no new `Value` variant); `workflow = ["data"]` feature; event log = newline-JSON via
`json::to_json_lossy`. Consistent across spec and plan.
