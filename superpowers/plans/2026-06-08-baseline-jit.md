# JIT — Baseline Cranelift JIT — Implementation Plan (DEFERRED / GATED)

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development (fresh implementer + independent reviewer
> per task; reviewer runs the commands and probes edges). Steps use `- [ ]`.
>
> **THIS PLAN IS EXPLORATORY AND DEFERRED. It is NOT scheduled. Do NOT begin Task 1+ until Task 0 (the
> gate) has PASSED with a recorded, owner-noted go/no-go decision.** The JIT is the single sanctioned
> deferral of the Serious Language campaign (`goal-brief.md` "JIT (deferred) — Cranelift baseline; only
> after NUM+VAL+profiling"). Per `goal.md`, a deferral proceeds only with a recorded justification —
> here that recording IS Task 0. If any one of the three preconditions fails, this document stays
> reference material and no further task runs.

**Spec:** `superpowers/specs/2026-06-08-baseline-jit-design.md`. **Branch:** `feat/baseline-jit` off
`main` (cut ONLY after Task 0 passes). **Depends on (HARD gate, all three):** **NUM merged** (`Value::Int`
live, `ArithKind::Int` in `adapt.rs`) AND **VAL merged** (`Value` ≤16B, scalars unboxed/inlined — NOT
necessarily full NaN-box) AND **profiling evidence** that interpreter *dispatch* (not allocation/GC/I/O)
dominates a CPU-bound corpus. **Non-breaking:** runtime-only, in-memory; no `.aso` change
(`ASO_FORMAT_VERSION` untouched at `aso.rs:105`), no opcode change, no surface-syntax change.

**Architecture:** a method-level **baseline JIT** behind a default-off Cargo `jit` feature. Tier-up by
per-`FnProto` call + loop-backedge counting (cheap `Cell<u32>` home, NOT a `RefCell<HashMap>`); compile a
hot `FnProto` (`chunk.rs:335`) from its bytecode to native code via **Cranelift** (`cranelift-jit` +
`cranelift-frontend`, pure-Rust, no LLVM); codegen consumes the EXISTING `adapt.rs` `ArithKind::Int`/`Number`
+ `ic.rs` `Mono` feedback behind guards; checked-vs-wrapping integer codegen preserves NUM semantics via a
**conditional branch → shared panic helper** (never a CLIF `trap`); deopt = mechanism (a) per-op
shared-generic-helper fallback (sound after side effects) + mechanism (b) entry-only re-dispatch (no
mid-function OSR); per-isolate code cache on `Vm` (`run.rs:51`). The interpreter stays tier-0 + the deopt
target. **The gate is the four/five-way differential** (tree-walker == generic-VM == specialized-VM ==
**JIT-VM**) incl. always-JIT(threshold=0) + always-deopt + COMBINED modes + a JIT-COVERAGE assertion.

**Tech stack:** Rust; `cranelift-jit`/`cranelift-frontend`/`cranelift-codegen` (feature-gated dev/runtime
deps, pure-Rust posture like `stacker`/`gcmodule`, `Cargo.toml:28,34`); `src/vm/{chunk,run,adapt,ic,opcode}.rs`;
a new `src/vm/jit/` module tree; `tests/vm_differential.rs`; `bench/`.

---

## Shared API Contract (pinned to current code — verify post-NUM/VAL rebase)
**Existing (verified at HEAD; line numbers WILL shift after NUM + VAL merge — re-grep, never hardcode):**
- `FnProto` `chunk.rs:335` — the unit of compilation; plain struct (no interior cells today),
  `is_async:339`/`is_generator:340`, `params:358` (the shared arg layout), `ret:361`.
- `Closure { proto: Rc<FnProto>, … }` `value_ext.rs:22-23` — the live frame reaches its proto here.
- `Vm` struct `run.rs:51` — `specialize:104`, `shapes:75`, `class_methods:61`, `user_globals:134`; the
  per-isolate home for all JIT state.
- `Vm::run_loop` `run.rs:581`; frame-push / call site `run.rs:1253` (call-counter site); `Op::Loop`
  backedge arm `run.rs:1418` (backedge-counter site); `enter_frame_depth` `run.rs:299` (+ `MAX_CALL_DEPTH`,
  `call_depth_cell`); the guard-miss → generic deopt arm `run.rs:3798-3803` (`set_arith_cache(deopt)` →
  `apply_binop`); `eval_binop_adaptive` `run.rs:3743`.
- `apply_binop` `interp.rs:5076` (`pub(crate) fn apply_binop(op, l, r, span) -> Result<Value, Control>`) —
  the shared generic arithmetic helper mechanism (a) calls. `check_call_args` (interp) — the shared arg
  checker `FnProto.params` feeds.
- `ArithKind` `adapt.rs:49` (post-NUM: `Int` ALONGSIDE `Number`=float), `ArithCache::Specialized{kind}`
  `adapt.rs:78`, `WARMUP_THRESHOLD:44`, `arith_cache(op_off)` `chunk.rs:607`, `GlobalCache` `adapt.rs:164`.
- `InlineCache::Mono{shape,index}` `ic.rs:60`, `POLY_MAX:48`, `MethodCache::Mono` `ic.rs:147`.
- `Op::operand_width` `opcode.rs:623` (the decode discipline the translator reuses).
- `vm_run_source` / `vm_run_source_generic` `lib.rs:332/345` (specialized + generic VM test entry points);
  three-way assertion `tests/vm_differential.rs:1198-1204`; whole-corpus gate + `EXAMPLE_SKIPS` `:797`.

**New names (do not rename):** Cargo feature `jit`; module `src/vm/jit/` (`mod.rs`, `translate.rs`,
`codegen.rs`, `cache.rs`, `deopt.rs`, `counters.rs`); `FnProto.jit_call_count: Cell<u32>` +
`FnProto.jit_backedge_count: Cell<u32>` (cheap home, `#[cfg(feature="jit")]`); per-`FnProto` JIT state
enum `JitState { Cold, Warming, Queued, Compiled, Blacklisted }` (on the per-isolate side table, keyed by
`Rc<FnProto>` identity); `Vm.jit: Option<JitContext>` (mirrors `Vm.specialize`); env knobs
`ASCRIPT_JIT_THRESHOLD` (0 = always-JIT) + `ASCRIPT_JIT_STRESS_DEOPT` (force every guard to miss).

## Conventions (every task)
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- BOTH feature-config gates from `goal-brief.md` (clippy clean `--all-targets` AND
  `--no-default-features --all-targets`; `cargo test` AND `cargo test --no-default-features` green) PLUS a
  THIRD config: `cargo test --features jit` and `cargo clippy --features jit --all-targets`. The JIT is
  additive and default-OFF, so the bare and default builds MUST be byte-for-byte the pre-JIT engine.
- Four/five-way byte-identity (`tree-walker == generic-VM == specialized-VM == JIT-VM`, all differential
  modes, both feature configs) — **fix the codegen, never the assertion** (`goal-brief.md` Gate 1).
- Never hold a `RefCell`/`resources` borrow across `.await` (Gate 4); the JIT compile is synchronous and
  on the current-thread `!Send` runtime — do NOT introduce `Send` bounds or a background compile thread (v1).

---

## Task 0 — THE GATE (blocking; no other task starts until this passes)
**Files:** none in `src/` — a recorded decision + a measurement report under `bench/` and a note appended
to the design spec. **This is a precondition audit, not implementation.** Per spec §0/§1.1 + `goal-brief.md`.
- [ ] **Precondition 1 — NUM merged.** Verify `Value::Int(i64)` exists in `src/value.rs` and
  `ArithKind::Int` exists in `src/vm/adapt.rs:49` (the integer feedback the JIT's checked-int codegen
  consumes). If absent → STOP; the JIT over boxed `f64` has a low ceiling (spec §1, §1.1 row 1).
- [ ] **Precondition 2 — VAL merged.** Verify `Value` is ≤16 bytes and scalars are unboxed/inlined
  (`static_assertions::assert_eq_size!`/`const_assert!` on `size_of::<Value>()`; 8-byte NaN-box OR the
  sanctioned 16-byte niche fallback both satisfy this — full NaN-boxing is a bonus, NOT required, spec
  §1.1 row 2). If `Value` is still the fat enum + `Rc`, native code has nothing to bite on → STOP.
- [ ] **Precondition 3 — profiling evidence.** Produce a report (sibling to `bench/PROFILING_RESULTS.md`)
  showing interpreter **dispatch** — NOT allocation/`Cc` refcount/GC/I/O — dominates a representative
  CPU-bound corpus, and estimate the headroom a baseline JIT could recover. If allocation/GC dominate →
  STOP (VAL/GC is the lever, not the JIT; spec §1.1 row 3, §7).
- [ ] **Record the go/no-go.** Append an owner-noted decision to the design spec §1.1 (date, who, which
  preconditions passed, the measured dispatch share). A "no-go" is a valid, documented outcome — the plan
  ends here. A "go" unblocks Task 1 and authorizes cutting `feat/baseline-jit`.
- [ ] Independent review CONFIRMS each precondition by re-running the checks (not by trusting the report).
  No commit (no code) — the artifact is the recorded decision + the `bench/` report.

> **Everything below is conditional on Task 0 = GO.**

## Task 1 — Cargo `jit` feature + Cranelift deps (default-OFF) + zero-cost-when-off proof
**Files:** `Cargo.toml`. **Tests:** build-matrix (no new `src/` yet).
- [ ] Add a `jit = ["dep:cranelift-jit", "dep:cranelift-frontend", "dep:cranelift-codegen"]` feature to
  `Cargo.toml:106` `[features]`; the three crates as `optional = true` deps (pure-Rust, no LLVM — same
  posture as `stacker`/`gcmodule`, `Cargo.toml:28,34`). **Do NOT add `jit` to `default`** (`Cargo.toml:107`).
- [ ] **Failing/guard check:** `cargo build` and `cargo build --no-default-features` link ZERO Cranelift
  symbols (the feature is off → the crates are not compiled in). `cargo build --features jit` pulls them.
  Confirm `cargo tree` shows cranelift only under `--features jit`.
- [ ] Green all three configs; clippy clean. Review confirms default-off + the bare build unchanged. Commit.

## Task 2 — Tier-up counters: cheap `Cell<u32>` home, gated, zero-cost-when-off benchmark
**Files:** `src/vm/chunk.rs` (`FnProto`), `src/vm/run.rs` (`run_loop:1253`, `Op::Loop:1418`),
`src/vm/jit/counters.rs` (new). **Tests:** `chunk.rs` unit + a `bench/` microbench. Spec §2.1.
- [ ] **Failing tests:** with `jit` on, after N calls to a fn its `FnProto.jit_call_count == N`; after a
  hot loop its `jit_backedge_count` counts backedges; crossing `JIT_THRESHOLD` flips `JitState` to
  `Queued`. With `jit` OFF, the counter fields and increment sites **do not exist** (compile-out).
- [ ] Add `#[cfg(feature="jit")] pub jit_call_count: Cell<u32>` + `jit_backedge_count: Cell<u32>` ON
  `FnProto` (`chunk.rs:335`) — a **side datum beside `Chunk`, NOT inline in `Chunk.code`** (the
  disassembler/goldens/oracle depend on byte-identical bytecode; follow the `adapt.rs` "side map, not
  in-place quickening" precedent). The call bump at the frame-push site (`run.rs:1253`), reached via the
  live `closure.proto` (`value_ext.rs:23`); the backedge bump at `Op::Loop` (`run.rs:1418`) via the same
  proto — a **direct field bump, NOT a `RefCell<HashMap>` probe per backedge** (spec §2.1). Both increment
  sites and both fields are `#[cfg(feature="jit")]`-gated, so the JIT-off interpreter is byte-for-byte the
  pre-JIT loop.
- [ ] **Mandatory zero-cost-when-off benchmark** (the SP8 / Gate-12 bar): a `bench/` microbench on the
  call-heavy AND loop-heavy corpus proving NO steady-state regression for (i) `jit` OFF vs the pre-JIT
  build, and (ii) `jit` ON-but-COLD (counters incrementing, nothing compiled) vs `jit` OFF. If the cold
  bump shows in steady state, the home is wrong — fix the home, do not relax the gate (spec §2.1).
- [ ] Green all three configs; clippy. Review re-runs the bench + greps for any `RefCell`/`HashMap` on the
  backedge path. Commit.

## Task 3 — Per-isolate JIT context, state machine, code cache (no codegen yet)
**Files:** `src/vm/jit/{mod.rs,cache.rs}` (new), `src/vm/run.rs` (`Vm:51`, `with_specialize:175`).
**Tests:** `jit/cache.rs` unit. Spec §6.1, §2.1, §2.3.
- [ ] **Failing tests:** a `Vm` built with `jit` enabled has `Some(JitContext)`; one built without has
  `None` (mirrors `Vm.specialize`, `run.rs:104`). `JitContext` maps `Rc<FnProto>` identity → `JitState`
  (`Cold/Warming/Queued/Compiled{code_ptr}/Blacklisted`); a dropped `Vm` drops its code cache
  (deterministic reclamation, like every isolate-local resource).
- [ ] Add `#[cfg(feature="jit")] jit: Option<JitContext>` to `Vm` (`run.rs:51`), constructed in
  `with_specialize` (`run.rs:175`). `JitContext` owns a `cranelift_jit::JITModule` + the per-`FnProto`
  side table (state + installed `code_ptr`) keyed by `Rc::as_ptr` identity. **Per-isolate, never shared**
  (the runtime is `!Send`; `JITModule` owns its code memory; a `worker fn` hot in three isolates compiles
  three times — spec §6.1). State machine transitions only (no Cranelift codegen yet — that is Task 5).
- [ ] Green all three configs; clippy. Review confirms `!Send` preserved (`assert_not_impl_any!` style),
  drop reclaims the cache. Commit.

## Task 4 — Tier-up trigger + synchronous compile dispatch (stub codegen → bail)
**Files:** `src/vm/run.rs` (call dispatch at `Op::Call`/`Op::CallMethod`), `src/vm/jit/mod.rs`.
**Tests:** `vm_differential.rs` (must stay green). Spec §2.2, §2.3, §3.4.
- [ ] **Failing tests:** crossing `JIT_THRESHOLD` on a call enqueues then synchronously compiles the
  `FnProto` on that call; with a STUB translator that bails on every body, the proto is **blacklisted**
  and runs on the interpreter forever after (spec §2.3, §3.4 prefer whole-function bail (b) for v1).
  Async/generator protos (`is_async`/`is_generator`, `chunk.rs:339-340`) are NEVER eligible.
- [ ] At the call dispatch site (`run.rs:1253` neighborhood): *if the callee `FnProto` has an installed
  `code_ptr`, enter native code; else push an interpreter frame as today.* The native entry and the
  interpreter push **apply the IDENTICAL `check_call_args`** (`FnProto.params`, `chunk.rs:358`) — the JIT
  does NOT get its own arg-checking convention. Compile is **synchronous, lazy, on the threshold-crossing
  call** (v1; background compile is a documented non-goal, spec §2.2, §8). Stub translator bails everything
  for now (Task 5 fills codegen).
- [ ] Three-way differential STILL green (nothing compiles yet → identical behavior). Green all three
  configs; clippy. Review. Commit.

## Task 5 — Cranelift translator: straight-line + branches + loops + calls + generic-helper arith
**Files:** `src/vm/jit/{translate.rs,codegen.rs}` (new). **Tests:** `vm_differential.rs` always-JIT mode.
Spec §3.1, §3.2, §3.4.
- [ ] **Failing tests (always-JIT mode, `JIT_THRESHOLD=0`):** a lowerable fn (straight-line + `if` +
  `while`/loop + calls, arithmetic routed to the SHARED `apply_binop` `interp.rs:5076`) produces
  byte-identical output to the tree-walker. An unsupported op (`Await`, `Yield`, `MakeGenerator`, `Import`,
  destructuring/match, native-resource touching) triggers **whole-function bail (b)** + blacklist.
- [ ] Walk `FnProto.chunk.code` once (reuse `Op::operand_width` `opcode.rs:623`); lower the stack machine
  to a Cranelift SSA builder (operand stack = a compile-time value stack the translator tracks — classic
  "abstract interpretation of the bytecode"). Lower control flow (branches/loops) to CLIF blocks. Lower
  EVERY arithmetic/member op CONSERVATIVELY for now to **a call into the same generic helper the
  interpreter falls through to** (`apply_binop`, generic member read) — the fast-path guards come in Tasks
  6-7. This makes correctness-first the baseline: every op is already "call the shared helper."
- [ ] **JIT-COVERAGE wired (spec §5.1):** always-JIT mode emits compiled-vs-bailed counts; this task's
  acceptance includes a NON-zero compiled fraction on a hand-picked lowerable example (proves native code
  actually ran, not a false-green second interpreter pass).
- [ ] Always-JIT differential green; clippy `--features jit`. Review probes a bail path + the coverage
  number. Commit.

## Task 6 — Checked-vs-wrapping integer codegen (NUM semantics) via shared panic path
**Files:** `src/vm/jit/codegen.rs`. **Tests:** `vm_differential.rs` (numeric edges). Spec §3.3.
- [ ] **Failing tests (always-JIT):** an `ArithKind::Int`-specialized site (`adapt.rs:49`, read via
  `arith_cache(op_off)` `chunk.rs:607`) emits native i64 `iadd`/`isub`/`imul` **with an overflow check**;
  on overflow it **branches to the shared panic-raising path** producing the byte-identical recoverable
  Tier-2 `[value, err]` (`integer overflow in '<op>'`, same span) — NOT a CLIF `trap`, NOT a silent
  wrapping `add` (Gate 6 / spec §3.3). Wrapping ops (`+% -% *%`) emit plain wrapping native instructions.
  `int / 0`, `% 0`, shift ≥64/<0, mixed-type promotion → guard + call the generic path (or branch to the
  same shared panic helper). A `Number`(float)-specialized site emits native `f64` behind a float guard;
  unspecialized/polymorphic → call generic `apply_binop`.
- [ ] Implement the guarded native arithmetic: tag/`is-int` test on both operands → native checked op;
  guard miss → mechanism (a) call to `apply_binop` (Task 8 formalizes deopt). The overflow branch and the
  div0/shift/promotion branches are all **conditional-branch → shared-helper call**, never a raw `trap`,
  never silent UB (spec §3.3 is explicit and load-bearing).
- [ ] FUZZ-relevant numeric edges (overflow boundary, div-by-zero, 2^53 boundary, wrapping-vs-checked)
  byte-identical across all four engines. Always-JIT differential green; clippy. Review. Commit.

## Task 7 — IC/global feedback codegen: shape-guarded slot load, method dispatch, global cache
**Files:** `src/vm/jit/codegen.rs`. **Tests:** `vm_differential.rs` (IC-heavy). Spec §3.2.
- [ ] **Failing tests (always-JIT):** a `GET_PROP`/`SET_PROP` site that is `InlineCache::Mono{shape,index}`
  (`ic.rs:60`) emits a `shape_id`-guard + direct `values.get_index(index)` (the exact fast path
  `run.rs` takes, inlined); on guard miss → call the generic member read. `Poly` (≤`POLY_MAX`, `ic.rs:48`)
  → a small guarded scan; `Mega` → generic. `MethodCache::Mono` (`ic.rs:147`) → class-identity guard +
  direct compiled-closure entry. A `GlobalCache::Cached`/`IndexBound` (`adapt.rs:164`) → guarded direct
  read; the `struct_gen`/`version` guard → native compare against the live counter.
- [ ] Implement the guarded lowerings; every guard-miss path is a mechanism-(a) call to the SAME generic
  helper the interpreter uses (symmetry is what makes byte-identity achievable, spec §3.2).
- [ ] IC/method/global differential (always-JIT) byte-identical; clippy. Review probes a Poly/Mega
  fall-through. Commit.

## Task 8 — Deopt: mechanism (a) per-op generic-helper + mechanism (b) entry-only re-dispatch
**Files:** `src/vm/jit/deopt.rs` (new), `src/vm/jit/translate.rs`. **Tests:** `vm_differential.rs`
always-deopt mode. Spec §4 (the hard part).
- [ ] **Failing tests:** **mechanism (a)** — a guard miss inside a compiled body calls the SAME shared
  generic helper the interpreter's deopt arm calls (`run.rs:3798-3803` → `apply_binop`) and **continues in
  native code** with the canonical result; this is sound REGARDLESS of prior side effects (no entry
  window). **Mechanism (b)** — a not-yet-lowerable op encountered while the frame has produced NO
  observable effect re-executes the WHOLE call on tier-0 with the original args; entry-only.
- [ ] Implement the **load-bearing invariant** (spec §4.1): the translator tracks one "has the frame
  produced an observable effect yet?" bit. While false, an awkward op MAY resolve by entry re-dispatch (b);
  once true, EVERY remaining guard MUST lower to mechanism (a) only (never a whole-frame re-dispatch).
  **Supporting invariant (stack discipline):** the guarded fast path and its generic-helper substitute MUST
  leave the compile-time operand stack at identical height/shape — assert this per guard site at compile
  time (a mismatch is a translator bug). **NO mid-function OSR** — no operand-stack/locals reconstruction
  metadata (spec §4.1, §4.2, §8). Minimal deopt metadata only: the tier-0 `FnProto` + per-guard "which
  helper / re-dispatch" flag.
- [ ] **Recursion-depth parity (spec §4.3):** the JIT native entry performs the SAME single
  `Interp.call_depth` increment/decrement per call (`enter_frame_depth` `run.rs:299`, `MAX_CALL_DEPTH`) so
  `maximum recursion depth exceeded` fires byte-identically. The JIT does not skip the guard.
- [ ] **always-deopt mode** (`ASCRIPT_JIT_STRESS_DEOPT`): every guard forced to miss → exercises (a)/(b) on
  every site; differential byte-identical. Clippy. Review probes the effect-bit transition + a recursive fn
  hitting the depth cap under JIT. Commit.

## Task 9 — THE GATE: four/five-way differential + coverage assertion + COMBINED mode
**Files:** `tests/vm_differential.rs`, `src/lib.rs` (a `vm_run_source_jit` entry point). **Tests:** the
whole-corpus differential. Spec §5.1 (THE non-negotiable gate). `goal-brief.md` Gate 1.
- [ ] Add a `vm_run_source_jit` entry (`lib.rs`, sibling to `vm_run_source`/`vm_run_source_generic`
  `:332/345`) running the JIT-VM. Extend the three-way assertion (`vm_differential.rs:1198-1204`) to the
  full chain: **tree-walker == generic-VM == specialized-VM == JIT-VM** over the ENTIRE example/golden
  corpus (respecting `EXAMPLE_SKIPS:797`), in BOTH feature configs (`.aso`-compiled is covered by whichever
  VM mode runs it).
- [ ] **Required differential MODES (each a distinct config, spec §5.1):** (1) **always-JIT**
  (`JIT_THRESHOLD=0`) — every eligible proto compiled; (2) **always-deopt** — every guard forced to miss;
  (3) **always-JIT + always-deopt COMBINED** (REQUIRED, not optional) — compile everything AND force every
  guard to miss, so the compiled prologue/calling-convention/entry path runs while every body op routes
  through the shared helper (catches a codegen bug that hides whenever either knob alone leaves a path cold).
- [ ] **MANDATORY JIT-COVERAGE assertion (the false-green trap, spec §5.1):** always-JIT mode emits
  `compiled / (compiled + bailed)` over the corpus and **FAILS if that fraction is ~0** (threshold=0 alone
  is false-green: with whole-function bail, one unsupported op leaves a proto on the interpreter, silently
  degenerating the "JIT differential" into a second interpreter run). The coverage number is reported
  alongside the differential result.
- [ ] The tree-walker is NEVER relaxed to match the JIT — a divergence is ALWAYS a JIT codegen/guard bug;
  fix the codegen (`goal-brief.md` Gate 1, the rule governing the existing specialized/generic split
  `run.rs:104`). All modes byte-identical, both configs; clippy. Independent review re-runs all three modes
  + inspects the coverage number. Commit.

## Task 10 — FUZZ differential mode
**Files:** the FUZZ harness (whatever FUZZ landed), `tests/`. **Tests:** the differential fuzzer. Spec §5.2.
- [ ] Add the JIT-VM (always-JIT mode) as a mode of the FUZZ campaign's grammar-aware differential fuzzer:
  every fuzz-generated program runs on the JIT-VM and its output/panic MUST match the tree-walker. The
  numeric edges FUZZ already targets (overflow boundaries, div-by-zero, the 2^53 boundary, wrapping vs
  checked) are EXACTLY the codegen the JIT is most likely to get subtly wrong — the JIT must be a
  first-class fuzz target, not a manually-tested afterthought (spec §5.2).
- [ ] **Dependency note:** this task assumes FUZZ has merged (it is stood up alongside NUM and runs
  continuously, `goal-brief.md`). If FUZZ is not yet present at JIT-implementation time, record the wiring
  as a deferred follow-up against the FUZZ harness rather than inventing a parallel one.
- [ ] Fuzzer green with the JIT mode; clippy. Review. Commit.

## Task 11 — Bench report (honest, measured-not-promised)
**Files:** `bench/JIT_RESULTS.md` (new), bench scripts under `bench/`. Spec §7.
- [ ] Produce a `bench/` report (the `bench/PROFILING_RESULTS.md`/`WORKERS_RESULTS.md` convention)
  quantifying: speedup on the numeric/loop corpus; the (non-)effect on allocation-bound code; synchronous
  compile latency (compile time vs recovered runtime) and the break-even hotness threshold; and the
  cold-counter no-regression result (Task 2) restated. **No speedup number is promised** — the report
  states what was measured (spec §7).
- [ ] **Honest-ceiling framing (REQUIRED, spec §7):** state explicitly that even post-NUM/VAL the
  `Rc`/`Cc` + cycle-GC value model + gradual runtime contracts cap gains; VAL unboxes only SCALARS — every
  heap kind (`Str`/`Array`/`Object`/`Map`/`Instance`) keeps its `Cc` payload in BOTH VAL outcomes, so the
  JIT cannot remove refcount traffic either way (under the 16-byte niche fallback the ceiling is lower
  still — one extra indirection vs full NaN-box). AScript's JIT is a baseline interpreter accelerator, NOT
  a path to C/Java-class throughput. Over-promising here is the failure mode.
- [ ] Review confirms the report is measured-not-promised and the ceiling framing is present. Commit.

## Task 12 — Docs
**Files:** `CLAUDE.md` (a JIT/tiers paragraph), the design spec (un-defer note if go), `goal.md`/roadmap
entry, `docs/content/` only if a user-visible flag is exposed. Gate 11.
- [ ] CLAUDE.md: a "Larger subsystems" paragraph for the JIT tier (default-off `jit` feature; tier-0
  interpreter is the permanent oracle + deopt target; the four/five-way differential adds JIT-VM;
  per-isolate code cache; no `.aso` change). Record the Task-0 go decision in the design spec. Roadmap
  entry. If a `--jit`/`ASCRIPT_JIT_*` CLI surface is exposed, document it (and add to NAV if a new page —
  the orphan gotcha). NO new user-facing language semantics (JIT is invisible).
- [ ] Review; commit.

## Done when
Task 0 recorded a GO; the `feat/baseline-jit` branch is behind the default-off `jit` feature; the bare and
default builds are byte-for-byte the pre-JIT engine (zero-cost-when-off benched, Gate 10); the
four/five-way byte-identity (`tree-walker == generic-VM == specialized-VM == JIT-VM`) holds across
always-JIT, always-deopt, and the COMBINED mode in BOTH feature configs, with the JIT-COVERAGE assertion
proving native code actually ran; the JIT is a first-class FUZZ target; NUM checked/wrapping semantics are
preserved via the shared panic path (never a CLIF `trap`, never silent wrap — Gate 6); deopt is
entry-only + per-op generic-helper (no mid-function OSR); recursion-depth parity holds; `ASO_FORMAT_VERSION`
is untouched; clippy + tests green in all three configs; the `bench/` report is measured-not-promised with
the honest-ceiling framing. **Merge `--no-ff` to `main` ONLY if Task 0 = GO** — otherwise this plan closes
unmerged as a recorded no-go.

## Open questions / explicitly out of scope (documented non-goals, spec §8)
- **Mid-function OSR** — out of scope for v1 (the fragile deopt-metadata problem; entry-only deopt is
  provably correct and tractable). A future OSR effort would add a per-safepoint register/stack→bytecode map.
- **Async/generator function JIT** — `is_async`/`is_generator` protos stay on the interpreter (they suspend
  across `.await`; the M17 async non-goals forbid continuation capture).
- **Background/async compilation** — v1 is synchronous; off-thread compile is gated on compile-latency
  evidence and complicated by `!Send` + `JITModule` ownership.
- **Persistent on-disk native cache / AOT-to-native** — the `.aso` stays bytecode; native artifacts are
  BIN's concern.
- **An optimizing tier-2** — out of scope; baseline only.
- **OPEN: JIT_THRESHOLD tuning** — V8 Sparkplug tiers very eagerly, LuaJIT/HotSpot far less; the value is
  empirical against the Task-0 profiling corpus (start conservative). Not resolvable until Task 0 produces
  the corpus.
